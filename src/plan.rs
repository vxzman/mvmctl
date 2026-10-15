use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

use crate::config::{BackendType, BootConfig, SerialConfig, SpiceConfig, VmConfig};
use crate::registry::ResourceRegistry;
use crate::settings::{self, GlobalConfig, SerialTarget};

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
    pub fn compile(global: &GlobalConfig, config: &VmConfig) -> Result<Self> {
        let total_nets = config.networks.len();

        let mut resolved_networks = Vec::with_capacity(total_nets);
        for (idx, net) in config.networks.iter().enumerate() {
            let ifname = settings::interface_name(
                global,
                &config.backend,
                &config.name,
                net.name.as_deref(),
                idx,
                total_nets,
            )?;
            resolved_networks.push(ResolvedNetwork {
                mode: net.mode.clone(),
                parent: net.parent.clone(),
                mac: net.mac.as_ref().map(|m| m.to_lowercase()),
                interface_name: ifname,
            });
        }

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

        let qmp_or_api = global.control_socket(&config.backend, &config.name);
        let serial_sock = match settings::resolve_serial(global, config)? {
            SerialTarget::Socket(path) => Some(path),
            _ => None,
        };

        Ok(ResolvedPlan {
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
        })
    }

    /// Save compiled plan into `<state_dir>/resolved`, falling back to `<runtime_dir>/resolved`.
    pub fn save_resolved(&self, global: &GlobalConfig) -> Result<PathBuf> {
        let candidate_dirs = global.resolved_dirs();

        let mut target_dir = candidate_dirs[1].clone();
        for dir in &candidate_dirs {
            if fs::create_dir_all(dir).is_ok() {
                target_dir = dir.clone();
                break;
            }
        }

        let target_file = target_dir.join(format!("{}.json", self.name));
        let content =
            serde_json::to_string_pretty(self).context("Failed to serialize resolved plan")?;
        fs::write(&target_file, content)
            .with_context(|| format!("Failed to write plan to '{}'", target_file.display()))?;
        Ok(target_file)
    }
}
