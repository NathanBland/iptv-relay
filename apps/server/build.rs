use std::process::Command;

fn main() {
    println!("cargo:rerun-if-changed=.git/HEAD");
    println!("cargo:rerun-if-changed=.git/refs/tags");
    println!("cargo:rerun-if-changed=build.rs");

    let hash = std::env::var("GIT_COMMIT_HASH").unwrap_or_else(|_| {
        Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_owned())
    });

    let tag = std::env::var("GIT_TAG").unwrap_or_else(|_| {
        Command::new("git")
            .args(["describe", "--tags", "--abbrev=0"])
            .output()
            .ok()
            .and_then(|output| {
                if output.status.success() {
                    String::from_utf8(output.stdout).ok().map(|s| s.trim().to_owned())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "unknown".to_owned())
    });

    println!("cargo:rustc-env=GIT_COMMIT_HASH={hash}");
    println!("cargo:rustc-env=GIT_TAG={tag}");
}
