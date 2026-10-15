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
use crate::config::VmConfig;
use crate::network::NetworkManager;

pub struct CloudHypervisorBackend;

impl CloudHypervisorBackend {
    fn find_binary(config: &VmConfig) -> Result<PathBuf> {
        if let Some(p) = &config.hypervisor_path {
            if p.exists() {
                return Ok(p.clone());
            }
        }

        let candidates = [
            PathBuf::from("/usr/local/bin/cloud-hypervisor-static"),
            PathBuf::from("/usr/local/bin/cloud-hypervisor"),
            PathBuf::from("/usr/bin/cloud-hypervisor"),
            PathBuf::from("/usr/bin/cloud-hypervisor-static"),
        ];

        for c in &candidates {
            if c.exists() {
                return Ok(c.clone());
            }
        }

        if let Ok(path) = which::which("cloud-hypervisor") {
            return Ok(path);
        }

        Err(anyhow!(
            "Cloud-Hypervisor binary not found. Please install it or specify 'hypervisor_path'."
        ))
    }
}

mod which {
    use std::path::PathBuf;
    pub fn which(name: &str) -> Result<PathBuf, ()> {
        if let Ok(paths) = std::env::var("PATH") {
            for p in std::env::split_paths(&paths) {
                let full = p.join(name);
                if full.is_file() {
                    return Ok(full);
                }
            }
        }
        Err(())
    }
}

impl HypervisorBackend for CloudHypervisorBackend {
    fn run(&self, config: &VmConfig) -> Result<()> {
        fs::create_dir_all("/run/mvmctl")
            .context("Failed to create /run/mvmctl directory")?;

        let bin_path = Self::find_binary(config)?;

        let kernel = config
            .boot
            .kernel
            .as_ref()
            .ok_or_else(|| anyhow!("Cloud-Hypervisor requires 'boot.kernel' to be specified."))?;

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

        let prep_nets = NetworkManager::prepare_networks(
            &config.name,
            &config.backend,
            &config.networks,
        )?;

        let mut cmd = Command::new(&bin_path);
        cmd.arg("--cpus").arg(format!("boot={}", config.cpus));
        cmd.arg("--memory").arg(format!("size={}", config.memory));
        cmd.arg("--kernel").arg(kernel);

        let cmdline = config
            .boot
            .cmdline
            .as_deref()
            .unwrap_or("console=ttyS0 root=/dev/vda rw rootwait");
        cmd.arg("--cmdline").arg(cmdline);

        let api_socket = format!("/run/mvmctl/{}-api.sock", config.name);
        cmd.arg("--api-socket").arg(format!("path={}", api_socket));

        if let Some(initrd) = &config.boot.initrd {
            cmd.arg("--initramfs").arg(initrd);
        }

        for disk in &config.disks {
            let direct_str = if disk.direct { "on" } else { "off" };
            let disk_arg = format!(
                "path={},image_type={},direct={},num_queues=2,queue_size=256",
                disk.path.display(),
                disk.format,
                direct_str
            );
            cmd.arg("--disk").arg(disk_arg);
        }

        // Serial configuration
        if let Some(ser) = &config.serial {
            match ser.mode.as_deref() {
                Some("socket") => {
                    let spath = ser
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(format!("/run/mvmctl/{}-serial.sock", config.name)));
                    cmd.arg("--serial").arg(format!("socket={}", spath.display()));
                }
                Some("file") => {
                    let spath = ser
                        .path
                        .clone()
                        .unwrap_or_else(|| PathBuf::from(format!("/var/log/mvmctl/{}.log", config.name)));
                    cmd.arg("--serial").arg(format!("file={}", spath.display()));
                }
                Some("tty") => {
                    cmd.arg("--serial").arg("tty");
                }
                Some("none") => {
                    cmd.arg("--serial").arg("off");
                }
                _ => {
                    let def_socket = format!("/run/mvmctl/{}-serial.sock", config.name);
                    cmd.arg("--serial").arg(format!("socket={}", def_socket));
                }
            }
        } else {
            let def_socket = format!("/run/mvmctl/{}-serial.sock", config.name);
            cmd.arg("--serial").arg(format!("socket={}", def_socket));
        }

        // Extra passthrough arguments
        for extra in &config.extra_args {
            cmd.arg(extra);
        }

        // Network arguments and inherited file descriptors
        let mut fd_mappings = Vec::new();
        let mut current_target_fd = 3i32;

        for net in &prep_nets {
            if net.mode == "macvtap" {
                let tap_raw = net.tap_file.as_ref().map(|f| f.as_raw_fd()).unwrap_or(-1);
                let tap_target = current_target_fd;
                current_target_fd += 1;
                fd_mappings.push((tap_raw, tap_target));

                if let Some(mac) = &net.mac {
                    cmd.arg("--net")
                        .arg(format!("fd={},mac={}", tap_target, mac));
                } else {
                    cmd.arg("--net").arg(format!("fd={}", tap_target));
                }
            } else if net.mode == "bridge" || net.mode == "tap" {
                if let Some(mac) = &net.mac {
                    cmd.arg("--net")
                        .arg(format!("tap={},mac={}", net.interface_name, mac));
                } else {
                    cmd.arg("--net")
                        .arg(format!("tap={}", net.interface_name));
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
                    let flags = libc::fcntl(target_fd, libc::F_GETFD);
                    if flags >= 0 {
                        libc::fcntl(target_fd, libc::F_SETFD, flags & !libc::FD_CLOEXEC);
                    }
                }
                Ok(())
            });
        }

        println!("Starting Cloud-Hypervisor VM '{}'...", config.name);
        let mut child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn Cloud-Hypervisor '{}'", bin_path.display()))?;

        let status = child.wait().context("Cloud-Hypervisor process failed")?;
        if !status.success() {
            eprintln!("Cloud-Hypervisor process exited with: {:?}", status);
        }

        Ok(())
    }

    fn stop(&self, config: &VmConfig) -> Result<()> {
        let sock_path = format!("/run/mvmctl/{}-api.sock", config.name);
        if !Path::new(&sock_path).exists() {
            println!("API socket '{}' not found, VM might already be stopped.", sock_path);
            return Ok(());
        }

        println!("Sending ACPI power-button to Cloud-Hypervisor VM '{}'...", config.name);
        let mut stream = UnixStream::connect(&sock_path)
            .with_context(|| format!("Failed to connect to API socket '{}'", sock_path))?;
        stream.set_read_timeout(Some(Duration::from_secs(3)))?;
        stream.set_write_timeout(Some(Duration::from_secs(3)))?;

        // HTTP PUT /api/v1/vm.power-button
        let req = "PUT /api/v1/vm.power-button HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\n\r\n";
        stream.write_all(req.as_bytes())?;
        stream.flush()?;

        let mut reader = BufReader::new(stream);
        let mut status_line = String::new();
        let _ = reader.read_line(&mut status_line);

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
        let api_sock = format!("/run/mvmctl/{}-api.sock", config.name);
        let ser_sock = format!("/run/mvmctl/{}-serial.sock", config.name);
        let _ = fs::remove_file(api_sock);
        let _ = fs::remove_file(ser_sock);
        Ok(())
    }
}
