use std::io::{BufRead, BufReader};
use std::process::ChildStderr;
use std::thread::{self, JoinHandle};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HypervisorLine {
    Drop,
    Info(String),
    Warn(String),
    Error(String),
    Forward(String),
}

pub fn info(msg: impl AsRef<str>) {
    println!("mvmctl: {}", msg.as_ref());
}

pub fn warn(msg: impl AsRef<str>) {
    eprintln!("mvmctl: warning: {}", msg.as_ref());
}

pub fn error(msg: impl AsRef<str>) {
    eprintln!("mvmctl: error: {}", msg.as_ref());
}

pub fn hypervisor(backend: &str, line: &str) {
    match interpret_hypervisor(backend, line) {
        HypervisorLine::Drop => {}
        HypervisorLine::Info(msg) => info(msg),
        HypervisorLine::Warn(msg) => warn(msg),
        HypervisorLine::Error(msg) => error(msg),
        HypervisorLine::Forward(msg) => eprintln!("{msg}"),
    }
}

/// Pump hypervisor stderr through [`hypervisor`]. `inspect` sees every raw line
/// first so lifecycle parsers can still match reset/shutdown text.
pub fn spawn_stderr_pump(
    stderr: Option<ChildStderr>,
    backend: &'static str,
    mut inspect: impl FnMut(&str) + Send + 'static,
) -> JoinHandle<()> {
    thread::spawn(move || {
        let Some(stderr) = stderr else {
            return;
        };
        let mut reader = BufReader::new(stderr);
        let mut buf = Vec::new();
        loop {
            buf.clear();
            match reader.read_until(b'\n', &mut buf) {
                Ok(0) => return,
                Ok(_) => {
                    while matches!(buf.last(), Some(&b'\n' | &b'\r')) {
                        buf.pop();
                    }
                    let line = String::from_utf8_lossy(&buf);
                    inspect(&line);
                    hypervisor(backend, &line);
                }
                Err(_) => return,
            }
        }
    })
}

pub fn interpret_hypervisor(backend: &str, line: &str) -> HypervisorLine {
    let line = line.trim();
    if line.is_empty() {
        return HypervisorLine::Drop;
    }
    if let Some(name) = tap_already_exists_name(line) {
        return HypervisorLine::Info(format!(
            "tap {name} already on the host; {backend} reused it without changing host IP"
        ));
    }
    if line.contains("IP configuration will not be overwritten") {
        return HypervisorLine::Drop;
    }
    if line.contains("TUNSETSTEERINGEBPF") {
        return HypervisorLine::Drop;
    }
    if let Some((level, message)) = parse_ch_line(line) {
        return match level {
            "ERROR" => HypervisorLine::Error(format!("{backend}: {message}")),
            "WARN" => HypervisorLine::Warn(format!("{backend}: {message}")),
            "DEBUG" => HypervisorLine::Drop,
            _ => HypervisorLine::Forward(format!("{backend}: {message}")),
        };
    }
    HypervisorLine::Forward(line.to_string())
}

fn tap_already_exists_name(line: &str) -> Option<&str> {
    let after = line.split_once("Tap ")?.1;
    let name = after.split_once(" already exists")?.0.trim();
    if name.is_empty() || name.contains(char::is_whitespace) {
        None
    } else {
        Some(name)
    }
}

fn parse_ch_line(line: &str) -> Option<(&str, &str)> {
    let body = line
        .strip_prefix("cloud-hypervisor:")
        .unwrap_or(line)
        .trim();
    let (head, message) = body.rsplit_once(" -- ")?;
    let message = message.trim();
    if message.is_empty() {
        return None;
    }
    let level = ["ERROR", "WARN", "INFO", "DEBUG"]
        .into_iter()
        .find(|item| head.contains(item))?;
    Some((level, message))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tap_reuse_becomes_mvmctl_info() {
        let line = "cloud-hypervisor:   0.013299s: <vmm> WARN:net_util/src/open_tap.rs:84 -- Tap ch-rhel already exists. IP configuration will not be overwritten.";
        assert_eq!(
            interpret_hypervisor("cloud-hypervisor", line),
            HypervisorLine::Info(
                "tap ch-rhel already on the host; cloud-hypervisor reused it without changing host IP"
                    .to_string()
            )
        );
    }

    #[test]
    fn wrapped_ip_warning_is_dropped() {
        assert_eq!(
            interpret_hypervisor(
                "cloud-hypervisor",
                "IP configuration will not be overwritten."
            ),
            HypervisorLine::Drop
        );
    }

    #[test]
    fn qemu_ebpf_steering_noise_is_dropped() {
        let line = "qemu-system-x86_64: -device virtio-net-pci,netdev=net0,mac=52:54:00:23:94:3f: Issue while setting TUNSETSTEERINGEBPF: Invalid argument with fd: 3, prog_fd: -1";
        assert_eq!(interpret_hypervisor("qemu", line), HypervisorLine::Drop);
    }

    #[test]
    fn other_ch_warn_keeps_the_message() {
        let line = "cloud-hypervisor:   1.2s: <vmm> WARN:vmm/src/vm.rs:10 -- disk queue is full";
        assert_eq!(
            interpret_hypervisor("cloud-hypervisor", line),
            HypervisorLine::Warn("cloud-hypervisor: disk queue is full".to_string())
        );
    }

    #[test]
    fn ch_info_strips_timestamp() {
        let line = "cloud-hypervisor:   0.01s: <vmm> INFO:vmm/src/vm.rs:1 -- VM reset event";
        assert_eq!(
            interpret_hypervisor("cloud-hypervisor", line),
            HypervisorLine::Forward("cloud-hypervisor: VM reset event".to_string())
        );
    }
}
