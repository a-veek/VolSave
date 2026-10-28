//! Adds the git commit to the version, so test builds can be told apart.

use std::process::Command;

fn main() {
    let git = |args: &[&str]| {
        Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|out| out.status.success())
            .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_string())
    };
    let version = env!("CARGO_PKG_VERSION");
    // Only inside the VolSave repository, not when used as a git dependency.
    let in_repo = std::path::Path::new("../../.git").exists();
    let build = match git(&["rev-parse", "--short", "HEAD"]).filter(|_| in_repo) {
        Some(commit) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|s| !s.is_empty());
            format!(
                "{version} ({commit}{})",
                if dirty { "-modified" } else { "" }
            )
        }
        None => version.to_string(),
    };
    println!("cargo:rustc-env=VOLSAVE_VERSION={build}");
    println!("cargo:rerun-if-changed=../../.git/HEAD");
    println!("cargo:rerun-if-changed=../../.git/refs");
    println!("cargo:rerun-if-changed=../../.git/index");
}
