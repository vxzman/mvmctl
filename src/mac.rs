use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{is_vm_config_file, VmConfig};
use crate::settings::GlobalConfig;

const MAC_SALT_LIMIT: u32 = 10_000;

pub fn mac_is_unset(mac: &Option<String>) -> bool {
    match mac {
        None => true,
        Some(text) => mac_is_unset_str(text),
    }
}

pub fn mac_is_unset_str(mac: &str) -> bool {
    mac.trim().is_empty()
}

/// Fill empty MACs in memory. Explicit addresses are kept as written.
///
/// Assignment walks VM name, then NIC index, after every explicit address is
/// reserved. The same inputs always produce the same address, including when
/// the generator fills a config that has not been written back yet.
pub fn fill_configs(configs: &mut [(PathBuf, VmConfig)]) -> Result<()> {
    let mut used = HashSet::new();
    for (_, cfg) in configs.iter() {
        for net in &cfg.networks {
            if let Some(mac) = explicit_mac(&net.mac) {
                used.insert(mac);
            }
        }
    }

    let mut order: Vec<usize> = (0..configs.len()).collect();
    order.sort_by(|&a, &b| configs[a].1.name.cmp(&configs[b].1.name).then(a.cmp(&b)));

    let mut assigned = Vec::new();
    for idx in order {
        let name = configs[idx].1.name.clone();
        for nic in 0..configs[idx].1.networks.len() {
            if mac_is_unset(&configs[idx].1.networks[nic].mac) {
                let mac = allocate_mac(&name, nic as u32, &mut used)?;
                assigned.push((idx, nic, mac));
            }
        }
    }
    for (idx, nic, mac) in assigned {
        configs[idx].1.networks[nic].mac = Some(mac);
    }
    Ok(())
}

/// Allocate missing MACs and write them back. `also` covers `mvmctl run ./vm.json`
/// when that file is outside `config_dirs`. Files whose MACs are already set
/// are left byte-for-byte unchanged. The systemd generator must not call this.
pub fn persist_missing_macs(global: &GlobalConfig, also: Option<&Path>) -> Result<()> {
    let mut paths = discover_vm_files(global)?;
    if let Some(extra) = also {
        if extra.is_file() {
            push_unique(&mut paths, extra.to_path_buf());
        }
    }

    let mut parsed = Vec::new();
    for path in paths {
        if let Some(item) = parse_vm_file(&path) {
            parsed.push(item);
        }
    }

    let mut configs: Vec<(PathBuf, VmConfig)> = parsed
        .iter()
        .map(|item| (item.path.clone(), item.config.clone()))
        .collect();
    fill_configs(&mut configs)?;

    for (item, (_, filled)) in parsed.iter_mut().zip(configs.iter()) {
        let wrote = write_assigned_macs(&item.path, &mut item.value, filled)?;
        if !wrote {
            continue;
        }
        for (nic, net) in filled.networks.iter().enumerate() {
            if mac_is_unset(&item.config.networks[nic].mac) {
                if let Some(mac) = &net.mac {
                    crate::log::info(format!(
                        "assigned MAC {mac} to VM '{}' network {nic} ({})",
                        filled.name,
                        item.path.display()
                    ));
                }
            }
        }
    }
    Ok(())
}

/// Overlay the stable MAC set onto one VM without writing its file.
pub fn overlay_stable_macs(global: &GlobalConfig, config: &mut VmConfig) -> Result<()> {
    let mut configs = Vec::new();
    for path in discover_vm_files(global)? {
        if let Some(item) = parse_vm_file(&path) {
            configs.push((item.path, item.config));
        }
    }
    if let Some(slot) = configs.iter_mut().find(|(_, cfg)| cfg.name == config.name) {
        *slot = (PathBuf::new(), config.clone());
    } else {
        configs.push((PathBuf::new(), config.clone()));
    }
    fill_configs(&mut configs)?;
    if let Some((_, filled)) = configs.iter().find(|(_, cfg)| cfg.name == config.name) {
        config.networks.clone_from(&filled.networks);
    }
    Ok(())
}

fn explicit_mac(mac: &Option<String>) -> Option<String> {
    mac.as_ref()
        .map(|text| text.trim())
        .filter(|text| !text.is_empty())
        .map(|text| text.to_ascii_lowercase())
}

fn allocate_mac(vm: &str, nic: u32, used: &mut HashSet<String>) -> Result<String> {
    for salt in 0..MAC_SALT_LIMIT {
        let mac = hash_mac(vm, nic, salt);
        if used.insert(mac.clone()) {
            return Ok(mac);
        }
    }
    Err(anyhow!(
        "Could not allocate a free MAC for VM '{vm}' network {nic}"
    ))
}

fn hash_mac(vm: &str, nic: u32, salt: u32) -> String {
    let mut data = Vec::with_capacity(vm.len() + 10);
    data.extend_from_slice(vm.as_bytes());
    data.push(0);
    data.extend_from_slice(&nic.to_le_bytes());
    data.push(0);
    data.extend_from_slice(&salt.to_le_bytes());
    let hash = fnv1a64(&data);
    format!(
        "52:54:00:{:02x}:{:02x}:{:02x}",
        hash as u8,
        (hash >> 8) as u8,
        (hash >> 16) as u8
    )
}

fn fnv1a64(data: &[u8]) -> u64 {
    const OFFSET: u64 = 0xcbf29ce484222325;
    const PRIME: u64 = 0x100000001b3;
    let mut hash = OFFSET;
    for byte in data {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(PRIME);
    }
    hash
}

struct ParsedVm {
    path: PathBuf,
    value: Value,
    config: VmConfig,
}

fn discover_vm_files(global: &GlobalConfig) -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();
    for dir in &global.config_dirs {
        if !dir.is_dir() {
            continue;
        }
        for entry in
            fs::read_dir(dir).with_context(|| format!("Failed to read '{}'", dir.display()))?
        {
            let path = entry
                .with_context(|| format!("Failed to read '{}'", dir.display()))?
                .path();
            if path.is_file() && is_vm_config_file(&path) {
                push_unique(&mut paths, path);
            }
        }
    }
    Ok(paths)
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    let canon = fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
    let seen = paths
        .iter()
        .any(|existing| fs::canonicalize(existing).unwrap_or_else(|_| existing.clone()) == canon);
    if !seen {
        paths.push(path);
    }
}

fn parse_vm_file(path: &Path) -> Option<ParsedVm> {
    let text = fs::read_to_string(path).ok()?;
    let value: Value = serde_json::from_str(&text).ok()?;
    let config: VmConfig = serde_json::from_value(value.clone()).ok()?;
    config.validate(path).ok()?;
    Some(ParsedVm {
        path: fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
        value,
        config,
    })
}

fn write_assigned_macs(path: &Path, value: &mut Value, filled: &VmConfig) -> Result<bool> {
    let Some(networks) = value.get_mut("networks").and_then(|v| v.as_array_mut()) else {
        return Ok(false);
    };
    if networks.len() != filled.networks.len() {
        return Err(anyhow!(
            "Network list in '{}' does not match the parsed VM",
            path.display()
        ));
    }
    let mut changed = false;
    for (slot, net) in networks.iter_mut().zip(&filled.networks) {
        let obj = slot
            .as_object_mut()
            .ok_or_else(|| anyhow!("Network entry in '{}' is not a JSON object", path.display()))?;
        let unset = match obj.get("mac") {
            None | Some(Value::Null) => true,
            Some(Value::String(text)) => text.trim().is_empty(),
            Some(_) => false,
        };
        if unset {
            let mac = net.mac.as_ref().ok_or_else(|| {
                anyhow!("Missing allocated MAC while updating '{}'", path.display())
            })?;
            obj.insert("mac".to_string(), Value::String(mac.clone()));
            changed = true;
        }
    }
    if changed {
        write_atomic(path, value)?;
    }
    Ok(changed)
}

fn write_atomic(path: &Path, value: &Value) -> Result<()> {
    let text = serde_json::to_string_pretty(value)? + "\n";
    let tmp = tmp_path(path);
    fs::write(&tmp, text).with_context(|| format!("Failed to write '{}'", tmp.display()))?;
    if let Ok(meta) = fs::metadata(path) {
        let _ = fs::set_permissions(&tmp, meta.permissions());
    }
    if let Err(err) = fs::rename(&tmp, path) {
        let _ = fs::remove_file(&tmp);
        return Err(err).with_context(|| format!("Failed to replace '{}'", path.display()));
    }
    Ok(())
}

fn tmp_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_os_string();
    name.push(".tmp");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vm(name: &str, mac: Option<&str>) -> (PathBuf, VmConfig) {
        let mac_field = match mac {
            None => String::new(),
            Some(value) => format!(r#","mac":"{value}""#),
        };
        let cfg: VmConfig = serde_json::from_str(&format!(
            r#"{{"name":"{name}","backend":"qemu","networks":[{{"parent":"eth0"{mac_field}}},{{"parent":"eth0"}}]}}"#
        ))
        .unwrap();
        (PathBuf::from(format!("{name}.json")), cfg)
    }

    #[test]
    fn hash_is_stable_and_uses_the_qemu_oui() {
        assert_eq!(hash_mac("app", 0, 0), "52:54:00:ec:1a:98");
        assert_eq!(hash_mac("app", 1, 0), "52:54:00:ff:10:33");
        assert_ne!(hash_mac("app", 0, 1), "52:54:00:ec:1a:98");
        assert!(mac_is_unset(&None));
        assert!(mac_is_unset(&Some("  ".to_string())));
        assert!(!mac_is_unset(&Some("52:54:00:00:00:01".to_string())));
    }

    #[test]
    fn explicit_mac_wins_and_order_does_not_change_the_result() {
        let mut forward = vec![vm("zeta", None), vm("app", Some("52:54:00:AA:BB:CC"))];
        let mut reverse = vec![vm("app", Some("52:54:00:AA:BB:CC")), vm("zeta", None)];
        fill_configs(&mut forward).unwrap();
        fill_configs(&mut reverse).unwrap();

        let app = forward.iter().find(|(_, cfg)| cfg.name == "app").unwrap();
        assert_eq!(app.1.networks[0].mac.as_deref(), Some("52:54:00:AA:BB:CC"));
        let zeta = forward.iter().find(|(_, cfg)| cfg.name == "zeta").unwrap();
        let zeta_again = reverse.iter().find(|(_, cfg)| cfg.name == "zeta").unwrap();
        assert_eq!(zeta.1.networks[0].mac, zeta_again.1.networks[0].mac);
        assert_eq!(zeta.1.networks[1].mac, zeta_again.1.networks[1].mac);
        assert_ne!(zeta.1.networks[0].mac, zeta.1.networks[1].mac);
        assert!(zeta.1.networks[0]
            .mac
            .as_deref()
            .unwrap()
            .starts_with("52:54:00:"));
    }

    #[test]
    fn collision_with_an_explicit_mac_bumps_the_salt() {
        let mut configs = vec![vm("other", Some("52:54:00:ec:1a:98")), vm("app", None)];
        fill_configs(&mut configs).unwrap();
        let app = configs.iter().find(|(_, cfg)| cfg.name == "app").unwrap();
        let salted = hash_mac("app", 0, 1);
        assert_eq!(app.1.networks[0].mac.as_deref(), Some(salted.as_str()));
        assert_eq!(
            configs[0].1.networks[0].mac.as_deref(),
            Some("52:54:00:ec:1a:98")
        );
    }

    #[test]
    fn persist_writes_only_missing_macs_and_keeps_unknown_fields() {
        let dir = std::env::temp_dir().join(format!("mvmctl-mac-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let vm_path = dir.join("app.json");
        let explicit = dir.join("kept.json");
        let global_path = dir.join("config.json");
        fs::write(
            &vm_path,
            "{\n  \"name\": \"app\",\n  \"backend\": \"qemu\",\n  \"note\": \"keep\",\n  \"networks\": [{\"parent\": \"eth0\", \"mac\": \"\"}]\n}\n",
        )
        .unwrap();
        let explicit_body =
            "{\n  \"name\": \"kept\",\n  \"backend\": \"qemu\",\n  \"networks\": [{\"parent\": \"eth0\", \"mac\": \"52:54:00:00:00:0A\"}]\n}\n";
        fs::write(&explicit, explicit_body).unwrap();
        fs::write(&global_path, "{\"stop_timeout_secs\":12,\"custom\":true}\n").unwrap();

        let global = GlobalConfig {
            config_dirs: vec![dir.clone()],
            ..GlobalConfig::default()
        };
        persist_missing_macs(&global, None).unwrap();

        let written = fs::read_to_string(&vm_path).unwrap();
        let value: Value = serde_json::from_str(&written).unwrap();
        assert_eq!(value["note"], "keep");
        assert_eq!(value["networks"][0]["mac"], "52:54:00:ec:1a:98");
        assert_eq!(fs::read_to_string(&explicit).unwrap(), explicit_body);
        assert_eq!(
            fs::read_to_string(&global_path).unwrap(),
            "{\"stop_timeout_secs\":12,\"custom\":true}\n"
        );

        let again = fs::read_to_string(&vm_path).unwrap();
        persist_missing_macs(&global, None).unwrap();
        assert_eq!(fs::read_to_string(&vm_path).unwrap(), again);

        let _ = fs::remove_dir_all(&dir);
    }
}
