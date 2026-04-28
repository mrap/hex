use std::path::Path;
use std::process::Command;

fn main() {
    // Version source of truth: version.txt at the hex workspace root.
    // Falls back to Cargo.toml version if version.txt doesn't exist (dev builds).
    let hex_dir = std::env::var("HEX_DIR")
        .or_else(|_| std::env::var("CARGO_MANIFEST_DIR").map(|d| {
            Path::new(&d).parent().unwrap().parent().unwrap().to_string_lossy().into_owned()
        }))
        .unwrap_or_default();

    let version_file = Path::new(&hex_dir).join("version.txt");
    let version = if version_file.exists() {
        std::fs::read_to_string(&version_file)
            .unwrap_or_default()
            .trim()
            .to_string()
    } else {
        std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0-dev".to_string())
    };

    // Git SHA for build metadata
    let git_sha = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());

    println!("cargo:rustc-env=HEX_VERSION={}", version);
    println!("cargo:rustc-env=HEX_GIT_SHA={}", git_sha);
    println!("cargo:rerun-if-changed=../../version.txt");
}
