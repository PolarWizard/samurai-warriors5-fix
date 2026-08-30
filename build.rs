use std::process::Command;

/// Exposes `GIT_HASH` and `RUSTC_VERSION` as compile-time env vars (read via
/// `env!(...)` in `lib.rs`) for the log's build-provenance line. Shells out
/// directly rather than adding a build-time crate (`vergen` et al.) -- two
/// `Command::output()` calls cover the whole job.
fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs");

    let git_hash = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        // No commits yet, git missing, or not a git checkout (e.g. a source
        // zip) -- the version line still needs a value to sit next to.
        .unwrap_or_else(|| "nogit".to_string());
    println!("cargo:rustc-env=GIT_HASH={git_hash}");

    let rustc = std::env::var("RUSTC").unwrap_or_else(|_| "rustc".to_string());
    let rustc_version = Command::new(rustc)
        .arg("--version")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo:rustc-env=RUSTC_VERSION={rustc_version}");
}
