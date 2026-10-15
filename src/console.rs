use anyhow::{anyhow, Context, Result};
use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;

use crate::config::VmConfig;
use crate::settings::{self, GlobalConfig, SerialTarget};

struct RawTerminalGuard {
    orig: libc::termios,
    active: bool,
}

impl RawTerminalGuard {
    fn new() -> Result<Self> {
        unsafe {
            if libc::isatty(libc::STDIN_FILENO) == 0 {
                return Ok(Self {
                    orig: std::mem::zeroed(),
                    active: false,
                });
            }
            let mut orig: libc::termios = std::mem::zeroed();
            if libc::tcgetattr(libc::STDIN_FILENO, &mut orig) != 0 {
                return Err(io::Error::last_os_error())
                    .context("Failed to get terminal attributes");
            }
            let mut raw = orig;
            libc::cfmakeraw(&mut raw);
            if libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &raw) != 0 {
                return Err(io::Error::last_os_error()).context("Failed to set raw terminal");
            }
            Ok(Self { orig, active: true })
        }
    }
}

impl Drop for RawTerminalGuard {
    fn drop(&mut self) {
        if self.active {
            unsafe {
                libc::tcsetattr(libc::STDIN_FILENO, libc::TCSANOW, &self.orig);
            }
        }
    }
}

pub fn find_serial_socket(global: &GlobalConfig, target: &str) -> Result<(String, PathBuf)> {
    if let Ok((_, cfg)) = VmConfig::find_and_load(target, &global.config_dirs) {
        return match settings::resolve_serial(global, &cfg)? {
            SerialTarget::Socket(path) => Ok((cfg.name, path)),
            _ => Err(anyhow!(
                "VM '{}' serial is not in socket mode. Set serial.mode to \"socket\" to attach.",
                cfg.name
            )),
        };
    }

    Ok((target.to_string(), global.default_serial_socket(target)))
}

pub fn run_console(global: &GlobalConfig, target: &str) -> Result<()> {
    let (name, socket_path) = find_serial_socket(global, target)?;

    if !socket_path.exists() {
        return Err(anyhow!(
            "Serial socket '{}' does not exist.\nMake sure VM '{}' is running and configured with a serial socket.",
            socket_path.display(),
            name
        ));
    }

    println!(
        "Connecting to '{}' serial console at '{}'...",
        name,
        socket_path.display()
    );
    println!("Escape sequence: Press 'Ctrl+]' to disconnect.");
    println!("------------------------------------------------------------");

    let stream = UnixStream::connect(&socket_path).with_context(|| {
        format!(
            "Failed to connect to serial socket '{}'",
            socket_path.display()
        )
    })?;

    let _guard = RawTerminalGuard::new()?;

    let mut read_stream = stream.try_clone()?;
    let mut write_stream = stream;

    let running = Arc::new(AtomicBool::new(true));
    let running_clone = running.clone();

    // Background thread: Socket -> Stdout
    let handle = thread::spawn(move || {
        let mut buf = [0u8; 1024];
        let mut stdout = io::stdout();
        while running_clone.load(Ordering::Relaxed) {
            match read_stream.read(&mut buf) {
                Ok(0) => break,
                Ok(n) => {
                    let _ = stdout.write_all(&buf[..n]);
                    let _ = stdout.flush();
                }
                Err(_) => break,
            }
        }
        running_clone.store(false, Ordering::Relaxed);
    });

    // Main thread: Stdin -> Socket
    let mut stdin = io::stdin();
    let mut in_buf = [0u8; 128];
    while running.load(Ordering::Relaxed) {
        match stdin.read(&mut in_buf) {
            Ok(0) => break,
            Ok(n) => {
                // Check for Ctrl+] (ASCII 29 / 0x1d) to cleanly disconnect
                if in_buf[..n].contains(&0x1d) {
                    break;
                }
                if write_stream.write_all(&in_buf[..n]).is_err() {
                    break;
                }
                let _ = write_stream.flush();
            }
            Err(_) => break,
        }
    }

    running.store(false, Ordering::Relaxed);
    let _ = write_stream.shutdown(std::net::Shutdown::Both);
    let _ = handle.join();

    // Cleanly restore cursor and terminal newline
    print!("\r\n[Disconnected from {} serial console]\r\n", name);
    let _ = io::stdout().flush();

    Ok(())
}
