use std::path::Path;
use std::process::Command;

use time::OffsetDateTime;

fn main() {
    // Emitting any rerun-if-changed disables cargo's default "watch the whole
    // package" behaviour, so the source paths have to be listed explicitly or
    // the recorded build date goes stale.
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-changed=Cargo.toml");
    println!("cargo:rerun-if-changed=build.rs");
    for p in [".git/HEAD", ".git/refs"] {
        if Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    println!("cargo:rerun-if-env-changed=PPK2TUI_GIT_SHA");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");

    println!("cargo:rustc-env=PPK2TUI_GIT_SHA={}", git_sha());
    println!("cargo:rustc-env=PPK2TUI_BUILD_DATE={}", build_date());
}

fn git_sha() -> String {
    // Docker builds exclude .git (see .dockerignore), so the image build passes
    // the sha in through this variable instead of shelling out to git.
    if let Ok(sha) = std::env::var("PPK2TUI_GIT_SHA") {
        let sha = sha.trim();
        if !sha.is_empty() {
            return sha.to_string();
        }
    }

    let Ok(out) = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
    else {
        return "unknown".to_string();
    };
    if !out.status.success() {
        return "unknown".to_string();
    }
    let sha = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if sha.is_empty() {
        return "unknown".to_string();
    }

    // Tracked-file changes only: untracked scratch files shouldn't flag a build.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .map(|o| o.status.success() && !o.stdout.is_empty())
        .unwrap_or(false);

    if dirty {
        format!("{sha}-dirty")
    } else {
        sha
    }
}

fn build_date() -> String {
    // SOURCE_DATE_EPOCH is the reproducible-builds convention; honour it so a
    // pinned build produces a stable date.
    let now = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.trim().parse::<i64>().ok())
        .and_then(|secs| OffsetDateTime::from_unix_timestamp(secs).ok())
        .unwrap_or_else(OffsetDateTime::now_utc);

    // time::Date renders as ISO 8601 (YYYY-MM-DD).
    now.date().to_string()
}
