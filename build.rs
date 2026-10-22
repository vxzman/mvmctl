use std::env;
use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/index");

    set("MVMCTL_GIT", &git_id());
    set("MVMCTL_BUILT_AT", &built_at());
    set("MVMCTL_RUSTC", &rustc_version());
    set(
        "MVMCTL_TARGET",
        &env::var("TARGET").unwrap_or_else(|_| "unknown".into()),
    );
    set(
        "MVMCTL_PROFILE",
        &env::var("PROFILE").unwrap_or_else(|_| "unknown".into()),
    );
}

fn set(key: &str, value: &str) {
    println!("cargo:rustc-env={key}={value}");
}

fn git_id() -> String {
    let dir = env::var("CARGO_MANIFEST_DIR").unwrap_or_else(|_| ".".into());
    let hash = git_output(&dir, &["rev-parse", "--short=12", "HEAD"]).unwrap_or_default();
    if hash.is_empty() {
        return "unknown".into();
    }
    let dirty = git_output(&dir, &["status", "--porcelain", "--untracked-files=no"])
        .map(|text| !text.is_empty())
        .unwrap_or(false);
    if dirty {
        format!("{hash}-dirty")
    } else {
        hash
    }
}

fn git_output(dir: &str, args: &[&str]) -> Option<String> {
    let output = Command::new("git")
        .args(["-C", dir])
        .args(args)
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    Some(text.trim().to_string())
}

fn built_at() -> String {
    Command::new("date")
        .args(["-u", "+%Y-%m-%dT%H:%M:%SZ"])
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".into())
}

fn rustc_version() -> String {
    let rustc = env::var("RUSTC").unwrap_or_else(|_| "rustc".into());
    Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .and_then(|output| String::from_utf8(output.stdout).ok())
        .map(|text| text.trim().to_string())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| "unknown".into())
}
