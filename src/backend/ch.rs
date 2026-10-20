use anyhow::{anyhow, Context, Result};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::backend::{ensure_runtime_dir, ignore_sigint, lock_writable_disks, HypervisorBackend};
use crate::config::{BootMode, VmConfig};
use crate::control::{self, ch_request};
use crate::log;
use crate::network::NetworkManager;
use crate::settings::{self, firmware_path, GlobalConfig, SerialTarget};

pub struct CloudHypervisorBackend;

impl CloudHypervisorBackend {
    fn find_binary(global: &GlobalConfig, config: &VmConfig) -> Result<PathBuf> {
        if let Some(path) = &config.hypervisor_path {
            if path.is_file() {
                return Ok(path.clone());
            }
            return Err(anyhow!(
                "Cloud-Hypervisor binary '{}' not found",
                path.display()
            ));
        }
        for candidate in &global.cloud_hypervisor.binaries {
            if candidate.is_file() {
                return Ok(candidate.clone());
            }
        }
        if let Some(path) = settings::which("cloud-hypervisor") {
            return Ok(path);
        }
        Err(anyhow!(
            "Cloud-Hypervisor binary not found. Install it or set cloud_hypervisor.binaries in config.json"
        ))
    }
}

fn supervise_ch(
    cmd: &mut Command,
    bin_path: &Path,
    config: &VmConfig,
    global: &GlobalConfig,
) -> Result<()> {
    let mut restarts = Vec::new();
    loop {
        log::info(format!("starting cloud-hypervisor VM '{}'", config.name));
        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn Cloud-Hypervisor '{}'", bin_path.display())
        })?;
        ignore_sigint();
        if let Ok(mode) = global.socket_mode_bits() {
            if let Ok(paths) = control::socket_paths(global, config) {
                control::tighten_sockets(paths, mode);
            }
        }

        let signal = Arc::new(Mutex::new(None));
        let reader = log::spawn_stderr_pump(child.stderr.take(), "cloud-hypervisor", {
            let signal = Arc::clone(&signal);
            move |line| {
                if let Some(kind) = control::ch_lifecycle_signal(line) {
                    *signal.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(kind);
                }
            }
        });
        let status = child.wait().context("Cloud-Hypervisor process failed")?;
        let _ = reader.join();
        let last = *signal.lock().unwrap_or_else(|poison| poison.into_inner());

        match control::classify_ch_exit(last, status.success()) {
            control::VmExit::Stopped => return Ok(()),
            control::VmExit::Failed => {
                return Err(anyhow!("Cloud-Hypervisor exited with {status}"));
            }
            control::VmExit::Restart => {
                control::note_restart(&mut restarts)
                    .with_context(|| format!("VM '{}' reset too often", config.name))?;
                log::info(format!(
                    "cloud-hypervisor exited during guest reset; restarting VM '{}'",
                    config.name
                ));
                control::unlink_runtime_sockets(global, config);
            }
        }
    }
}

/// `vm.shutdown` leaves the daemon up. `vmm.shutdown` exits it.
fn force_ch_poweroff(sock: &Path, name: &str) -> Result<()> {
    if sock.exists() {
        if let Err(err) = ch_request(sock, "PUT", "/api/v1/vm.shutdown") {
            log::warn(format!(
                "cloud-hypervisor vm.shutdown failed for VM '{name}': {err:#}"
            ));
        }
    }
    if !sock.exists() {
        log::info(format!("VM '{name}' powered off"));
        return Ok(());
    }
    if let Err(err) = ch_request(sock, "PUT", "/api/v1/vmm.shutdown") {
        log::warn(format!(
            "cloud-hypervisor vmm.shutdown failed for VM '{name}': {err:#}"
        ));
    }
    if control::wait_for_removal(sock, Duration::from_secs(5)) {
        log::info(format!("VM '{name}' powered off"));
        return Ok(());
    }
    Err(anyhow!(
        "VM '{name}' is still running after forced power-off"
    ))
}

impl HypervisorBackend for CloudHypervisorBackend {
    fn run(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        ensure_runtime_dir(global)?;
        let bin_path = Self::find_binary(global, config)?;

        // Probe the lock, then release it. cloud-hypervisor >= v46 flocks the
        // disk itself and fails if this process still holds LOCK_EX.
        drop(lock_writable_disks(config)?);

        let (prep_nets, _net_guard) = NetworkManager::prepare_networks(global, config)?;

        let mut cmd = Command::new(&bin_path);
        cmd.arg("--cpus").arg(format!("boot={}", config.cpus));
        cmd.arg("--memory").arg(format!("size={}", config.memory));

        match config.boot.mode {
            BootMode::Direct => {
                let kernel = config.boot.kernel.as_ref().ok_or_else(|| {
                    anyhow!("Cloud-Hypervisor direct boot requires 'boot.kernel'")
                })?;
                cmd.arg("--kernel").arg(kernel);
                if let Some(initrd) = &config.boot.initrd {
                    cmd.arg("--initramfs").arg(initrd);
                }
                let cmdline = config
                    .boot
                    .cmdline
                    .as_deref()
                    .unwrap_or(global.cloud_hypervisor.default_cmdline.as_str());
                cmd.arg("--cmdline").arg(cmdline);
            }
            BootMode::Uefi => {
                let firmware = firmware_path(global, config).ok_or_else(|| {
                    anyhow!(
                        "Cloud-Hypervisor UEFI boot requires 'boot.uefi_code' or cloud_hypervisor.uefi_firmware"
                    )
                })?;
                cmd.arg("--firmware").arg(firmware);
            }
        }

        let api_socket = global.control_socket(&config.backend, &config.name);
        cmd.arg("--api-socket")
            .arg(format!("path={}", api_socket.display()));

        for disk in &config.disks {
            let mut parts = vec![
                format!("path={}", disk.path.display()),
                format!("direct={}", if disk.direct { "on" } else { "off" }),
                format!("num_queues={}", global.cloud_hypervisor.disk_num_queues),
                format!("queue_size={}", global.cloud_hypervisor.disk_queue_size),
            ];
            if global.cloud_hypervisor.pass_image_type {
                parts.insert(1, format!("image_type={}", disk.format));
            }
            if disk.readonly {
                parts.push("readonly=on".to_string());
            }
            cmd.arg("--disk").arg(parts.join(","));
        }

        match settings::resolve_serial(global, config)? {
            SerialTarget::None => {
                cmd.arg("--serial").arg("off");
            }
            SerialTarget::Socket(path) => {
                cmd.arg("--serial")
                    .arg(format!("socket={}", path.display()));
            }
            SerialTarget::File(path) => {
                if let Some(parent) = path.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                cmd.arg("--serial").arg(format!("file={}", path.display()));
            }
            SerialTarget::Stdio => {
                cmd.arg("--serial").arg("tty");
            }
        }

        for extra in &config.extra_args {
            cmd.arg(extra);
        }

        let mut fd_mappings = Vec::new();
        let mut current_target_fd = 3i32;

        for net in &prep_nets {
            if net.mode == "macvtap" {
                let tap_raw = net.tap_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
                let tap_target = current_target_fd;
                current_target_fd += 1;
                fd_mappings.push((tap_raw, tap_target));

                if let Some(mac) = &net.mac {
                    cmd.arg("--net").arg(format!("fd={tap_target},mac={mac}"));
                } else {
                    cmd.arg("--net").arg(format!("fd={tap_target}"));
                }
            } else if net.mode == "bridge" || net.mode == "tap" {
                if let Some(mac) = &net.mac {
                    cmd.arg("--net")
                        .arg(format!("tap={},mac={}", net.interface_name, mac));
                } else {
                    cmd.arg("--net").arg(format!("tap={}", net.interface_name));
                }
            }
        }

        unsafe {
            cmd.pre_exec(move || {
                // Runs in the forked child before exec. SIG_DFL undoes the parent's
                // SIGINT ignore so a restarted process still dies on Ctrl+C.
                libc::signal(libc::SIGINT, libc::SIG_DFL);
                libc::umask(0o077);
                for &(src_fd, target_fd) in &fd_mappings {
                    if src_fd >= 0 && src_fd != target_fd && libc::dup2(src_fd, target_fd) < 0 {
                        return Err(std::io::Error::last_os_error());
                    }
                    let flags = libc::fcntl(target_fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }

        cmd.stderr(Stdio::piped());
        supervise_ch(&mut cmd, &bin_path, config, global)
    }

    fn stop(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        let sock = global.control_socket(&config.backend, &config.name);
        if !sock.exists() {
            log::info(format!(
                "API socket '{}' not found, VM might already be stopped",
                sock.display()
            ));
            return Ok(());
        }

        log::info(format!(
            "sending ACPI power-button to cloud-hypervisor VM '{}'",
            config.name
        ));
        match ch_request(&sock, "PUT", "/api/v1/vm.power-button") {
            Ok((code, _)) if (200..300).contains(&code) => {
                let timeout = Duration::from_secs(global.stop_timeout_secs);
                if control::wait_for_removal(&sock, timeout) {
                    log::info(format!("VM '{}' stopped", config.name));
                    return Ok(());
                }
                log::warn(format!(
                    "VM '{}' did not stop within {} seconds; forcing power-off",
                    config.name, global.stop_timeout_secs
                ));
            }
            Ok((code, body)) => {
                log::warn(format!(
                    "cloud-hypervisor power-button returned HTTP {code}: {body}; forcing power-off"
                ));
            }
            Err(err) => {
                log::warn(format!(
                    "ACPI power-button failed for VM '{}': {err:#}; forcing power-off",
                    config.name
                ));
            }
        }
        force_ch_poweroff(&sock, &config.name)
    }

    fn clean(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        log::info(format!(
            "cleaning network interfaces and sockets for VM '{}'",
            config.name
        ));
        NetworkManager::cleanup(global, config)
    }
}
