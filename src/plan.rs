use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::config::{BackendType, BootConfig, SerialConfig, SpiceConfig, VmConfig};
use crate::registry::ResourceRegistry;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedNetwork {
    pub mode: String,
    pub parent: String,
    pub mac: Option<String>,
    pub interface_name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedDisk {
    pub source_path: PathBuf,
    pub canonical_path: PathBuf,
    pub format: String,
    pub direct: bool,
    pub cache: String,
    pub aio: String,
    pub discard: String,
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResolvedPlan {
    pub name: String,
    pub description: String,
    pub backend: BackendType,
    pub hypervisor_path: Option<PathBuf>,
    pub cpus: u32,
    pub memory: String,
    pub autostart: bool,
    pub boot: BootConfig,
    pub disks: Vec<ResolvedDisk>,
    pub networks: Vec<ResolvedNetwork>,
    pub spice: Option<SpiceConfig>,
    pub serial: Option<SerialConfig>,
    pub extra_args: Vec<String>,
    pub qmp_or_api_socket: PathBuf,
    pub serial_socket: Option<PathBuf>,
}

impl ResolvedPlan {
    pub fn compile(config: &VmConfig) -> Self {
        let prefix = match config.backend {
            BackendType::Qemu => "qu-",
            BackendType::CloudHypervisor => "ch-",
        };
        let total_nets = config.networks.len();

        let resolved_networks: Vec<ResolvedNetwork> = config
            .networks
            .iter()
            .enumerate()
            .map(|(idx, net)| {
                let ifname = match &net.name {
                    Some(n) => n.clone(),
                    None => {
                        if total_nets == 1 {
                            format!("{}{}", prefix, config.name)
                        } else {
                            format!("{}{}-{}", prefix, config.name, idx)
                        }
                    }
                };
                ResolvedNetwork {
                    mode: net.mode.clone(),
                    parent: net.parent.clone(),
                    mac: net.mac.as_ref().map(|m| m.to_lowercase()),
                    interface_name: ifname,
                }
            })
            .collect();

        let resolved_disks: Vec<ResolvedDisk> = config
            .disks
            .iter()
            .map(|d| ResolvedDisk {
                source_path: d.path.clone(),
                canonical_path: ResourceRegistry::canonicalize_path(&d.path),
                format: d.format.clone(),
                direct: d.direct,
                cache: d.cache.clone(),
                aio: d.aio.clone(),
                discard: d.discard.clone(),
                readonly: d.readonly,
            })
            .collect();

        let (qmp_or_api, serial_sock) = match config.backend {
            BackendType::Qemu => {
                let qmp = PathBuf::from(format!("/run/mvmctl/{}-qmp.sock", config.name));
                let ser = config
                    .serial
                    .as_ref()
                    .and_then(|s| s.path.clone())
                    .or_else(|| Some(PathBuf::from(format!("/run/mvmctl/{}-serial.sock", config.name))));
                (qmp, ser)
            }
            BackendType::CloudHypervisor => {
                let api = PathBuf::from(format!("/run/mvmctl/{}-api.sock", config.name));
                let ser = config
                    .serial
                    .as_ref()
                    .and_then(|s| s.path.clone())
                    .or_else(|| Some(PathBuf::from(format!("/run/mvmctl/{}-serial.sock", config.name))));
                (api, ser)
            }
        };

        ResolvedPlan {
            name: config.name.clone(),
            description: config
                .description
                .clone()
                .unwrap_or_else(|| format!("MicroVM {} ({})", config.name, config.backend)),
            backend: config.backend.clone(),
            hypervisor_path: config.hypervisor_path.clone(),
            cpus: config.cpus,
            memory: config.memory.clone(),
            autostart: config.autostart,
            boot: config.boot.clone(),
            disks: resolved_disks,
            networks: resolved_networks,
            spice: config.spice.clone(),
            serial: config.serial.clone(),
            extra_args: config.extra_args.clone(),
            qmp_or_api_socket: qmp_or_api,
            serial_socket: serial_sock,
        }
    }

    /// Save compiled plan into /var/lib/mvmctl/resolved/ or fallback /run/mvmctl/resolved/
    pub fn save_resolved(&self) -> Result<PathBuf> {
        let candidate_dirs = [
            PathBuf::from("/var/lib/mvmctl/resolved"),
            PathBuf::from("/run/mvmctl/resolved"),
        ];

        let mut target_dir = candidate_dirs[1].clone();
        for dir in &candidate_dirs {
            if fs::create_dir_all(dir).is_ok() {
                target_dir = dir.clone();
                break;
            }
        }

        let target_file = target_dir.join(format!("{}.json", self.name));
        let content = serde_json::to_string_pretty(self)
            .context("Failed to serialize resolved plan")?;
        fs::write(&target_file, content)
            .with_context(|| format!("Failed to write plan to '{}'", target_file.display()))?;
        Ok(target_file)
    }

    pub fn load_resolved(name: &str) -> Result<Self> {
        let candidates = [
            PathBuf::from(format!("/var/lib/mvmctl/resolved/{}.json", name)),
            PathBuf::from(format!("/run/mvmctl/resolved/{}.json", name)),
        ];

        for cand in &candidates {
            if cand.is_file() {
                let content = fs::read_to_string(cand)?;
                let plan: ResolvedPlan = serde_json::from_str(&content)?;
                return Ok(plan);
            }
        }

        anyhow::bail!("Resolved plan for '{}' not found.", name)
    }
}
