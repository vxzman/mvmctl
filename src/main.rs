use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};

mod backend;
mod config;
mod console;
mod control;
mod generator;
mod network;
mod plan;
mod registry;
mod settings;

use backend::ch::CloudHypervisorBackend;
use backend::qemu::QemuBackend;
use backend::HypervisorBackend;
use config::{BackendType, VmConfig};
use generator::SystemdGenerator;
use plan::ResolvedPlan;
use registry::ResourceRegistry;
use settings::GlobalConfig;

#[derive(Parser)]
#[command(
    name = "mvmctl",
    author = "Antigravity & kaishuai",
    version = env!("CARGO_PKG_VERSION"),
    about = "MicroVM controller and systemd Quadlet-style generator with collision detection"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run a MicroVM in the foreground (used by systemd ExecStart or manual debug)
    Run {
        /// Path to JSON config file or VM name (searched in /etc/mvmctl/)
        target: String,
    },

    /// Gracefully stop a running MicroVM (used by systemd ExecStop)
    Stop {
        /// Path to JSON config file or VM name
        target: String,
    },

    /// Clean up network interfaces and sockets (used by systemd ExecStopPost)
    Clean {
        /// Path to JSON config file or VM name
        target: String,
    },

    /// Generate systemd service unit file(s)
    Generate {
        /// Path to JSON config file or VM name
        target: Option<String>,

        /// Write unit file directly to /etc/systemd/system/
        #[arg(short, long)]
        write: bool,

        /// Output directory or file destination
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// Process all JSON configurations in /etc/mvmctl/
        #[arg(short, long)]
        all: bool,
    },

    /// List all configured MicroVMs and their status
    List,

    /// Connect to a running MicroVM's serial console
    #[command(alias = "attach")]
    Console {
        /// VM name or path to JSON config
        target: String,
    },

    /// Perform global configuration & resource collision checks (MAC, Disks, Ports, Interfaces)
    Check,

    /// Display global resource allocation ledger (MACs, disks, network interfaces, ports)
    Resources,

    /// Inspect fully resolved execution plan for a VM
    Inspect {
        /// VM name or path to JSON config
        target: String,
    },

    /// Validate a single JSON configuration file
    Validate {
        /// Path to JSON config file
        target: String,
    },

    /// Internal systemd generator mode
    #[command(name = "systemd-generator")]
    SystemdGen {
        normal_dir: PathBuf,
        early_dir: Option<PathBuf>,
        late_dir: Option<PathBuf>,
    },
}

fn get_backend(backend_type: &BackendType) -> Box<dyn HypervisorBackend> {
    match backend_type {
        BackendType::Qemu => Box::new(QemuBackend),
        BackendType::CloudHypervisor => Box::new(CloudHypervisorBackend),
    }
}

fn load_vm(global: &GlobalConfig, target: &str) -> Result<(PathBuf, VmConfig)> {
    let found = VmConfig::find_and_load(target, &global.config_dirs)?;
    settings::check_vm(global, &found.1)?;
    Ok(found)
}

fn is_systemd_generator_env(args: &[String]) -> bool {
    let prog = &args[0];
    if prog.contains("generator") {
        return true;
    }
    if args.len() >= 2 && args.len() <= 4 {
        let first = &args[1];
        if !first.starts_with('-')
            && !matches!(
                first.as_str(),
                "run"
                    | "stop"
                    | "clean"
                    | "generate"
                    | "list"
                    | "console"
                    | "attach"
                    | "check"
                    | "resources"
                    | "inspect"
                    | "validate"
                    | "systemd-generator"
                    | "help"
            )
        {
            if Path::new(first).is_dir() || first.contains("generator") {
                return true;
            }
        }
    }
    false
}

fn main() -> Result<()> {
    let raw_args: Vec<String> = std::env::args().collect();
    let global = GlobalConfig::load()?;

    // Check if called directly by systemd as a generator
    if is_systemd_generator_env(&raw_args) {
        let output_dir = Path::new(&raw_args[1]);
        return SystemdGenerator::run_systemd_generator(&global, output_dir);
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run { target }) => {
            let (_path, config) = load_vm(&global, &target)?;
            let backend = get_backend(&config.backend);
            backend.run(&config, &global)?;
        }

        Some(Commands::Stop { target }) => {
            let (_path, config) = load_vm(&global, &target)?;
            let backend = get_backend(&config.backend);
            backend.stop(&config, &global)?;
        }

        Some(Commands::Clean { target }) => {
            let (_path, config) = load_vm(&global, &target)?;
            let backend = get_backend(&config.backend);
            backend.clean(&config, &global)?;
        }

        Some(Commands::Generate {
            target,
            write,
            output,
            all,
        }) => {
            if all {
                let dest = output.unwrap_or_else(|| global.systemd.unit_dir.clone());
                println!(
                    "Generating units for configs in {:?} into '{}'...",
                    global.config_dirs,
                    dest.display()
                );
                SystemdGenerator::run_systemd_generator(&global, &dest)?;
                println!("Done.");
                return Ok(());
            }

            let target_str = target
                .ok_or_else(|| anyhow!("Please specify a target configuration or use '--all'"))?;
            let (path, config) = load_vm(&global, &target_str)?;

            if write {
                let dest_dir = output.unwrap_or_else(|| global.systemd.unit_dir.clone());
                let written =
                    SystemdGenerator::write_static_unit(&global, &config, &path, &dest_dir)?;
                println!("Wrote systemd unit to: {}", written.display());
            } else if let Some(out_path) = output {
                if out_path.is_dir() {
                    let written =
                        SystemdGenerator::write_static_unit(&global, &config, &path, &out_path)?;
                    println!("Wrote systemd unit to: {}", written.display());
                } else {
                    let content = SystemdGenerator::generate_unit(&global, &config, &path);
                    fs::write(&out_path, content)?;
                    println!("Wrote unit to: {}", out_path.display());
                }
            } else {
                let content = SystemdGenerator::generate_unit(&global, &config, &path);
                print!("{}", content);
            }
        }

        Some(Commands::List) => {
            list_vms(&global)?;
        }

        Some(Commands::Console { target }) => {
            console::run_console(&global, &target)?;
        }

        Some(Commands::Check) => {
            run_check(&global)?;
        }

        Some(Commands::Resources) => {
            run_resources(&global)?;
        }

        Some(Commands::Inspect { target }) => {
            let (_path, config) = load_vm(&global, &target)?;
            let plan = ResolvedPlan::compile(&global, &config)?;
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }

        Some(Commands::Validate { target }) => {
            let (path, config) = load_vm(&global, &target)?;
            println!("Configuration in '{}' is valid!", path.display());
            println!("  VM Name: {}", config.name);
            println!("  Backend: {}", config.backend);
            println!("  Boot Mode: {:?}", config.boot.mode);
            println!("  CPUs: {}, Memory: {}", config.cpus, config.memory);
            println!("  Disks: {}", config.disks.len());
            println!("  Networks: {}", config.networks.len());
            for warning in settings::path_warnings(&global, &config) {
                println!("  [!] {warning}");
            }
        }

        Some(Commands::SystemdGen { normal_dir, .. }) => {
            SystemdGenerator::run_systemd_generator(&global, &normal_dir)?;
        }

        None => {
            println!("mvmctl - MicroVM Controller. Run 'mvmctl --help' for usage.");
        }
    }

    Ok(())
}

fn run_check(global: &GlobalConfig) -> Result<()> {
    println!(
        "Scanning configuration directories: {:?}",
        global.config_dirs
    );
    let (registry, report) = ResourceRegistry::build_and_validate(global);

    println!("\n=== Global Resource & Conflict Audit ===");
    println!("Discovered Virtual Machines: {}", registry.names.len());

    let mut all_ok = true;

    // Report Disks
    let mut disk_errors = 0;
    for err in &report.errors {
        if err.starts_with("Disk Conflict") {
            eprintln!("  [✗] {}", err);
            disk_errors += 1;
            all_ok = false;
        }
    }
    if disk_errors == 0 {
        println!("  [✓] Disk Exclusivity: OK (No concurrent write collisions)");
    }

    // Report MACs
    let mut mac_errors = 0;
    for err in &report.errors {
        if err.starts_with("MAC Collision") {
            eprintln!("  [✗] {}", err);
            mac_errors += 1;
            all_ok = false;
        }
    }
    if mac_errors == 0 {
        println!("  [✓] MAC Addresses: OK (No duplicates found)");
    }

    // Report Interface names
    let mut if_errors = 0;
    for err in &report.errors {
        if err.starts_with("Interface Name Conflict") {
            eprintln!("  [✗] {}", err);
            if_errors += 1;
            all_ok = false;
        }
    }
    if if_errors == 0 {
        println!("  [✓] Virtual Network Interfaces: OK (Unique names guaranteed)");
    }

    // Report Ports & Sockets
    let mut port_errors = 0;
    for err in &report.errors {
        if err.starts_with("SPICE Port") || err.starts_with("Socket Path") {
            eprintln!("  [✗] {}", err);
            port_errors += 1;
            all_ok = false;
        }
    }
    if port_errors == 0 {
        println!("  [✓] Ports & IPC Sockets: OK (No overlaps)");
    }

    // Other errors
    for err in &report.errors {
        if !err.starts_with("Disk Conflict")
            && !err.starts_with("MAC Collision")
            && !err.starts_with("Interface Name Conflict")
            && !err.starts_with("SPICE Port")
            && !err.starts_with("Socket Path")
        {
            eprintln!("  [✗] {}", err);
            all_ok = false;
        }
    }

    for warn in &report.warnings {
        println!("  [!] Notice: {}", warn);
    }

    println!("------------------------------------------------------------");
    if all_ok {
        println!(
            "Result: All {} VM configuration(s) are valid and 100% CONFLICT-FREE!",
            registry.names.len()
        );
        Ok(())
    } else {
        println!(
            "Result: Found {} conflict error(s)! Conflicting VMs: {:?}",
            report.errors.len(),
            report.conflicting_vms
        );
        anyhow::bail!("Resource collision check failed.");
    }
}

fn run_resources(global: &GlobalConfig) -> Result<()> {
    let (registry, _) = ResourceRegistry::build_and_validate(global);
    let entries = registry.list_resources();

    println!(
        "{:<8} {:<16} {:<32} {}",
        "TYPE", "VM", "IDENTIFIER", "DETAIL"
    );
    println!("{}", "-".repeat(80));

    for item in entries {
        println!(
            "{:<8} {:<16} {:<32} {}",
            item.resource_type, item.vm_name, item.identifier, item.detail
        );
    }

    Ok(())
}

fn list_vms(global: &GlobalConfig) -> Result<()> {
    println!(
        "{:<16} {:<16} {:<10} {:<8} {:<10} {:<12}",
        "NAME", "BACKEND", "CPUS", "MEM", "AUTOSTART", "STATUS"
    );
    println!("{}", "-".repeat(76));

    let (registry, report) = ResourceRegistry::build_and_validate(global);
    for err in &report.errors {
        eprintln!("  [!] {err}");
    }

    let mut rows = Vec::new();
    for (name, path) in &registry.names {
        if let Ok(cfg) = VmConfig::load_from_file(path) {
            let status = if report.conflicting_vms.contains(name) {
                "conflict".to_string()
            } else {
                control::vm_status(global, &cfg)
            };
            rows.push((cfg, status));
        }
    }
    rows.sort_by(|a, b| a.0.name.cmp(&b.0.name));

    if rows.is_empty() && report.errors.is_empty() {
        println!("No VM configurations found in {:?}", global.config_dirs);
        return Ok(());
    }

    for (cfg, status) in &rows {
        println!(
            "{:<16} {:<16} {:<10} {:<8} {:<10} {:<12}",
            cfg.name,
            cfg.backend.to_string(),
            cfg.cpus,
            cfg.memory,
            if cfg.autostart { "yes" } else { "no" },
            status
        );
    }

    Ok(())
}
