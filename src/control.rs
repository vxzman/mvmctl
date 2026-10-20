use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use crate::config::{BackendType, VmConfig};
use crate::settings::{self, GlobalConfig};

pub fn vm_status(global: &GlobalConfig, cfg: &VmConfig) -> String {
    let sock = global.control_socket(&cfg.backend, &cfg.name);
    if !sock.exists() {
        return "stopped".to_string();
    }
    match cfg.backend {
        BackendType::Qemu => match qmp_execute(&sock, "query-status") {
            Ok(v) => v
                .get("return")
                .and_then(|r| r.get("status"))
                .and_then(|s| s.as_str())
                .unwrap_or("unknown")
                .to_ascii_lowercase(),
            Err(_) => "unknown".to_string(),
        },
        BackendType::CloudHypervisor => match ch_request(&sock, "GET", "/api/v1/vm.info") {
            Ok((code, body)) if (200..300).contains(&code) => serde_json::from_str::<Value>(&body)
                .ok()
                .and_then(|v| {
                    v.get("state")
                        .and_then(|s| s.as_str())
                        .map(|s| s.to_ascii_lowercase())
                })
                .unwrap_or_else(|| "unknown".to_string()),
            Ok(_) => "unknown".to_string(),
            Err(_) => "unknown".to_string(),
        },
    }
}

pub fn qmp_execute(sock: &Path, command: &str) -> Result<Value> {
    let mut stream = UnixStream::connect(sock)
        .with_context(|| format!("Failed to connect to QMP socket '{}'", sock.display()))?;
    let timeout = Duration::from_millis(800);
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let mut reader = BufReader::new(stream.try_clone()?);
    let _greeting = read_line(&mut reader)?;

    stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\n")?;
    stream.flush()?;
    let _caps = read_qmp_return(&mut reader)?;

    let payload = format!("{{\"execute\":\"{command}\"}}\n");
    stream.write_all(payload.as_bytes())?;
    stream.flush()?;
    read_qmp_return(&mut reader)
}

pub fn ch_request(sock: &Path, method: &str, path: &str) -> Result<(u16, String)> {
    let mut stream = UnixStream::connect(sock)
        .with_context(|| format!("Failed to connect to API socket '{}'", sock.display()))?;
    let timeout = Duration::from_millis(800);
    stream.set_read_timeout(Some(timeout))?;
    stream.set_write_timeout(Some(timeout))?;

    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: localhost\r\nAccept: application/json\r\nConnection: close\r\nContent-Length: 0\r\n\r\n"
    );
    stream.write_all(req.as_bytes())?;
    stream.flush()?;

    let mut buf = Vec::new();
    let mut tmp = [0u8; 2048];
    loop {
        match stream.read(&mut tmp) {
            Ok(0) => break,
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if buf.len() > 1024 * 1024 {
                    break;
                }
            }
            Err(e)
                if e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::WouldBlock =>
            {
                break;
            }
            Err(e) => return Err(e).context("Failed to read cloud-hypervisor API response"),
        }
    }

    let text = String::from_utf8_lossy(&buf);
    if text.is_empty() {
        return Err(anyhow!("Empty response from cloud-hypervisor API"));
    }
    let mut lines = text.lines();
    let status_line = lines.next().unwrap_or("");
    let code = status_line
        .split_whitespace()
        .nth(1)
        .ok_or_else(|| anyhow!("Malformed API status line: {status_line}"))?
        .parse::<u16>()
        .with_context(|| format!("Malformed API status code in '{status_line}'"))?;
    let body = text
        .split_once("\r\n\r\n")
        .map(|(_, body)| body.trim())
        .unwrap_or("")
        .to_string();
    Ok((code, body))
}

fn read_line(reader: &mut BufReader<UnixStream>) -> Result<String> {
    let mut line = String::new();
    let n = reader.read_line(&mut line)?;
    if n == 0 {
        return Err(anyhow!("QMP connection closed"));
    }
    Ok(line)
}

fn read_qmp_return(reader: &mut BufReader<UnixStream>) -> Result<Value> {
    for _ in 0..16 {
        let line = read_line(reader)?;
        let value: Value = serde_json::from_str(line.trim())
            .with_context(|| format!("Failed to parse QMP message: {line}"))?;
        if value.get("error").is_some() {
            return Err(anyhow!("QMP error: {value}"));
        }
        if value.get("return").is_some() {
            return Ok(value);
        }
    }
    Err(anyhow!("QMP command produced no return value"))
}

pub fn wait_for_removal(path: &Path, timeout: Duration) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if !path.exists() {
            return true;
        }
        thread::sleep(Duration::from_millis(200));
    }
    !path.exists()
}

/// QEMU and cloud-hypervisor create their sockets after exec. Tighten them
/// once they appear. The child umask already defaults them to 0600.
pub fn tighten_sockets(paths: Vec<PathBuf>, mode: u32) {
    thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut pending = paths;
        while Instant::now() < deadline && !pending.is_empty() {
            pending.retain(|path| {
                if path.exists() {
                    let _ = fs::set_permissions(path, fs::Permissions::from_mode(mode));
                    false
                } else {
                    true
                }
            });
            if !pending.is_empty() {
                thread::sleep(Duration::from_millis(50));
            }
        }
    });
}

pub fn socket_paths(global: &GlobalConfig, cfg: &VmConfig) -> Result<Vec<PathBuf>> {
    let mut paths = global.runtime_ipc_sockets(&cfg.backend, &cfg.name);
    if let settings::SerialTarget::Socket(path) = settings::resolve_serial(global, cfg)? {
        paths.push(path);
    }
    Ok(paths)
}

pub fn unlink_runtime_sockets(global: &GlobalConfig, cfg: &VmConfig) {
    if let Ok(paths) = socket_paths(global, cfg) {
        for path in paths {
            let _ = fs::remove_file(path);
        }
    }
}

pub const RESTART_LIMIT: usize = 8;
pub const RESTART_WINDOW: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VmExit {
    Stopped,
    Restart,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChLifecycle {
    Reset,
    Shutdown,
}

/// Record a restart. The ninth restart inside 60 seconds is refused.
pub fn note_restart(stamps: &mut Vec<Instant>) -> Result<()> {
    let now = Instant::now();
    stamps.retain(|stamp| now.saturating_duration_since(*stamp) < RESTART_WINDOW);
    if stamps.len() >= RESTART_LIMIT {
        return Err(anyhow!(
            "hypervisor restarted {RESTART_LIMIT} times within {} seconds",
            RESTART_WINDOW.as_secs()
        ));
    }
    stamps.push(now);
    Ok(())
}

/// `Some(reason)` when `message` is a QMP SHUTDOWN event. RESET is ignored.
pub fn qemu_shutdown_reason(message: &str) -> Option<String> {
    let value: Value = serde_json::from_str(message).ok()?;
    if value.get("event").and_then(|event| event.as_str()) != Some("SHUTDOWN") {
        return None;
    }
    let reason = value
        .get("data")
        .and_then(|data| data.get("reason"))
        .and_then(|reason| reason.as_str())
        .unwrap_or("")
        .to_string();
    Some(reason)
}

pub fn classify_qemu_exit(shutdown_reason: Option<&str>, success: bool) -> VmExit {
    match shutdown_reason {
        Some("guest-reset") => VmExit::Restart,
        Some("guest-shutdown") => VmExit::Stopped,
        _ if success => VmExit::Stopped,
        _ => VmExit::Failed,
    }
}

/// Last matching cloud-hypervisor log line. `event = shutdown` is not a signal:
/// an in-process reboot logs it from `Vm::shutdown` and must not end the unit.
pub fn ch_lifecycle_signal(line: &str) -> Option<ChLifecycle> {
    if line.contains("VM reset event") || line.contains("event = rebooting") {
        Some(ChLifecycle::Reset)
    } else if line.contains("VM guest exit event") || line.contains("VMM exit event") {
        Some(ChLifecycle::Shutdown)
    } else {
        None
    }
}

pub fn classify_ch_exit(last: Option<ChLifecycle>, success: bool) -> VmExit {
    match last {
        Some(ChLifecycle::Reset) => VmExit::Restart,
        Some(ChLifecycle::Shutdown) if success => VmExit::Stopped,
        Some(ChLifecycle::Shutdown) => VmExit::Failed,
        None if success => VmExit::Stopped,
        None => VmExit::Failed,
    }
}

/// Read QMP until the socket closes. `stop` aborts the connect retry after the
/// child has already exited, so a QEMU that never created the socket does not
/// hold the supervisor for the full retry window.
pub fn watch_qemu_shutdown(sock: PathBuf, slot: Arc<Mutex<Option<String>>>, stop: Arc<AtomicBool>) {
    let Some(mut stream) = connect_qmp(&sock, &stop) else {
        return;
    };
    let mut pending = Vec::new();
    if let Err(err) = qmp_handshake(&mut stream, &mut pending, &slot) {
        eprintln!("QMP watcher for '{}' stopped: {err}", sock.display());
        return;
    }
    loop {
        match read_qmp_line(&mut stream, &mut pending) {
            Ok(line) => {
                if let Some(reason) = qemu_shutdown_reason(&line) {
                    *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(reason);
                }
            }
            Err(_) => return,
        }
    }
}

fn connect_qmp(sock: &Path, stop: &AtomicBool) -> Option<UnixStream> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        if stop.load(Ordering::Relaxed) {
            return None;
        }
        match UnixStream::connect(sock) {
            Ok(stream) => return Some(stream),
            Err(_) if Instant::now() < deadline => thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
}

fn qmp_handshake(
    stream: &mut UnixStream,
    pending: &mut Vec<u8>,
    slot: &Mutex<Option<String>>,
) -> Result<()> {
    let _greeting = read_qmp_line(stream, pending)?;
    stream.write_all(b"{\"execute\":\"qmp_capabilities\"}\n")?;
    stream.flush()?;
    for _ in 0..16 {
        let line = read_qmp_line(stream, pending)?;
        if let Some(reason) = qemu_shutdown_reason(&line) {
            *slot.lock().unwrap_or_else(|poison| poison.into_inner()) = Some(reason);
        }
        let value: Value = serde_json::from_str(&line)
            .with_context(|| format!("Failed to parse QMP message: {line}"))?;
        if value.get("error").is_some() {
            return Err(anyhow!("QMP error: {value}"));
        }
        if value.get("return").is_some() {
            return Ok(());
        }
    }
    Err(anyhow!("QMP handshake produced no return value"))
}

fn read_qmp_line(stream: &mut UnixStream, pending: &mut Vec<u8>) -> Result<String> {
    loop {
        if let Some(pos) = pending.iter().position(|byte| *byte == b'\n') {
            let line: Vec<u8> = pending.drain(..=pos).collect();
            return Ok(String::from_utf8_lossy(&line).trim().to_string());
        }
        let mut tmp = [0u8; 2048];
        match stream.read(&mut tmp) {
            Ok(0) => return Err(anyhow!("QMP connection closed")),
            Ok(n) => pending.extend_from_slice(&tmp[..n]),
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(err) => return Err(err).context("Failed to read QMP"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn qemu_exit_classes_follow_the_shutdown_reason() {
        assert!(qemu_shutdown_reason(r#"{"event":"RESET"}"#).is_none());
        assert_eq!(
            qemu_shutdown_reason(r#"{"event":"SHUTDOWN","data":{"reason":"guest-reset"}}"#)
                .as_deref(),
            Some("guest-reset")
        );
        assert_eq!(
            qemu_shutdown_reason(r#"{"event":"SHUTDOWN","data":{}}"#).as_deref(),
            Some("")
        );
        assert_eq!(
            classify_qemu_exit(Some("guest-reset"), true),
            VmExit::Restart
        );
        assert_eq!(
            classify_qemu_exit(Some("guest-reset"), false),
            VmExit::Restart
        );
        assert_eq!(
            classify_qemu_exit(Some("guest-shutdown"), false),
            VmExit::Stopped
        );
        assert_eq!(
            classify_qemu_exit(Some("host-qmp-quit"), true),
            VmExit::Stopped
        );
        assert_eq!(
            classify_qemu_exit(Some("host-qmp-quit"), false),
            VmExit::Failed
        );
        assert_eq!(
            classify_qemu_exit(Some("host-signal"), false),
            VmExit::Failed
        );
        assert_eq!(classify_qemu_exit(None, true), VmExit::Stopped);
        assert_eq!(classify_qemu_exit(None, false), VmExit::Failed);
        assert_eq!(classify_qemu_exit(Some(""), true), VmExit::Stopped);
        assert_eq!(classify_qemu_exit(Some(""), false), VmExit::Failed);
    }

    #[test]
    fn ch_log_lines_keep_reboot_apart_from_poweroff() {
        assert_eq!(
            ch_lifecycle_signal("INFO VM reset event"),
            Some(ChLifecycle::Reset)
        );
        assert_eq!(
            ch_lifecycle_signal("Event: source = vm event = rebooting"),
            Some(ChLifecycle::Reset)
        );
        assert_eq!(
            ch_lifecycle_signal("Event: source = vm event = shutdown"),
            None
        );
        assert_eq!(
            ch_lifecycle_signal("Event: source = vm event = rebooted"),
            None
        );
        assert_eq!(
            ch_lifecycle_signal("INFO VM guest exit event"),
            Some(ChLifecycle::Shutdown)
        );
        assert_eq!(
            ch_lifecycle_signal("INFO VMM exit event"),
            Some(ChLifecycle::Shutdown)
        );
        assert_eq!(
            classify_ch_exit(Some(ChLifecycle::Reset), false),
            VmExit::Restart
        );
        assert_eq!(
            classify_ch_exit(Some(ChLifecycle::Shutdown), true),
            VmExit::Stopped
        );
        assert_eq!(
            classify_ch_exit(Some(ChLifecycle::Shutdown), false),
            VmExit::Failed
        );
        assert_eq!(classify_ch_exit(None, true), VmExit::Stopped);
        assert_eq!(classify_ch_exit(None, false), VmExit::Failed);
    }

    #[test]
    fn restart_storm_is_capped_and_old_stamps_expire() {
        let mut stamps = Vec::new();
        for _ in 0..RESTART_LIMIT {
            note_restart(&mut stamps).unwrap();
        }
        assert!(note_restart(&mut stamps).is_err());

        let mut expired = vec![
            Instant::now()
                .checked_sub(Duration::from_secs(120))
                .unwrap();
            RESTART_LIMIT
        ];
        note_restart(&mut expired).unwrap();
        assert_eq!(expired.len(), 1);
    }
}
