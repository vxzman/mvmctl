use anyhow::{anyhow, Context, Result};
use std::fs::{self, File};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::io::AsRawFd;

use crate::config::VmConfig;
use crate::settings::GlobalConfig;

pub trait HypervisorBackend {
    fn run(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()>;
    fn stop(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()>;
    fn clean(&self, config: &VmConfig, global: &GlobalConfig) -> Result<()>;
}

/// Non-blocking exclusive lock on every writable disk.
///
/// The returned files hold the lock until they are dropped. Cloud-Hypervisor
/// v46 and later takes this same lock itself, so that backend must drop the
/// guard before spawn. QEMU does not lock, so the guard stays alive until the
/// process exits.
pub fn lock_writable_disks(config: &VmConfig) -> Result<Vec<File>> {
    let mut locks = Vec::new();
    for disk in &config.disks {
        if disk.readonly {
            continue;
        }
        if !disk.path.exists() {
            return Err(anyhow!("Disk '{}' does not exist", disk.path.display()));
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&disk.path)
            .with_context(|| {
                format!("Failed to open disk '{}' for locking", disk.path.display())
            })?;
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc != 0 {
            return Err(anyhow!(
                "Disk '{}' is locked by another process. Refusing to start to avoid filesystem corruption.",
                disk.path.display()
            ));
        }
        locks.push(file);
    }
    Ok(locks)
}

/// Ignore SIGINT in the parent only. Call this after `spawn`: the child keeps
/// the default disposition, dies on Ctrl+C, and `wait` returns so network
/// cleanup still runs.
pub fn ignore_sigint() {
    unsafe {
        libc::signal(libc::SIGINT, libc::SIG_IGN);
    }
}

pub fn ensure_runtime_dir(global: &GlobalConfig) -> Result<()> {
    fs::create_dir_all(&global.runtime_dir)
        .with_context(|| format!("Failed to create '{}'", global.runtime_dir.display()))?;
    let mode = global.runtime_mode()?;
    fs::set_permissions(&global.runtime_dir, fs::Permissions::from_mode(mode))?;
    Ok(())
}

pub mod ch;
pub mod qemu;
