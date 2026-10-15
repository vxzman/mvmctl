use anyhow::{anyhow, Context, Result};
use std::fs;
use std::os::unix::io::AsRawFd;
use std::os::unix::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

use crate::backend::{ensure_runtime_dir, ignore_sigint, lock_writable_disks, HypervisorBackend};
use crate::config::{BootMode, VmConfig};
use crate::control::{self, qmp_execute};
use crate::network::NetworkManager;
use crate::settings::{self, GlobalConfig, SerialTarget};

pub struct QemuBackend;

impl QemuBackend {
    fn find_binary(global: &GlobalConfig, config: &VmConfig) -> Result<PathBuf> {
        if let Some(path) = &config.hypervisor_path {
            if path.is_file() {
                return Ok(path.clone());
            }
            return Err(anyhow!("QEMU binary '{}' not found", path.display()));
        }
        if global.qemu.binary.is_file() {
            return Ok(global.qemu.binary.clone());
        }
        if let Some(path) = settings::which("qemu-system-x86_64") {
            return Ok(path);
        }
        Err(anyhow!(
            "QEMU binary '{}' not found. Install qemu-system-x86_64 or set qemu.binary in config.json",
            global.qemu.binary.display()
        ))
    }
}

fn ensure_uefi_vars(global: &GlobalConfig, config: &VmConfig) -> Result<(PathBuf, PathBuf)> {
    let code = config
        .boot
        .uefi_code
        .clone()
        .unwrap_or_else(|| global.qemu.uefi_code.clone());
    let vars = config.boot.uefi_vars.clone().unwrap_or_else(|| {
        global
            .qemu
            .nvram_dir
            .join(format!("{}_VARS.fd", config.name))
    });
    if !vars.exists() {
        if !global.qemu.uefi_vars_template.is_file() {
            return Err(anyhow!(
                "UEFI vars '{}' is missing and template '{}' was not found. Set qemu.uefi_vars_template in config.json",
                vars.display(),
                global.qemu.uefi_vars_template.display()
            ));
        }
        if let Some(parent) = vars.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("Failed to create '{}'", parent.display()))?;
        }
        fs::copy(&global.qemu.uefi_vars_template, &vars).with_context(|| {
            format!(
                "Failed to copy UEFI vars template '{}' to '{}'",
                global.qemu.uefi_vars_template.display(),
                vars.display()
            )
        })?;
    }
    Ok((code, vars))
}

impl HypervisorBackend for QemuBackend {
    fn run(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        ensure_runtime_dir(global)?;
        let bin_path = Self::find_binary(global, config)?;
        let disk_locks = lock_writable_disks(config)?;

        let (prep_nets, _net_guard) = NetworkManager::prepare_networks(global, config)?;

        let mut cmd = Command::new(&bin_path);
        cmd.arg("-name")
            .arg(format!("{},process={}", config.name, config.name));
        cmd.arg("-accel").arg(&global.qemu.accel);
        cmd.arg("-cpu").arg(&global.qemu.cpu);
        cmd.arg("-smp").arg(config.cpus.to_string());
        cmd.arg("-m").arg(&config.memory);
        cmd.arg("-machine").arg(&global.qemu.machine);
        cmd.arg("-global")
            .arg("driver=cfi.pflash01,property=secure,value=on");
        cmd.arg("-device").arg("virtio-rng-pci");
        cmd.arg("-device").arg("virtio-balloon-pci");

        let qmp_sock = global.control_socket(&config.backend, &config.name);
        cmd.arg("-qmp")
            .arg(format!("unix:{},server,nowait", qmp_sock.display()));
        cmd.arg("-display").arg("none");

        match settings::resolve_serial(global, config)? {
            SerialTarget::None => {}
            SerialTarget::Socket(path) => {
                cmd.arg("-serial")
                    .arg(format!("unix:{},server,nowait", path.display()));
            }
            SerialTarget::File(path) => {
                if let Some(parent) = path.parent() {
                    let _ = fs::create_dir_all(parent);
                }
                cmd.arg("-serial").arg(format!("file:{}", path.display()));
            }
            SerialTarget::Stdio => {
                cmd.arg("-serial").arg("mon:stdio");
            }
        }

        for disk in &config.disks {
            let mut opts = vec![
                format!("file={}", disk.path.display()),
                format!("format={}", disk.format),
                "if=virtio".to_string(),
                format!("cache={}", disk.cache),
                format!("aio={}", disk.aio),
                format!("discard={}", disk.discard),
            ];
            if disk.readonly {
                opts.push("readonly=on".to_string());
            }
            cmd.arg("-drive").arg(opts.join(","));
        }

        match config.boot.mode {
            BootMode::Uefi => {
                let (ovmf_code, ovmf_vars) = ensure_uefi_vars(global, config)?;
                cmd.arg("-drive").arg(format!(
                    "if=pflash,format=raw,unit=0,file={},readonly=on",
                    ovmf_code.display()
                ));
                cmd.arg("-drive").arg(format!(
                    "if=pflash,format=raw,unit=1,file={}",
                    ovmf_vars.display()
                ));
            }
            BootMode::Direct => {
                if let Some(kernel) = &config.boot.kernel {
                    cmd.arg("-kernel").arg(kernel);
                }
                if let Some(initrd) = &config.boot.initrd {
                    cmd.arg("-initrd").arg(initrd);
                }
                if let Some(cmdline) = &config.boot.cmdline {
                    cmd.arg("-append").arg(cmdline);
                }
            }
        }

        if let Some(sp) = &config.spice {
            if sp.enable {
                let port = sp.port.unwrap_or(5900);
                let mut sp_opt = format!("port={},addr={}", port, sp.addr);
                if let Some(pw) = &sp.password {
                    if !pw.is_empty() {
                        sp_opt.push_str(&format!(",password={pw}"));
                    } else {
                        sp_opt.push_str(",disable-ticketing=on");
                    }
                } else {
                    sp_opt.push_str(",disable-ticketing=on");
                }
                cmd.arg("-spice").arg(sp_opt);
                cmd.args([
                    "-device",
                    "virtio-vga",
                    "-device",
                    "qemu-xhci",
                    "-device",
                    "usb-tablet",
                    "-device",
                    "virtio-serial-pci",
                    "-chardev",
                    "spicevmc,id=vdagent,name=vdagent",
                    "-device",
                    "virtserialport,chardev=vdagent,name=com.redhat.spice.0",
                    "-audiodev",
                    "spice,id=snd0",
                    "-device",
                    "ich9-intel-hda",
                    "-device",
                    "hda-duplex,audiodev=snd0",
                ]);
            }
        }

        for extra in &config.extra_args {
            cmd.arg(extra);
        }

        let mut fd_mappings = Vec::new();
        let mut current_target_fd = 3i32;

        for (idx, net) in prep_nets.iter().enumerate() {
            if net.mode == "macvtap" {
                let tap_raw = net.tap_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
                let vhost_raw = net.vhost_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);

                let tap_target = current_target_fd;
                current_target_fd += 1;
                fd_mappings.push((tap_raw, tap_target));

                let vhost_arg = if vhost_raw >= 0 {
                    let vhost_target = current_target_fd;
                    current_target_fd += 1;
                    fd_mappings.push((vhost_raw, vhost_target));
                    format!(",vhost=on,vhostfd={vhost_target}")
                } else {
                    String::new()
                };

                cmd.arg("-netdev")
                    .arg(format!("tap,id=net{idx},fd={tap_target}{vhost_arg}"));

                if let Some(mac) = &net.mac {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{idx},mac={mac}"));
                } else {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{idx}"));
                }
            } else if net.mode == "bridge" || net.mode == "tap" {
                cmd.arg("-netdev").arg(format!(
                    "tap,id=net{idx},ifname={},script=no,downscript=no,vhost=on",
                    net.interface_name
                ));
                if let Some(mac) = &net.mac {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{idx},mac={mac}"));
                } else {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{idx}"));
                }
            }
        }

        unsafe {
            cmd.pre_exec(move || {
                // Child-only. QEMU creates the QMP and serial sockets with this mask.
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

        println!("Starting QEMU VM '{}'...", config.name);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn QEMU process '{}'", bin_path.display()))?;
        ignore_sigint();
        if let Ok(mode) = global.socket_mode_bits() {
            if let Ok(paths) = control::socket_paths(global, config) {
                control::tighten_sockets(paths, mode);
            }
        }

        let status = child
            .wait()
            .context("QEMU process failed during execution")?;
        drop(disk_locks);
        if !status.success() {
            return Err(anyhow!("QEMU exited with {status}"));
        }
        Ok(())
    }

    fn stop(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()> {
        let sock = global.control_socket(&config.backend, &config.name);
        if !sock.exists() {
            println!(
                "QMP socket '{}' not found, VM might already be stopped.",
                sock.display()
            );
            return Ok(());
        }

        println!(
            "Sending ACPI powerdown to QEMU VM '{}' via QMP...",
            config.name
        );
        qmp_execute(&sock, "system_powerdown")?;

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
