use anyhow::{anyhow, Result};
use clap::{Parser, Subcommand};
use std::fs;
use std::path::{Path, PathBuf};

mod backend;
mod config;
mod console;
mod generator;
mod network;

use backend::ch::CloudHypervisorBackend;
use backend::qemu::QemuBackend;
use backend::HypervisorBackend;
use config::{BackendType, VmConfig};
use generator::SystemdGenerator;

#[derive(Parser)]
#[command(
    name = "mvmctl",
    author = "Antigravity & kaishuai",
    version = "0.1.0",
    about = "MicroVM controller and systemd Quadlet-style generator for QEMU & Cloud-Hypervisor"
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

    /// Validate a JSON configuration file
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
                "run" | "stop" | "clean" | "generate" | "list" | "validate" | "systemd-generator" | "help"
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

    // Check if called directly by systemd as a generator
    if is_systemd_generator_env(&raw_args) {
        let output_dir = Path::new(&raw_args[1]);
        return SystemdGenerator::run_systemd_generator(output_dir);
    }

    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Run { target }) => {
            let (_path, config) = VmConfig::find_and_load(&target)?;
            let backend = get_backend(&config.backend);
            backend.run(&config)?;
        }

        Some(Commands::Stop { target }) => {
            let (_path, config) = VmConfig::find_and_load(&target)?;
            let backend = get_backend(&config.backend);
            backend.stop(&config)?;
        }

        Some(Commands::Clean { target }) => {
            let (_path, config) = VmConfig::find_and_load(&target)?;
            let backend = get_backend(&config.backend);
            backend.clean(&config)?;
        }

        Some(Commands::Generate {
            target,
            write,
            output,
            all,
        }) => {
            if all {
                let dest = output.unwrap_or_else(|| PathBuf::from("/etc/systemd/system"));
                println!("Generating units for all configs in /etc/mvmctl into '{}'...", dest.display());
                SystemdGenerator::run_systemd_generator(&dest)?;
                println!("Done.");
                return Ok(());
            }

            let target_str = target.ok_or_else(|| {
                anyhow!("Please specify a target configuration or use '--all'")
            })?;
            let (path, config) = VmConfig::find_and_load(&target_str)?;

            if write {
                let dest_dir = output.unwrap_or_else(|| PathBuf::from("/etc/systemd/system"));
                let written = SystemdGenerator::write_static_unit(&config, &path, &dest_dir)?;
                println!("Wrote systemd unit to: {}", written.display());
            } else if let Some(out_path) = output {
                if out_path.is_dir() {
                    let written = SystemdGenerator::write_static_unit(&config, &path, &out_path)?;
                    println!("Wrote systemd unit to: {}", written.display());
                } else {
                    let content = SystemdGenerator::generate_unit(&config, &path);
                    fs::write(&out_path, content)?;
                    println!("Wrote unit to: {}", out_path.display());
                }
            } else {
                let content = SystemdGenerator::generate_unit(&config, &path);
                print!("{}", content);
            }
        }

        Some(Commands::List) => {
            list_vms()?;
        }

        Some(Commands::Console { target }) => {
            console::run_console(&target)?;
        }

        Some(Commands::Validate { target }) => {
            let (path, config) = VmConfig::find_and_load(&target)?;
            println!("Configuration in '{}' is valid!", path.display());
            println!("  VM Name: {}", config.name);
            println!("  Backend: {}", config.backend);
            println!("  Boot Mode: {:?}", config.boot.mode);
            println!("  CPUs: {}, Memory: {}", config.cpus, config.memory);
            println!("  Disks: {}", config.disks.len());
            println!("  Networks: {}", config.networks.len());
        }

        Some(Commands::SystemdGen { normal_dir, .. }) => {
            SystemdGenerator::run_systemd_generator(&normal_dir)?;
        }

        None => {
            println!("mvmctl - MicroVM Controller. Run 'mvmctl --help' for usage.");
        }
    }

    Ok(())
}

fn list_vms() -> Result<()> {
    let mut dirs = Vec::new();
    if let Ok(custom) = std::env::var("MVMCTL_CONFIG_DIR") {
        dirs.push(PathBuf::from(custom));
    }
    dirs.push(PathBuf::from("/etc/mvmctl"));
    dirs.push(PathBuf::from("/etc/mvmctl/vms"));
    println!("{:<16} {:<16} {:<10} {:<8} {:<10} {:<12}", "NAME", "BACKEND", "CPUS", "MEM", "AUTOSTART", "STATUS");
    println!("{}", "-".repeat(76));

    let mut found = 0;
    for dir in &dirs {
        if !dir.is_dir() {
            continue;
        }
        if let Ok(entries) = fs::read_dir(dir) {
            for entry in entries.flatten() {
                let p = entry.path();
                if p.extension().and_then(|s| s.to_str()) == Some("json") {
                    if let Ok(cfg) = VmConfig::load_from_file(&p) {
                        found += 1;
                        let status = check_vm_status(&cfg);
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
                }
            }
        }
    }

    if found == 0 {
        println!("No VM configurations found in /etc/mvmctl/");
    }

    Ok(())
}

fn check_vm_status(cfg: &VmConfig) -> &'static str {
    match cfg.backend {
        BackendType::Qemu => {
            let sock = format!("/run/mvmctl/{}-qmp.sock", cfg.name);
            if Path::new(&sock).exists() {
                "running"
            } else {
                "stopped"
            }
        }
        BackendType::CloudHypervisor => {
            let sock = format!("/run/mvmctl/{}-api.sock", cfg.name);
            if Path::new(&sock).exists() {
                "running"
            } else {
                "stopped"
            }
        }
    }
}
