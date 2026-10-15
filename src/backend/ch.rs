use anyhow::{anyhow, Context, Result};
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::backend::{ensure_runtime_dir, ignore_sigint, lock_writable_disks, HypervisorBackend};
use crate::config::{BootMode, VmConfig};
use crate::control::{self, ch_request};
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

        println!("Starting Cloud-Hypervisor VM '{}'...", config.name);
        let mut child = cmd.spawn().with_context(|| {
            format!("Failed to spawn Cloud-Hypervisor '{}'", bin_path.display())
        })?;
        ignore_sigint();
        if let Ok(mode) = global.socket_mode_bits() {
            if let Ok(paths) = control::socket_paths(global, config) {
                control::tighten_sockets(paths, mode);
            }
        }

        let status = child.wait().context("Cloud-Hypervisor process failed")?;
        if !status.success() {
            return Err(anyhow!("Cloud-Hypervisor exited with {status}"));
        }
        Ok(())
    }

    fn stop(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        let sock = global.control_socket(&config.backend, &config.name);
        if !sock.exists() {
            println!(
                "API socket '{}' not found, VM might already be stopped.",
                sock.display()
            );
            return Ok(());
        }

        println!(
            "Sending ACPI power-button to Cloud-Hypervisor VM '{}'...",
            config.name
        );
        let (code, body) = ch_request(&sock, "PUT", "/api/v1/vm.power-button")?;
        if !(200..300).contains(&code) {
            return Err(anyhow!(
                "cloud-hypervisor power-button returned HTTP {code}: {body}"
            ));
        }

        let timeout = Duration::from_secs(global.stop_timeout_secs);
        if control::wait_for_removal(&sock, timeout) {
            println!("VM '{}' stopped successfully.", config.name);
            return Ok(());
        }
        Err(anyhow!(
            "VM '{}' did not terminate within {} seconds",
            config.name,
            global.stop_timeout_secs
        ))
    }

    fn clean(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        println!(
            "Cleaning up network interfaces and sockets for VM '{}'...",
            config.name
        );
        NetworkManager::cleanup(global, config)
    }
}
