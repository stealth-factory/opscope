//! What the binary can say about itself.
//!
//! The version comes from `Cargo.toml`; the commit and the date come from
//! here, because neither is knowable from source alone. A `--version` that
//! cannot say which commit it is has answered nothing useful: the version
//! only changes at a release, and most builds anybody runs are between two.

use std::process::Command;

fn main() {
    // The commit, short. `git` may not be here at all - a release tarball
    // has no `.git` - and that is not a build failure, it is an honest
    // "unknown". A build that refuses to happen off a checkout is worse
    // than one that admits what it does not know.
    let commit = Command::new("git")
        .args(["rev-parse", "--short", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown".into());

    // Whether that commit had anything uncommitted under it. A sha alone
    // says which commit was checked out, not which source was compiled.
    let dirty = Command::new("git")
        .args(["status", "--porcelain", "--untracked-files=no"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| !o.stdout.is_empty())
        .unwrap_or(false);
    let commit = if dirty { format!("{}-dirty", commit) } else { commit };

    // SOURCE_DATE_EPOCH first, which is the convention for making a build
    // reproducible: with it set, two builds of the same source agree. Then
    // the commit's own date, which has the same property and needs nothing
    // set. Wall-clock never, because it makes every build differ from every
    // other for no reader's benefit.
    let date = std::env::var("SOURCE_DATE_EPOCH")
        .ok()
        .and_then(|s| s.parse::<i64>().ok())
        .map(|secs| {
            Command::new("date")
                .args(["-u", "-d", &format!("@{}", secs), "+%Y-%m-%d"])
                .output()
                .ok()
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .unwrap_or_else(|| "unknown".into())
        })
        .or_else(|| {
            Command::new("git")
                .args(["log", "-1", "--date=format:%Y-%m-%d", "--format=%cd"])
                .output()
                .ok()
                .filter(|o| o.status.success())
                .and_then(|o| String::from_utf8(o.stdout).ok())
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
        })
        .unwrap_or_else(|| "unknown".into());

    println!("cargo:rustc-env=TOYS_COMMIT={}", commit);
    println!("cargo:rustc-env=TOYS_BUILD_DATE={}", date);
    // Rebuild when the checked-out commit changes. Without this the sha is
    // whatever it was the first time core compiled, and a `--version` that
    // names the wrong commit is worse than one that says "unknown".
    println!("cargo:rerun-if-changed=../.git/HEAD");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
