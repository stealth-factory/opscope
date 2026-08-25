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

    // Rebuild when the checked-out commit changes, or the sha is whatever it
    // was the first time core compiled and `--version` names the wrong
    // commit - worse than one that says "unknown", because it looks right.
    //
    // Watching `.git/HEAD` alone does not do it, and fails two ways at once.
    // On a branch checkout that file holds `ref: refs/heads/<branch>` and
    // does not change when a commit lands; what moves is the ref it names.
    // And in a linked worktree there is no `.git` directory at all - `.git`
    // is a file pointing elsewhere - so the path does not exist, which cargo
    // reads as "always rerun". Correct output, by accident, and only here.
    //
    // So git is asked where these actually live. `--git-path` resolves a
    // worktree's real git directory, and the ref is followed to the file
    // that moves. packed-refs covers a ref with no loose file of its own,
    // and HEAD itself covers a detached checkout, where it holds the sha.
    let watch = |path: &str| {
        let resolved = Command::new("git")
            .args(["rev-parse", "--git-path", path])
            .output()
            .ok()
            .filter(|o| o.status.success())
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty());
        if let Some(file) = resolved {
            println!("cargo:rerun-if-changed={}", file);
        }
    };
    watch("HEAD");
    watch("packed-refs");
    if let Some(head_ref) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        watch(&head_ref);
    }

    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
