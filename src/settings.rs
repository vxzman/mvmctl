use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

use crate::config::{BackendType, BootMode, VmConfig};

fn default_config_dirs() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/etc/mvmctl"),
        PathBuf::from("/etc/mvmctl/vms"),
        PathBuf::from("/usr/lib/mvmctl"),
    ]
}

fn default_runtime_dir() -> PathBuf {
    PathBuf::from("/run/mvmctl")
}

fn default_state_dir() -> PathBuf {
    PathBuf::from("/var/lib/mvmctl")
}

fn default_log_dir() -> PathBuf {
    PathBuf::from("/var/log/mvmctl")
}

fn default_mvmctl_bin() -> PathBuf {
    PathBuf::from("/usr/local/bin/mvmctl")
}

fn default_runtime_dir_mode() -> String {
    "0700".to_string()
}

fn default_socket_mode() -> String {
    "0600".to_string()
}

fn default_stop_timeout() -> u64 {
    45
}

fn default_max_ifname_len() -> usize {
    15
}

fn default_qemu_binary() -> PathBuf {
    PathBuf::from("/usr/bin/qemu-system-x86_64")
}

fn default_uefi_code() -> PathBuf {
    PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.ms.fd")
}

fn default_uefi_vars_template() -> PathBuf {
    PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.ms.fd")
}

fn default_nvram_dir() -> PathBuf {
    PathBuf::from("/var/lib/qemu/nvram")
}

fn default_qemu_prefix() -> String {
    "qu-".to_string()
}

fn default_machine() -> String {
    "q35,smm=on".to_string()
}

fn default_cpu() -> String {
    "host".to_string()
}

fn default_accel() -> String {
    "kvm".to_string()
}

fn default_ch_binaries() -> Vec<PathBuf> {
    vec![
        PathBuf::from("/usr/local/bin/cloud-hypervisor-static"),
        PathBuf::from("/usr/local/bin/cloud-hypervisor"),
        PathBuf::from("/usr/bin/cloud-hypervisor"),
        PathBuf::from("/usr/bin/cloud-hypervisor-static"),
    ]
}

fn default_ch_prefix() -> String {
    "ch-".to_string()
}

fn default_disk_queues() -> u32 {
    2
}

fn default_disk_queue_size() -> u32 {
    256
}

fn default_true() -> bool {
    true
}

fn default_cmdline() -> String {
    "console=ttyS0 root=/dev/vda rw rootwait".to_string()
}

fn default_unit_dir() -> PathBuf {
    PathBuf::from("/etc/systemd/system")
}

fn default_timeout_stop() -> u64 {
    90
}

fn default_after() -> Vec<String> {
    vec!["network-online.target".to_string()]
}

fn default_wants() -> Vec<String> {
    vec!["network-online.target".to_string()]
}

/// Host-wide settings. Per-VM hardware stays in the VM JSON files.
///
/// Loaded from `/etc/mvmctl/config.json`, or from the path in `MVMCTL_CONFIG`.
/// A missing file uses these defaults. Unknown keys are rejected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GlobalConfig {
    #[serde(default = "default_config_dirs")]
    pub config_dirs: Vec<PathBuf>,
    #[serde(default = "default_runtime_dir")]
    pub runtime_dir: PathBuf,
    #[serde(default = "default_state_dir")]
    pub state_dir: PathBuf,
    #[serde(default = "default_log_dir")]
    pub log_dir: PathBuf,
    #[serde(default = "default_mvmctl_bin")]
    pub mvmctl_bin: PathBuf,
    /// Octal mode string, for example `"0700"`.
    #[serde(default = "default_runtime_dir_mode")]
    pub runtime_dir_mode: String,
    /// Octal mode string applied to QMP, API, and serial sockets.
    #[serde(default = "default_socket_mode")]
    pub socket_mode: String,
    #[serde(default = "default_stop_timeout")]
    pub stop_timeout_secs: u64,
    #[serde(default = "default_max_ifname_len")]
    pub max_ifname_len: usize,
    #[serde(default)]
    pub qemu: QemuSettings,
    #[serde(default)]
    pub cloud_hypervisor: CloudHypervisorSettings,
    #[serde(default)]
    pub systemd: SystemdSettings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QemuSettings {
    #[serde(default = "default_qemu_binary")]
    pub binary: PathBuf,
    #[serde(default = "default_uefi_code")]
    pub uefi_code: PathBuf,
    #[serde(default = "default_uefi_vars_template")]
    pub uefi_vars_template: PathBuf,
    #[serde(default = "default_nvram_dir")]
    pub nvram_dir: PathBuf,
    #[serde(default = "default_qemu_prefix")]
    pub interface_prefix: String,
    #[serde(default = "default_machine")]
    pub machine: String,
    #[serde(default = "default_cpu")]
    pub cpu: String,
    #[serde(default = "default_accel")]
    pub accel: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CloudHypervisorSettings {
    #[serde(default = "default_ch_binaries")]
    pub binaries: Vec<PathBuf>,
    #[serde(default = "default_ch_prefix")]
    pub interface_prefix: String,
    #[serde(default = "default_disk_queues")]
    pub disk_num_queues: u32,
    #[serde(default = "default_disk_queue_size")]
    pub disk_queue_size: u32,
    /// Newer cloud-hypervisor requires `image_type`. Set false for builds that reject the key.
    #[serde(default = "default_true")]
    pub pass_image_type: bool,
    #[serde(default = "default_cmdline")]
    pub default_cmdline: String,
    /// Used when a VM's `boot.mode` is `uefi` and `boot.uefi_code` is unset.
    #[serde(default)]
    pub uefi_firmware: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SystemdSettings {
    #[serde(default = "default_unit_dir")]
    pub unit_dir: PathBuf,
    #[serde(default = "default_timeout_stop")]
    pub timeout_stop_sec: u64,
    #[serde(default = "default_after")]
    pub after: Vec<String>,
    #[serde(default = "default_wants")]
    pub wants: Vec<String>,
}

impl Default for QemuSettings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("default qemu settings")
    }
}

impl Default for CloudHypervisorSettings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("default cloud-hypervisor settings")
    }
}

impl Default for SystemdSettings {
    fn default() -> Self {
        serde_json::from_str("{}").expect("default systemd settings")
    }
}

impl Default for GlobalConfig {
    fn default() -> Self {
        serde_json::from_str("{}").expect("default global config")
    }
}

impl GlobalConfig {
    /// Load `/etc/mvmctl/config.json`, unless `MVMCTL_CONFIG` names another file.
    /// A missing file is not an error. `MVMCTL_CONFIG_DIR`, when set, is inserted
    /// at the front of `config_dirs`.
    pub fn load() -> Result<Self> {
        let path = std::env::var("MVMCTL_CONFIG")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("/etc/mvmctl/config.json"));
        let mut cfg = if path.is_file() {
            Self::load_file(&path)?
        } else if std::env::var_os("MVMCTL_CONFIG").is_some() {
            return Err(anyhow!("Global config '{}' does not exist", path.display()));
        } else {
            Self::default()
        };
        cfg.apply_env();
        Ok(cfg)
    }

    pub fn load_file(path: &Path) -> Result<Self> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read global config '{}'", path.display()))?;
        Self::from_json(&text)
            .with_context(|| format!("Failed to parse global config '{}'", path.display()))
    }

    pub fn from_json(text: &str) -> Result<Self> {
        let cfg: Self = serde_json::from_str(text)?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn apply_env(&mut self) {
        if let Ok(dir) = std::env::var("MVMCTL_CONFIG_DIR") {
            if !dir.is_empty() {
                let path = PathBuf::from(dir);
                if self.config_dirs.first() != Some(&path) {
                    self.config_dirs.insert(0, path);
                }
            }
        }
    }

    fn validate(&self) -> Result<()> {
        if self.stop_timeout_secs == 0 {
            return Err(anyhow!("stop_timeout_secs must be greater than 0"));
        }
        if self.systemd.timeout_stop_sec == 0 {
            return Err(anyhow!("systemd.timeout_stop_sec must be greater than 0"));
        }
        if self.max_ifname_len < 2 || self.max_ifname_len > 15 {
            return Err(anyhow!("max_ifname_len must be between 2 and 15"));
        }
        parse_mode(&self.runtime_dir_mode)?;
        parse_mode(&self.socket_mode)?;
        check_prefix("qemu.interface_prefix", &self.qemu.interface_prefix)?;
        check_prefix(
            "cloud_hypervisor.interface_prefix",
            &self.cloud_hypervisor.interface_prefix,
        )?;
        if self.cloud_hypervisor.disk_num_queues == 0 {
            return Err(anyhow!(
                "cloud_hypervisor.disk_num_queues must be greater than 0"
            ));
        }
        if self.cloud_hypervisor.disk_queue_size == 0 {
            return Err(anyhow!(
                "cloud_hypervisor.disk_queue_size must be greater than 0"
            ));
        }
        Ok(())
    }

    pub fn runtime_mode(&self) -> Result<u32> {
        parse_mode(&self.runtime_dir_mode)
    }

    pub fn socket_mode_bits(&self) -> Result<u32> {
        parse_mode(&self.socket_mode)
    }

    /// Systemd's stop timeout must outlive the in-process ACPI wait.
    pub fn unit_stop_timeout(&self) -> u64 {
        self.systemd
            .timeout_stop_sec
            .max(self.stop_timeout_secs.saturating_add(15))
    }

    /// Binary embedded in generated units.
    /// `MVMCTL_BIN` wins, then an existing `mvmctl_bin`, then the running executable.
    pub fn unit_binary(&self) -> PathBuf {
        resolve_unit_binary(
            &self.mvmctl_bin,
            std::env::var("MVMCTL_BIN").ok().as_deref(),
            std::env::current_exe().ok().as_deref(),
        )
    }

    pub fn control_socket(&self, backend: &BackendType, name: &str) -> PathBuf {
        let suffix = match backend {
            BackendType::Qemu => "qmp",
            BackendType::CloudHypervisor => "api",
        };
        self.runtime_dir.join(format!("{name}-{suffix}.sock"))
    }

    pub fn default_serial_socket(&self, name: &str) -> PathBuf {
        self.runtime_dir.join(format!("{name}-serial.sock"))
    }

    pub fn resolved_dirs(&self) -> [PathBuf; 2] {
        [
            self.state_dir.join("resolved"),
            self.runtime_dir.join("resolved"),
        ]
    }

    pub fn interface_prefix(&self, backend: &BackendType) -> &str {
        match backend {
            BackendType::Qemu => &self.qemu.interface_prefix,
            BackendType::CloudHypervisor => &self.cloud_hypervisor.interface_prefix,
        }
    }
}

pub fn resolve_unit_binary(
    configured: &Path,
    env_override: Option<&str>,
    current_exe: Option<&Path>,
) -> PathBuf {
    if let Some(p) = env_override {
        if !p.is_empty() {
            return PathBuf::from(p);
        }
    }
    if configured.is_file() {
        return configured.to_path_buf();
    }
    if let Some(cur) = current_exe {
        return cur.to_path_buf();
    }
    configured.to_path_buf()
}

pub fn parse_mode(text: &str) -> Result<u32> {
    let raw = text.trim();
    let digits = raw
        .strip_prefix("0o")
        .or_else(|| raw.strip_prefix("0O"))
        .unwrap_or(raw);
    if digits.is_empty() || !digits.chars().all(|c| ('0'..='7').contains(&c)) {
        return Err(anyhow!(
            "File mode '{text}' is not an octal string such as \"0700\""
        ));
    }
    let mode = u32::from_str_radix(digits, 8)
        .map_err(|_| anyhow!("File mode '{text}' is not a valid octal number"))?;
    if mode > 0o777 {
        return Err(anyhow!("File mode '{text}' is wider than 0777"));
    }
    Ok(mode)
}

fn check_prefix(field: &str, prefix: &str) -> Result<()> {
    if prefix.is_empty()
        || prefix.contains('/')
        || prefix.contains(' ')
        || !prefix
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_' || c == '.')
    {
        return Err(anyhow!(
            "{field} must be a short interface prefix without spaces or slashes"
        ));
    }
    Ok(())
}

pub fn which(name: &str) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&paths) {
        let full = dir.join(name);
        if full.is_file() {
            return Some(full);
        }
    }
    None
}

pub fn interface_name(
    global: &GlobalConfig,
    backend: &BackendType,
    vm_name: &str,
    requested: Option<&str>,
    index: usize,
    total: usize,
) -> Result<String> {
    let prefix = global.interface_prefix(backend);
    let ifname = match requested {
        Some(name) => name.to_string(),
        None if total <= 1 => format!("{prefix}{vm_name}"),
        None => format!("{prefix}{vm_name}-{index}"),
    };
    validate_ifname(&ifname, global.max_ifname_len)?;
    Ok(ifname)
}

pub fn interface_names(global: &GlobalConfig, cfg: &VmConfig) -> Result<Vec<String>> {
    let total = cfg.networks.len();
    cfg.networks
        .iter()
        .enumerate()
        .map(|(idx, net)| {
            interface_name(
                global,
                &cfg.backend,
                &cfg.name,
                net.name.as_deref(),
                idx,
                total,
            )
        })
        .collect()
}

pub fn validate_ifname(name: &str, max_len: usize) -> Result<()> {
    if name.is_empty() || name.len() > max_len {
        return Err(anyhow!(
            "Interface name '{name}' must be 1..={max_len} bytes (IFNAMSIZ)"
        ));
    }
    if name == "." || name == ".." || name.contains('/') {
        return Err(anyhow!(
            "Interface name '{name}' is not a usable device name"
        ));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | ':'))
    {
        return Err(anyhow!(
            "Interface name '{name}' contains characters outside [A-Za-z0-9_.:-]"
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SerialTarget {
    None,
    Socket(PathBuf),
    File(PathBuf),
    Stdio,
}

pub fn resolve_serial(global: &GlobalConfig, cfg: &VmConfig) -> Result<SerialTarget> {
    let mode = cfg
        .serial
        .as_ref()
        .and_then(|s| s.mode.as_deref())
        .unwrap_or("socket");
    let path = cfg.serial.as_ref().and_then(|s| s.path.clone());
    match mode {
        "none" | "off" => Ok(SerialTarget::None),
        "stdio" | "tty" => Ok(SerialTarget::Stdio),
        "file" => {
            Ok(SerialTarget::File(path.unwrap_or_else(|| {
                global.log_dir.join(format!("{}.log", cfg.name))
            })))
        }
        "socket" => {
            Ok(SerialTarget::Socket(path.unwrap_or_else(|| {
                global.default_serial_socket(&cfg.name)
            })))
        }
        other => Err(anyhow!(
            "Unsupported serial mode '{other}'. Use socket, file, stdio, tty, or none"
        )),
    }
}

pub fn check_boot(global: &GlobalConfig, cfg: &VmConfig) -> Result<()> {
    match cfg.boot.mode {
        BootMode::Direct => {
            if cfg.boot.kernel.is_none() {
                return Err(anyhow!("direct boot requires 'boot.kernel'"));
            }
        }
        BootMode::Uefi => {
            if cfg.backend == BackendType::CloudHypervisor && firmware_path(global, cfg).is_none() {
                return Err(anyhow!(
                    "Cloud-Hypervisor UEFI boot requires 'boot.uefi_code' or cloud_hypervisor.uefi_firmware in config.json"
                ));
            }
        }
    }
    Ok(())
}

/// Checks that do not require the guest files to exist yet.
pub fn check_vm(global: &GlobalConfig, cfg: &VmConfig) -> Result<()> {
    check_boot(global, cfg)?;
    let _ = resolve_serial(global, cfg)?;
    let _ = interface_names(global, cfg)?;
    Ok(())
}

pub fn firmware_path<'a>(global: &'a GlobalConfig, cfg: &'a VmConfig) -> Option<&'a Path> {
    cfg.boot
        .uefi_code
        .as_deref()
        .or(global.cloud_hypervisor.uefi_firmware.as_deref())
}

pub fn path_warnings(global: &GlobalConfig, cfg: &VmConfig) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(kernel) = &cfg.boot.kernel {
        if !kernel.exists() {
            warnings.push(format!("Kernel '{}' does not exist yet", kernel.display()));
        }
    }
    if let Some(initrd) = &cfg.boot.initrd {
        if !initrd.exists() {
            warnings.push(format!("Initrd '{}' does not exist yet", initrd.display()));
        }
    }
    for disk in &cfg.disks {
        if !disk.path.exists() {
            warnings.push(format!("Disk '{}' does not exist yet", disk.path.display()));
        }
    }
    if cfg.boot.mode == BootMode::Uefi {
        let code = cfg
            .boot
            .uefi_code
            .as_ref()
            .unwrap_or(&global.qemu.uefi_code);
        if cfg.backend == BackendType::Qemu && !code.exists() {
            warnings.push(format!("UEFI code '{}' does not exist yet", code.display()));
        }
        if cfg.backend == BackendType::CloudHypervisor {
            if let Some(fw) = firmware_path(global, cfg) {
                if !fw.exists() {
                    warnings.push(format!("Firmware '{}' does not exist yet", fw.display()));
                }
            }
        }
    }
    warnings
}

/// Escape a filesystem path the way `systemd-escape --path` does.
pub fn systemd_path_escape(path: &str) -> String {
    let trimmed = path.trim_start_matches('/');
    let mut out = String::new();
    for b in trimmed.bytes() {
        match b {
            b'/' => out.push('-'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b':' | b'_' | b'.' => {
                out.push(b as char);
            }
            c => out.push_str(&format!("\\x{c:02x}")),
        }
    }
    out
}

pub fn escape_unit_field(value: &str) -> String {
    value.replace(['\n', '\r'], " ").replace('%', "%%")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn example_file_matches_builtin_defaults() {
        let cfg = GlobalConfig::from_json(include_str!("../examples/config.json")).unwrap();
        assert_eq!(cfg, GlobalConfig::default());
    }

    #[test]
    fn empty_json_matches_builtin_defaults() {
        let cfg = GlobalConfig::from_json("{}").unwrap();
        assert_eq!(cfg.runtime_dir, PathBuf::from("/run/mvmctl"));
        assert_eq!(
            cfg.qemu.binary,
            PathBuf::from("/usr/bin/qemu-system-x86_64")
        );
        assert_eq!(cfg.qemu.interface_prefix, "qu-");
        assert_eq!(cfg.cloud_hypervisor.interface_prefix, "ch-");
        assert_eq!(cfg.cloud_hypervisor.disk_num_queues, 2);
        assert_eq!(cfg.stop_timeout_secs, 45);
        assert_eq!(cfg.unit_stop_timeout(), 90);
        assert!(cfg.cloud_hypervisor.pass_image_type);
    }

    #[test]
    fn partial_json_overrides_one_field() {
        let cfg = GlobalConfig::from_json(
            r#"{"runtime_dir":"/tmp/mvm","stop_timeout_secs":10,"systemd":{"timeout_stop_sec":20}}"#,
        )
        .unwrap();
        assert_eq!(cfg.runtime_dir, PathBuf::from("/tmp/mvm"));
        assert_eq!(cfg.stop_timeout_secs, 10);
        assert_eq!(cfg.state_dir, PathBuf::from("/var/lib/mvmctl"));
        assert_eq!(cfg.unit_stop_timeout(), 25);
    }

    #[test]
    fn unknown_field_is_rejected() {
        let err = GlobalConfig::from_json(r#"{"runtimeDir":"/tmp"}"#).unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("runtimeDir") || msg.contains("unknown field"));
    }

    #[test]
    fn mode_parser_accepts_octal() {
        assert_eq!(parse_mode("0700").unwrap(), 0o700);
        assert_eq!(parse_mode("0o600").unwrap(), 0o600);
        assert!(parse_mode("888").is_err());
        assert!(parse_mode("01777").is_err());
    }

    #[test]
    fn path_escape_matches_systemd() {
        assert_eq!(
            systemd_path_escape("/dev/ubuntu-vg/rhel.raw"),
            r"dev-ubuntu\x2dvg-rhel.raw"
        );
        assert_eq!(
            systemd_path_escape("/dev/disk/by-id/foo:bar"),
            r"dev-disk-by\x2did-foo:bar"
        );
    }

    #[test]
    fn unit_binary_prefers_env_then_existing_file() {
        let configured = Path::new("/usr/local/bin/mvmctl");
        let current = Path::new("/tmp/mvmctl-dev");
        assert_eq!(
            resolve_unit_binary(configured, Some("/opt/mvmctl"), Some(current)),
            PathBuf::from("/opt/mvmctl")
        );
        assert_eq!(
            resolve_unit_binary(Path::new("/no/such/mvmctl"), None, Some(current)),
            current
        );
    }

    #[test]
    fn interface_names_do_not_use_prefix_of_another_vm() {
        let global = GlobalConfig::default();
        let app = sample_vm("app", 2);
        let app1 = sample_vm("app-1", 1);
        assert_eq!(
            interface_names(&global, &app).unwrap(),
            vec!["qu-app-0", "qu-app-1"]
        );
        assert_eq!(interface_names(&global, &app1).unwrap(), vec!["qu-app-1"]);
    }

    #[test]
    fn long_default_interface_name_is_rejected() {
        let global = GlobalConfig::default();
        let cfg = sample_vm("this-name-is-too-long", 1);
        let err = interface_names(&global, &cfg).unwrap_err();
        assert!(err.to_string().contains("IFNAMSIZ"));
    }

    fn sample_vm(name: &str, nics: usize) -> VmConfig {
        let networks = (0..nics)
            .map(|i| {
                format!(r#"{{"mode":"macvtap","parent":"eth0","mac":"52:54:00:00:00:{i:02x}"}}"#)
            })
            .collect::<Vec<_>>()
            .join(",");
        let text = format!(
            r#"{{
                "name":"{name}",
                "backend":"qemu",
                "boot":{{"mode":"direct","kernel":"/tmp/vmlinuz"}},
                "networks":[{networks}]
            }}"#
        );
        serde_json::from_str(&text).unwrap()
    }
}
