use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum BackendType {
    Qemu,
    #[serde(alias = "ch")]
    CloudHypervisor,
}

impl std::fmt::Display for BackendType {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            BackendType::Qemu => write!(f, "qemu"),
            BackendType::CloudHypervisor => write!(f, "cloud-hypervisor"),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "kebab-case")]
pub enum BootMode {
    #[default]
    Uefi,
    Direct,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BootConfig {
    #[serde(default)]
    pub mode: BootMode,

    // Direct kernel boot options
    pub kernel: Option<PathBuf>,
    pub initrd: Option<PathBuf>,
    pub cmdline: Option<String>,

    // UEFI firmware boot options
    pub uefi_code: Option<PathBuf>,
    pub uefi_vars: Option<PathBuf>,
}

fn default_disk_format() -> String {
    "raw".to_string()
}

fn default_cache() -> String {
    "none".to_string()
}

fn default_aio() -> String {
    "io_uring".to_string()
}

fn default_discard() -> String {
    "unmap".to_string()
}

fn default_true() -> bool {
    true
}

fn default_cpus() -> u32 {
    2
}

fn default_memory() -> String {
    "2G".to_string()
}

fn default_spice_addr() -> String {
    "0.0.0.0".to_string()
}

fn default_net_mode() -> String {
    "macvtap".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskConfig {
    pub path: PathBuf,
    #[serde(default = "default_disk_format")]
    pub format: String,
    #[serde(default = "default_true")]
    pub direct: bool,
    #[serde(default = "default_cache")]
    pub cache: String,
    #[serde(default = "default_aio")]
    pub aio: String,
    #[serde(default = "default_discard")]
    pub discard: String,
    #[serde(default)]
    pub readonly: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NetworkConfig {
    #[serde(default = "default_net_mode")]
    pub mode: String, // "macvtap" or "bridge"
    pub parent: String,
    pub mac: Option<String>,
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SpiceConfig {
    #[serde(default)]
    pub enable: bool,
    #[serde(default = "default_spice_addr")]
    pub addr: String,
    pub port: Option<u16>,
    pub password: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SerialConfig {
    #[serde(default)]
    pub mode: Option<String>, // "socket", "stdio", "file", "none"
    pub path: Option<PathBuf>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub name: String,
    pub description: Option<String>,
    pub backend: BackendType,
    pub hypervisor_path: Option<PathBuf>,
    #[serde(default = "default_cpus")]
    pub cpus: u32,
    #[serde(default = "default_memory")]
    pub memory: String,
    #[serde(default)]
    pub autostart: bool,
    #[serde(default)]
    pub boot: BootConfig,
    #[serde(default)]
    pub disks: Vec<DiskConfig>,
    #[serde(default)]
    pub networks: Vec<NetworkConfig>,
    #[serde(default)]
    pub spice: Option<SpiceConfig>,
    #[serde(default)]
    pub serial: Option<SerialConfig>,
    #[serde(default)]
    pub extra_args: Vec<String>,
}

impl VmConfig {
    pub fn load_from_file(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config file '{}'", path.display()))?;
        let config: VmConfig = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse JSON config in '{}'", path.display()))?;
        config.validate(path)?;
        Ok(config)
    }

    pub fn validate(&self, path: &Path) -> Result<()> {
        if self.name.is_empty() {
            return Err(anyhow!("VM name cannot be empty in '{}'", path.display()));
        }

        match self.boot.mode {
            BootMode::Direct => {
                if let Some(kernel) = &self.boot.kernel {
                    if !kernel.exists() {
                        eprintln!(
                            "Warning: Kernel path '{}' does not exist currently.",
                            kernel.display()
                        );
                    }
                } else if self.backend == BackendType::CloudHypervisor {
                    return Err(anyhow!(
                        "Cloud-Hypervisor direct boot requires specifying 'boot.kernel'"
                    ));
                }
            }
            BootMode::Uefi => {
                if self.backend == BackendType::CloudHypervisor {
                    // Cloud-hypervisor uses direct kernel or direct firmware
                }
            }
        }

        Ok(())
    }

    pub fn find_and_load(target: &str) -> Result<(PathBuf, Self)> {
        let direct_path = PathBuf::from(target);
        if direct_path.is_file() {
            let cfg = Self::load_from_file(&direct_path)?;
            return Ok((std::fs::canonicalize(&direct_path).unwrap_or(direct_path), cfg));
        }

        let candidates = [
            PathBuf::from(format!("/etc/mvmctl/{}.json", target)),
            PathBuf::from(format!("/etc/mvmctl/vms/{}.json", target)),
            PathBuf::from(format!("{}.json", target)),
        ];

        for cand in &candidates {
            if cand.is_file() {
                let cfg = Self::load_from_file(cand)?;
                return Ok((std::fs::canonicalize(cand).unwrap_or_else(|_| cand.clone()), cfg));
            }
        }

        Err(anyhow!(
            "Could not locate VM config for '{}'. Searched: {}",
            target,
            candidates.iter().map(|p| p.display().to_string()).collect::<Vec<_>>().join(", ")
        ))
    }
}
