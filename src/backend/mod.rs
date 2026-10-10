use anyhow::Result;
use crate::config::VmConfig;

pub trait HypervisorBackend {
    fn run(&self, config: &VmConfig) -> Result<()>;
    fn stop(&self, config: &VmConfig) -> Result<()>;
    fn clean(&self, config: &VmConfig) -> Result<()>;
}

pub mod ch;
pub mod qemu;
