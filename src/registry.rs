use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use crate::config::{is_vm_config_file, VmConfig};
use crate::mac;
use crate::settings::{self, GlobalConfig, SerialTarget};

#[derive(Debug, Clone)]
pub struct ResourceEntry {
    pub vm_name: String,
    pub resource_type: String,
    pub identifier: String,
    pub detail: String,
}

#[derive(Debug, Default)]
pub struct ResourceRegistry {
    pub names: HashMap<String, PathBuf>,
    /// Loaded configs with empty MACs filled in memory. Nothing is written.
    pub configs: HashMap<String, VmConfig>,
    pub disks: HashMap<PathBuf, Vec<(String, bool)>>, // canonical_path -> Vec<(vm_name, readonly)>
    pub macs: HashMap<String, Vec<(String, String)>>, // mac_lower -> Vec<(vm_name, parent_if)>
    pub interfaces: HashMap<String, String>,          // ifname -> vm_name
    pub ports: HashMap<u16, String>,                  // port -> vm_name
    pub sockets: HashMap<PathBuf, String>,            // socket_path -> vm_name
}

#[derive(Debug, Default)]
pub struct ConflictReport {
    pub errors: Vec<String>,
    pub warnings: Vec<String>,
    pub conflicting_vms: HashSet<String>,
    pub valid_vms: HashSet<String>,
}

impl ResourceRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    /// Canonicalize a path, following symlinks if target exists (e.g. LVM /dev/vg/name -> /dev/dm-X)
    pub fn canonicalize_path(p: &Path) -> PathBuf {
        fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf())
    }

    /// Load all configs from the global search directories and detect conflicts.
    pub fn build_and_validate(global: &GlobalConfig) -> (Self, ConflictReport) {
        let search_dirs = &global.config_dirs;
        let mut registry = Self::new();
        let mut report = ConflictReport::default();
        let mut loaded_configs: Vec<(PathBuf, VmConfig)> = Vec::new();

        // 1. Discover and load all JSON configs
        for dir in search_dirs {
            if !dir.is_dir() {
                continue;
            }
            if let Ok(entries) = fs::read_dir(dir) {
                for entry in entries.flatten() {
                    let path = entry.path();
                    if path.is_file() && is_vm_config_file(&path) {
                        match VmConfig::load_from_file(&path) {
                            Ok(cfg) => {
                                // Check duplicate VM names across files
                                if let Some(existing_path) = registry.names.get(&cfg.name) {
                                    report.errors.push(format!(
                                        "Duplicate VM name '{}': Defined in both '{}' and '{}'",
                                        cfg.name,
                                        existing_path.display(),
                                        path.display()
                                    ));
                                    report.conflicting_vms.insert(cfg.name.clone());
                                    continue;
                                }
                                registry.names.insert(cfg.name.clone(), path.clone());
                                loaded_configs.push((path, cfg));
                            }
                            Err(e) => {
                                report.errors.push(format!(
                                    "Failed to parse config '{}': {}",
                                    path.display(),
                                    e
                                ));
                            }
                        }
                    }
                }
            }
        }

        if let Err(err) = mac::fill_configs(&mut loaded_configs) {
            report.errors.push(format!("MAC allocation failed: {err}"));
        }
        for (_, cfg) in &loaded_configs {
            registry.configs.insert(cfg.name.clone(), cfg.clone());
        }

        // 2. Scan and detect resource collisions across all loaded configs
        for (_path, cfg) in &loaded_configs {
            let mut vm_has_conflict = false;

            // (a) Disk conflict check
            for disk in &cfg.disks {
                let can_path = Self::canonicalize_path(&disk.path);
                let entry = registry.disks.entry(can_path.clone()).or_default();
                entry.push((cfg.name.clone(), disk.readonly));

                // If this disk is referenced by more than 1 VM, verify read-write exclusivity
                if entry.len() > 1 {
                    let any_writable = entry.iter().any(|(_, ro)| !ro);
                    if any_writable {
                        let participants: Vec<String> = entry
                            .iter()
                            .map(|(n, ro)| format!("{}({})", n, if *ro { "ro" } else { "rw" }))
                            .collect();

                        report.errors.push(format!(
                            "Disk Conflict on '{}': Writable access declared simultaneously by multiple VMs: [{}]",
                            can_path.display(),
                            participants.join(", ")
                        ));
                        for (p_name, _) in entry {
                            report.conflicting_vms.insert(p_name.clone());
                        }
                        vm_has_conflict = true;
                    } else {
                        report.warnings.push(format!(
                            "Disk '{}' shared as read-only among multiple VMs.",
                            can_path.display()
                        ));
                    }
                }
            }

            if let Err(e) = settings::check_boot(global, cfg) {
                report.errors.push(format!("VM '{}': {e}", cfg.name));
                report.conflicting_vms.insert(cfg.name.clone());
                vm_has_conflict = true;
            }

            // (b) Network MAC and Interface Name conflict check
            let total_nets = cfg.networks.len();

            for (idx, net) in cfg.networks.iter().enumerate() {
                let ifname = match settings::interface_name(
                    global,
                    &cfg.backend,
                    &cfg.name,
                    net.name.as_deref(),
                    idx,
                    total_nets,
                ) {
                    Ok(name) => name,
                    Err(e) => {
                        report.errors.push(format!("VM '{}': {e}", cfg.name));
                        report.conflicting_vms.insert(cfg.name.clone());
                        vm_has_conflict = true;
                        continue;
                    }
                };

                if let Some(existing_owner) = registry.interfaces.get(&ifname) {
                    report.errors.push(format!(
                        "Interface Name Conflict: '{}' is used by both '{}' and '{}'",
                        ifname, existing_owner, cfg.name
                    ));
                    report.conflicting_vms.insert(existing_owner.clone());
                    report.conflicting_vms.insert(cfg.name.clone());
                    vm_has_conflict = true;
                } else {
                    registry.interfaces.insert(ifname, cfg.name.clone());
                }

                // Check MAC collision
                if let Some(mac_str) = &net.mac {
                    let mac_norm = mac_str.to_lowercase();
                    let mac_entry = registry.macs.entry(mac_norm.clone()).or_default();
                    mac_entry.push((cfg.name.clone(), net.parent.clone()));

                    if mac_entry.len() > 1 {
                        let participants: Vec<String> = mac_entry
                            .iter()
                            .map(|(n, p)| format!("{}(on {})", n, p))
                            .collect();

                        report.errors.push(format!(
                            "MAC Collision: Address '{}' is duplicated across VMs: [{}]",
                            mac_str,
                            participants.join(", ")
                        ));
                        for (p_name, _) in mac_entry {
                            report.conflicting_vms.insert(p_name.clone());
                        }
                        vm_has_conflict = true;
                    }
                }
            }

            // (c) Port conflict check (SPICE)
            if let Some(sp) = &cfg.spice {
                if sp.enable {
                    let port = sp.port.unwrap_or(5900);
                    if let Some(existing_owner) = registry.ports.get(&port) {
                        report.errors.push(format!(
                            "SPICE Port Conflict: Port {} is used by both '{}' and '{}'",
                            port, existing_owner, cfg.name
                        ));
                        report.conflicting_vms.insert(existing_owner.clone());
                        report.conflicting_vms.insert(cfg.name.clone());
                        vm_has_conflict = true;
                    } else {
                        registry.ports.insert(port, cfg.name.clone());
                    }
                }
            }

            // (d) Socket path conflict check
            let mut sockets_to_check = global.runtime_ipc_sockets(&cfg.backend, &cfg.name);
            match settings::resolve_serial(global, cfg) {
                Ok(SerialTarget::Socket(path)) => sockets_to_check.push(path),
                Ok(_) => {}
                Err(e) => {
                    report.errors.push(format!("VM '{}': {e}", cfg.name));
                    report.conflicting_vms.insert(cfg.name.clone());
                    vm_has_conflict = true;
                }
            }

            for sock in sockets_to_check {
                if let Some(existing_owner) = registry.sockets.get(&sock) {
                    report.errors.push(format!(
                        "Socket Path Conflict: '{}' is shared between '{}' and '{}'",
                        sock.display(),
                        existing_owner,
                        cfg.name
                    ));
                    report.conflicting_vms.insert(existing_owner.clone());
                    report.conflicting_vms.insert(cfg.name.clone());
                    vm_has_conflict = true;
                } else {
                    registry.sockets.insert(sock, cfg.name.clone());
                }
            }

            if !vm_has_conflict && !report.conflicting_vms.contains(&cfg.name) {
                report.valid_vms.insert(cfg.name.clone());
            }
        }

        // Clean up valid_vms if marked later
        for c_vm in &report.conflicting_vms {
            report.valid_vms.remove(c_vm);
        }

        (registry, report)
    }

    /// List all registered resources in tabular representation
    pub fn list_resources(&self) -> Vec<ResourceEntry> {
        let mut list = Vec::new();

        // Disks
        for (path, vms) in &self.disks {
            for (vm, ro) in vms {
                list.push(ResourceEntry {
                    vm_name: vm.clone(),
                    resource_type: "DISK".to_string(),
                    identifier: path.display().to_string(),
                    detail: if *ro {
                        "ReadOnly (Shared)".to_string()
                    } else {
                        "ReadWrite (Exclusive)".to_string()
                    },
                });
            }
        }

        // MACs
        for (mac, vms) in &self.macs {
            for (vm, parent) in vms {
                list.push(ResourceEntry {
                    vm_name: vm.clone(),
                    resource_type: "MAC".to_string(),
                    identifier: mac.clone(),
                    detail: format!("Parent: {}", parent),
                });
            }
        }

        // Interfaces
        for (ifname, vm) in &self.interfaces {
            list.push(ResourceEntry {
                vm_name: vm.clone(),
                resource_type: "NET-IF".to_string(),
                identifier: ifname.clone(),
                detail: "Virtual Device Node".to_string(),
            });
        }

        // Ports
        for (port, vm) in &self.ports {
            list.push(ResourceEntry {
                vm_name: vm.clone(),
                resource_type: "PORT".to_string(),
                identifier: format!("{}/tcp", port),
                detail: "SPICE Remote Desktop".to_string(),
            });
        }

        // Sockets
        for (sock, vm) in &self.sockets {
            list.push(ResourceEntry {
                vm_name: vm.clone(),
                resource_type: "SOCKET".to_string(),
                identifier: sock.display().to_string(),
                detail: "Control/Serial IPC".to_string(),
            });
        }

        list.sort_by(|a, b| {
            a.resource_type
                .cmp(&b.resource_type)
                .then(a.vm_name.cmp(&b.vm_name))
        });
        list
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::GlobalConfig;
    use std::fs;

    #[test]
    fn disk_and_interface_prefix_conflicts_are_reported() {
        let dir = std::env::temp_dir().join(format!("mvmctl-reg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let disk = dir.join("disk.img");
        fs::write(&disk, b"x").unwrap();
        let disk_s = disk.display().to_string();

        fs::write(
            dir.join("app.json"),
            format!(
                r#"{{
                    "name":"app",
                    "backend":"qemu",
                    "boot":{{"mode":"direct","kernel":"/tmp/vmlinuz"}},
                    "disks":[{{"path":"{disk_s}"}}],
                    "networks":[
                        {{"parent":"eth0","mac":"52:54:00:00:00:01"}},
                        {{"parent":"eth0","mac":"52:54:00:00:00:02"}}
                    ]
                }}"#
            ),
        )
        .unwrap();
        fs::write(
            dir.join("app-1.json"),
            format!(
                r#"{{
                    "name":"app-1",
                    "backend":"qemu",
                    "boot":{{"mode":"direct","kernel":"/tmp/vmlinuz"}},
                    "disks":[{{"path":"{disk_s}"}}],
                    "networks":[{{"parent":"eth0","mac":"52:54:00:00:00:03"}}]
                }}"#
            ),
        )
        .unwrap();

        let global = GlobalConfig {
            config_dirs: vec![dir.clone()],
            ..GlobalConfig::default()
        };
        let (_registry, report) = ResourceRegistry::build_and_validate(&global);
        let text = report.errors.join("\n");
        assert!(text.contains("Disk Conflict"), "{text}");
        assert!(text.contains("qu-app-1"), "{text}");
        assert!(report.conflicting_vms.contains("app"));
        assert!(report.conflicting_vms.contains("app-1"));

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn config_json_is_skipped_and_empty_macs_stay_on_disk() {
        let dir = std::env::temp_dir().join(format!("mvmctl-macreg-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).unwrap();
        let global_body = "{\"stop_timeout_secs\":12}\n";
        fs::write(dir.join("config.json"), global_body).unwrap();
        fs::write(
            dir.join("app.json"),
            r#"{"name":"app","backend":"qemu","boot":{"mode":"direct","kernel":"/tmp/vmlinuz"},"networks":[{"parent":"eth0","mac":""}]}"#,
        )
        .unwrap();
        fs::write(
            dir.join("zeta.json"),
            r#"{"name":"zeta","backend":"qemu","boot":{"mode":"direct","kernel":"/tmp/vmlinuz"},"networks":[{"parent":"eth0"}]}"#,
        )
        .unwrap();

        let global = GlobalConfig {
            config_dirs: vec![dir.clone()],
            ..GlobalConfig::default()
        };
        let (registry, report) = ResourceRegistry::build_and_validate(&global);
        let text = report.errors.join("\n");
        assert!(!text.contains("config.json"), "{text}");
        assert!(!report.conflicting_vms.contains("app"), "{text}");
        let app = registry.configs["app"].networks[0].mac.as_deref().unwrap();
        let zeta = registry.configs["zeta"].networks[0].mac.as_deref().unwrap();
        assert!(app.starts_with("52:54:00:"), "{app}");
        assert!(zeta.starts_with("52:54:00:"), "{zeta}");
        assert_ne!(app, zeta);
        assert_eq!(registry.macs.get(app).map(|rows| rows.len()), Some(1));
        let raw = fs::read_to_string(dir.join("app.json")).unwrap();
        assert!(raw.contains("\"mac\":\"\""), "{raw}");
        assert_eq!(
            fs::read_to_string(dir.join("config.json")).unwrap(),
            global_body
        );

        let _ = fs::remove_dir_all(&dir);
    }
}
