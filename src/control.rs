use anyhow::{anyhow, Context, Result};
use serde_json::Value;
use std::fs;
use std::io::{BufRead, BufReader, Read, Write};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
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
    let mut paths = vec![global.control_socket(&cfg.backend, &cfg.name)];
    if let settings::SerialTarget::Socket(path) = settings::resolve_serial(global, cfg)? {
        paths.push(path);
    }
    Ok(paths)
}
