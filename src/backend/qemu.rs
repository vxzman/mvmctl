use anyhow::{anyhow, Context, Result};
use std::fs;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::io::AsRawFd;
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use crate::backend::HypervisorBackend;
use crate::config::{BootMode, VmConfig};
use crate::network::NetworkManager;

pub struct QemuBackend;

impl HypervisorBackend for QemuBackend {
    fn run(&self, config: &VmConfig) -> Result<()> {
        fs::create_dir_all("/run/mvmctl")
            .context("Failed to create /run/mvmctl directory")?;

        let bin_path = config
            .hypervisor_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("/usr/bin/qemu-system-x86_64"));

        if !bin_path.exists() {
            return Err(anyhow!(
                "QEMU binary '{}' not found. Please install qemu-system-x86_64 or specify 'hypervisor_path'",
                bin_path.display()
            ));
        }

        // Acquire exclusive kernel lock (flock) on writable disks to prevent accidental dual-mount
        let mut disk_locks = Vec::new();
        for disk in &config.disks {
            if !disk.readonly && disk.path.exists() {
                match fs::OpenOptions::new().read(true).write(true).open(&disk.path) {
                    Ok(file) => {
                        let fd = file.as_raw_fd();
                        if unsafe { libc::flock(fd, libc::LOCK_EX | libc::LOCK_NB) } != 0 {
                            return Err(anyhow!(
                                "Disk '{}' is currently locked by another process! Aborting startup to prevent filesystem corruption.",
                                disk.path.display()
                            ));
                        }
                        disk_locks.push(file);
                    }
                    Err(e) => {
                        eprintln!(
                            "Notice: Could not acquire flock on '{}': {}",
                            disk.path.display(),
                            e
                        );
                    }
                }
            }
        }

        // 1. Prepare networks
        let prep_nets = NetworkManager::prepare_networks(
            &config.name,
            &config.backend,
            &config.networks,
        )?;

        let mut cmd = Command::new(&bin_path);
        cmd.arg("-name").arg(format!("{},process={}", config.name, config.name));
        cmd.arg("-accel").arg("kvm");
        cmd.arg("-cpu").arg("host");
        cmd.arg("-smp").arg(config.cpus.to_string());
        cmd.arg("-m").arg(&config.memory);
        cmd.arg("-machine").arg("q35,smm=on");
        cmd.arg("-global").arg("driver=cfi.pflash01,property=secure,value=on");
        cmd.arg("-device").arg("virtio-rng-pci");
        cmd.arg("-device").arg("virtio-balloon-pci");

        let qmp_sock = format!("/run/mvmctl/{}-qmp.sock", config.name);
        cmd.arg("-qmp").arg(format!("unix:{},server,nowait", qmp_sock));
        cmd.arg("-display").arg("none");

        // Serial console
        if let Some(ser) = &config.serial {
            match ser.mode.as_deref() {
                Some("socket") => {
                    let spath = ser
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(format!("/run/mvmctl/{}-serial.sock", config.name)));
                    cmd.arg("-serial").arg(format!("unix:{},server,nowait", spath.display()));
                }
                Some("file") => {
                    let spath = ser
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(format!("/var/log/mvmctl/{}.log", config.name)));
                    cmd.arg("-serial").arg(format!("file:{}", spath.display()));
                }
                Some("none") => {}
                _ => {
                    cmd.arg("-serial").arg("mon:stdio");
                }
            }
        } else {
            cmd.arg("-serial").arg("mon:stdio");
        }

        // Disks
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

        // Boot mode
        match config.boot.mode {
            BootMode::Uefi => {
                let ovmf_code = config
                    .boot
                    .uefi_code
                    .clone()
                    .unwrap_or_else(|| PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.ms.fd"));
                let ovmf_vars = config
                    .boot
                    .uefi_vars
                    .clone()
                    .unwrap_or_else(|| PathBuf::from(format!("/var/lib/qemu/nvram/{}_VARS.fd", config.name)));

                if let Some(parent) = ovmf_vars.parent() {
                    let _ = fs::create_dir_all(parent);
                }

                cmd.arg("-drive")
                    .arg(format!("if=pflash,format=raw,unit=0,file={},readonly=on", ovmf_code.display()));
                cmd.arg("-drive")
                    .arg(format!("if=pflash,format=raw,unit=1,file={}", ovmf_vars.display()));
            }
            BootMode::Direct => {
                if let Some(k) = &config.boot.kernel {
                    cmd.arg("-kernel").arg(k);
                }
                if let Some(initrd) = &config.boot.initrd {
                    cmd.arg("-initrd").arg(initrd);
                }
                if let Some(cmdline) = &config.boot.cmdline {
                    cmd.arg("-append").arg(cmdline);
                }
            }
        }

        // SPICE display
        if let Some(sp) = &config.spice {
            if sp.enable {
                let port = sp.port.unwrap_or(5900);
                let mut sp_opt = format!("port={},addr={}", port, sp.addr);
                if let Some(pw) = &sp.password {
                    if !pw.is_empty() {
                        sp_opt.push_str(&format!(",password={}", pw));
                    } else {
                        sp_opt.push_str(",disable-ticketing=on");
                    }
                } else {
                    sp_opt.push_str(",disable-ticketing=on");
                }
                cmd.arg("-spice").arg(sp_opt);
                cmd.args([
                    "-device", "virtio-vga",
                    "-device", "qemu-xhci",
                    "-device", "usb-tablet",
                    "-device", "virtio-serial-pci",
                    "-chardev", "spicevmc,id=vdagent,name=vdagent",
                    "-device", "virtserialport,chardev=vdagent,name=com.redhat.spice.0",
                    "-audiodev", "spice,id=snd0",
                    "-device", "ich9-intel-hda",
                    "-device", "hda-duplex,audiodev=snd0",
                ]);
            }
        }

        // Extra passthrough arguments
        for extra in &config.extra_args {
            cmd.arg(extra);
        }

        // Networks mapping and file descriptors passing
        let mut fd_mappings = Vec::new();
        let mut current_target_fd = 3i32;

        for (idx, net) in prep_nets.iter().enumerate() {
            if net.mode == "macvtap" {
                let tap_raw = net.tap_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
                let vhost_raw = net.vhost_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);

                let tap_target = current_target_fd;
                current_target_fd += 1;
                fd_mappings.push((tap_raw, tap_target));

                let (vhost_arg, _vhost_target_opt) = if vhost_raw >= 0 {
                    let vhost_target = current_target_fd;
                    current_target_fd += 1;
                    fd_mappings.push((vhost_raw, vhost_target));
                    (format!(",vhost=on,vhostfd={}", vhost_target), Some(vhost_target))
                } else {
                    ("".to_string(), None)
                };

                cmd.arg("-netdev").arg(format!(
                    "tap,id=net{},fd={}{}",
                    idx, tap_target, vhost_arg
                ));

                if let Some(mac) = &net.mac {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{},mac={}", idx, mac));
                } else {
                    cmd.arg("-device").arg(format!("virtio-net-pci,netdev=net{}", idx));
                }
            } else if net.mode == "bridge" || net.mode == "tap" {
                cmd.arg("-netdev").arg(format!(
                    "tap,id=net{},ifname={},script=no,downscript=no,vhost=on",
                    idx, net.interface_name
                ));
                if let Some(mac) = &net.mac {
                    cmd.arg("-device")
                        .arg(format!("virtio-net-pci,netdev=net{},mac={}", idx, mac));
                } else {
                    cmd.arg("-device").arg(format!("virtio-net-pci,netdev=net{}", idx));
                }
            }
        }

        unsafe {
            cmd.pre_exec(move || {
                for &(src_fd, target_fd) in &fd_mappings {
                    if src_fd >= 0 && src_fd != target_fd {
                        if libc::dup2(src_fd, target_fd) < 0 {
                            return Err(std::io::Error::last_os_error());
                        }
                    }
                    // Clear close-on-exec
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

        let status = child.wait().context("QEMU process failed during execution")?;
        if !status.success() {
            eprintln!("QEMU process exited with: {:?}", status);
        }

        Ok(())
    }

    fn stop(&self, config: &VmConfig) -> Result<()> {
        let sock_path = format!("/run/mvmctl/{}-qmp.sock", config.name);
        if !Path::new(&sock_path).exists() {
            println!("QMP socket '{}' not found, VM might already be stopped.", sock_path);
            return Ok(());
        }

        println!("Sending ACPI powerdown to QEMU VM '{}' via QMP...", config.name);
        let mut stream = UnixStream::connect(&sock_path)
            .with_context(|| format!("Failed to connect to QMP socket '{}'", sock_path))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;

        let mut reader = BufReader::new(stream.try_clone()?);
        let mut greeting = String::new();
        let _ = reader.read_line(&mut greeting);

        // QMP handshake
        stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\n")?;
        stream.flush()?;
        let mut resp = String::new();
        let _ = reader.read_line(&mut resp);

        // Send system_powerdown
        stream.write_all(b"{\"execute\":\"system_powerdown\"}\n")?;
        stream.flush()?;

        for _ in 0..45 {
            if !Path::new(&sock_path).exists() {
                println!("VM '{}' stopped successfully.", config.name);
                return Ok(());
            }
            thread::sleep(Duration::from_secs(1));
        }

        println!("Warning: VM '{}' did not terminate within 45 seconds.", config.name);
        Ok(())
    }

    fn clean(&self, config: &VmConfig) -> Result<()> {
        println!("Cleaning up network interfaces and sockets for VM '{}'...", config.name);
        NetworkManager::cleanup_networks(&config.name, &config.backend)?;
        let qmp_sock = format!("/run/mvmctl/{}-qmp.sock", config.name);
        let ser_sock = format!("/run/mvmctl/{}-serial.sock", config.name);
        let _ = fs::remove_file(qmp_sock);
        let _ = fs::remove_file(ser_sock);
        Ok(())
    }
}
