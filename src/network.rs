use anyhow::{anyhow, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::Command;

use crate::config::{BackendType, NetworkConfig};

#[derive(Debug)]
pub struct PreparedNetwork {
    pub interface_name: String,
    pub mode: String,
    pub mac: Option<String>,
    pub tap_file: Option<File>,
    pub vhost_file: Option<File>,
}

pub struct NetworkManager;

impl NetworkManager {
    pub fn prepare_networks(
        vm_name: &str,
        backend: &BackendType,
        configs: &[NetworkConfig],
    ) -> Result<Vec<PreparedNetwork>> {
        let prefix = match backend {
            BackendType::Qemu => "qu-",
            BackendType::CloudHypervisor => "ch-",
        };

        let mut results = Vec::new();
        let total = configs.len();

        for (idx, cfg) in configs.iter().enumerate() {
            let ifname = match &cfg.name {
                Some(n) => n.clone(),
                None => {
                    if total == 1 {
                        format!("{}{}", prefix, vm_name)
                    } else {
                        format!("{}{}-{}", prefix, vm_name, idx)
                    }
                }
            };

            let mode = cfg.mode.to_lowercase();
            match mode.as_str() {
                "macvtap" => {
                    Self::setup_macvtap(&ifname, &cfg.parent, cfg.mac.as_deref())?;

                    let ifindex = Self::get_ifindex(&ifname)?;
                    let tap_dev = format!("/dev/tap{}", ifindex);
                    let tap_file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&tap_dev)
                        .with_context(|| format!("Failed to open tap device '{}'", tap_dev))?;

                    let vhost_file = if *backend == BackendType::Qemu {
                        OpenOptions::new()
                            .read(true)
                            .write(true)
                            .open("/dev/vhost-net")
                            .ok()
                    } else {
                        None
                    };

                    results.push(PreparedNetwork {
                        interface_name: ifname,
                        mode,
                        mac: cfg.mac.clone(),
                        tap_file: Some(tap_file),
                        vhost_file,
                    });
                }
                "bridge" | "tap" => {
                    Self::setup_bridge_tap(&ifname, &cfg.parent)?;
                    results.push(PreparedNetwork {
                        interface_name: ifname,
                        mode,
                        mac: cfg.mac.clone(),
                        tap_file: None,
                        vhost_file: None,
                    });
                }
                other => {
                    return Err(anyhow!(
                        "Unsupported network mode '{}' for interface '{}'",
                        other,
                        ifname
                    ));
                }
            }
        }

        Ok(results)
    }

    fn setup_macvtap(ifname: &str, parent: &str, mac: Option<&str>) -> Result<()> {
        let sys_path = format!("/sys/class/net/{}", ifname);
        if !Path::new(&sys_path).exists() {
            let mut cmd = Command::new("ip");
            cmd.args(["link", "add", "link", parent, "name", ifname]);
            if let Some(m) = mac {
                cmd.args(["address", m]);
            }
            cmd.args(["type", "macvtap", "mode", "bridge"]);

            let status = cmd
                .status()
                .with_context(|| format!("Failed to execute 'ip link add' for {}", ifname))?;
            if !status.success() {
                return Err(anyhow!("Failed to create macvtap device '{}'", ifname));
            }
        }

        let up_status = Command::new("ip")
            .args(["link", "set", ifname, "up"])
            .status()
            .with_context(|| format!("Failed to bring up {}", ifname))?;
        if !up_status.success() {
            return Err(anyhow!("Failed to bring up interface '{}'", ifname));
        }

        Ok(())
    }

    fn setup_bridge_tap(ifname: &str, parent: &str) -> Result<()> {
        let sys_path = format!("/sys/class/net/{}", ifname);
        if !Path::new(&sys_path).exists() {
            let status = Command::new("ip")
                .args(["tuntap", "add", ifname, "mode", "tap"])
                .status()
                .with_context(|| format!("Failed to create tap device {}", ifname))?;
            if !status.success() {
                return Err(anyhow!("Failed to create tap device '{}'", ifname));
            }

            let master_status = Command::new("ip")
                .args(["link", "set", ifname, "master", parent])
                .status()
                .with_context(|| format!("Failed to attach {} to bridge {}", ifname, parent))?;
            if !master_status.success() {
                return Err(anyhow!("Failed to attach '{}' to bridge '{}'", ifname, parent));
            }
        }

        let up_status = Command::new("ip")
            .args(["link", "set", ifname, "up"])
            .status()
            .with_context(|| format!("Failed to bring up {}", ifname))?;
        if !up_status.success() {
            return Err(anyhow!("Failed to bring up interface '{}'", ifname));
        }

        Ok(())
    }

    fn get_ifindex(ifname: &str) -> Result<u32> {
        let ifindex_path = format!("/sys/class/net/{}/ifindex", ifname);
        let s = fs::read_to_string(&ifindex_path)
            .with_context(|| format!("Failed to read ifindex for '{}'", ifname))?;
        let idx = s.trim().parse::<u32>().context("Invalid ifindex number")?;
        Ok(idx)
    }

    pub fn cleanup_networks(vm_name: &str, backend: &BackendType) -> Result<()> {
        let prefixes = match backend {
            BackendType::Qemu => vec!["qu-"],
            BackendType::CloudHypervisor => vec!["ch-"],
        };

        if let Ok(entries) = fs::read_dir("/sys/class/net") {
            for entry in entries.flatten() {
                let name = entry.file_name().to_string_lossy().to_string();
                for prefix in &prefixes {
                    let exact = format!("{}{}", prefix, vm_name);
                    let indexed_prefix = format!("{}{}-", prefix, vm_name);
                    if name == exact || name.starts_with(&indexed_prefix) {
                        let _ = Command::new("ip").args(["link", "del", &name]).status();
                    }
                }
            }
        }
        Ok(())
    }
}
