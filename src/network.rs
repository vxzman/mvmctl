use anyhow::{anyhow, Context, Result};
use std::fs::{self, File, OpenOptions};
use std::path::Path;
use std::process::Command;

use crate::config::{BackendType, VmConfig};
use crate::settings::{self, GlobalConfig};

#[derive(Debug)]
pub struct PreparedNetwork {
    pub interface_name: String,
    pub mode: String,
    pub mac: Option<String>,
    pub tap_file: Option<File>,
    pub vhost_file: Option<File>,
}

/// Deletes the interfaces it owns when dropped, including on setup failure.
pub struct NetworkCleanupGuard {
    names: Vec<String>,
}

impl NetworkCleanupGuard {
    pub fn new(names: Vec<String>) -> Self {
        Self { names }
    }
}

impl Drop for NetworkCleanupGuard {
    fn drop(&mut self) {
        delete_interfaces(&self.names);
    }
}

pub struct NetworkManager;

impl NetworkManager {
    pub fn prepare_networks(
        global: &GlobalConfig,
        config: &VmConfig,
    ) -> Result<(Vec<PreparedNetwork>, NetworkCleanupGuard)> {
        let mut results = Vec::new();
        let mut guard = NetworkCleanupGuard::new(Vec::new());
        let total = config.networks.len();

        for (idx, cfg) in config.networks.iter().enumerate() {
            let ifname = settings::interface_name(
                global,
                &config.backend,
                &config.name,
                cfg.name.as_deref(),
                idx,
                total,
            )?;
            let mode = cfg.mode.to_lowercase();
            match mode.as_str() {
                "macvtap" => {
                    Self::setup_macvtap(&ifname, &cfg.parent, cfg.mac.as_deref())?;
                    guard.names.push(ifname.clone());

                    let ifindex = Self::get_ifindex(&ifname)?;
                    let tap_dev = format!("/dev/tap{ifindex}");
                    let tap_file = OpenOptions::new()
                        .read(true)
                        .write(true)
                        .open(&tap_dev)
                        .with_context(|| format!("Failed to open tap device '{tap_dev}'"))?;

                    let vhost_file = if config.backend == BackendType::Qemu {
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
                    guard.names.push(ifname.clone());
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
                        "Unsupported network mode '{other}' for interface '{ifname}'"
                    ));
                }
            }
        }

        Ok((results, guard))
    }

    fn setup_macvtap(ifname: &str, parent: &str, mac: Option<&str>) -> Result<()> {
        let sys_path = format!("/sys/class/net/{ifname}");
        let created = !Path::new(&sys_path).exists();
        if created {
            let mut cmd = Command::new("ip");
            cmd.args(["link", "add", "link", parent, "name", ifname]);
            if let Some(m) = mac {
                cmd.args(["address", m]);
            }
            cmd.args(["type", "macvtap", "mode", "bridge"]);

            let status = cmd
                .status()
                .with_context(|| format!("Failed to execute 'ip link add' for {ifname}"))?;
            if !status.success() {
                return Err(anyhow!("Failed to create macvtap device '{ifname}'"));
            }
        }

        let up_status = Command::new("ip")
            .args(["link", "set", ifname, "up"])
            .status()
            .with_context(|| format!("Failed to bring up {ifname}"))?;
        if !up_status.success() {
            if created {
                delete_interfaces([ifname]);
            }
            return Err(anyhow!("Failed to bring up interface '{ifname}'"));
        }

        Ok(())
    }

    fn setup_bridge_tap(ifname: &str, parent: &str) -> Result<()> {
        let sys_path = format!("/sys/class/net/{ifname}");
        let created = !Path::new(&sys_path).exists();
        if created {
            let status = Command::new("ip")
                .args(["tuntap", "add", ifname, "mode", "tap"])
                .status()
                .with_context(|| format!("Failed to create tap device {ifname}"))?;
            if !status.success() {
                return Err(anyhow!("Failed to create tap device '{ifname}'"));
            }

            let master_status = Command::new("ip")
                .args(["link", "set", ifname, "master", parent])
                .status()
                .with_context(|| format!("Failed to attach {ifname} to bridge {parent}"))?;
            if !master_status.success() {
                delete_interfaces([ifname]);
                return Err(anyhow!("Failed to attach '{ifname}' to bridge '{parent}'"));
            }
        }

        let up_status = Command::new("ip")
            .args(["link", "set", ifname, "up"])
            .status()
            .with_context(|| format!("Failed to bring up {ifname}"))?;
        if !up_status.success() {
            if created {
                delete_interfaces([ifname]);
            }
            return Err(anyhow!("Failed to bring up interface '{ifname}'"));
        }

        Ok(())
    }

    fn get_ifindex(ifname: &str) -> Result<u32> {
        let ifindex_path = format!("/sys/class/net/{ifname}/ifindex");
        let s = fs::read_to_string(&ifindex_path)
            .with_context(|| format!("Failed to read ifindex for '{ifname}'"))?;
        s.trim().parse::<u32>().context("Invalid ifindex number")
    }

    pub fn cleanup(global: &GlobalConfig, config: &VmConfig) -> Result<()> {
        let names = settings::interface_names(global, config)?;
        delete_interfaces(&names);

        let control = global.control_socket(&config.backend, &config.name);
        let _ = fs::remove_file(control);
        if let settings::SerialTarget::Socket(path) = settings::resolve_serial(global, config)? {
            let _ = fs::remove_file(path);
        }
        Ok(())
    }
}

pub fn delete_interfaces<I, S>(names: I)
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    for name in names {
        let _ = Command::new("ip")
            .args(["link", "del", name.as_ref()])
            .status();
    }
}
