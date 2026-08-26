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

    // What has to rerun this script, and what must not.
    //
    // Two false starts are worth recording, because each looked right.
    //
    // Watching `.git/HEAD` alone fails twice over: on a branch checkout
    // that file holds `ref: refs/heads/<branch>` and does not move when a
    // commit lands, and in a linked worktree there is no `.git` directory
    // at all. So git is asked where these actually live, and the ref HEAD
    // names is followed to the file that does move.
    //
    // Then the watches were removed altogether, on the theory that a
    // script emitting no `rerun-if-changed` runs on every build. That rule
    // holds only for a script emitting no `rerun-if-*` of any kind, and
    // this one emits `rerun-if-env-changed` below - which overrides the
    // default outright. The result was a script that ran almost never: a
    // clean tree four commits later still stamped the old sha and a
    // `-dirty` that was hours stale, and a thirty-second full rebuild did
    // not move it.
    //
    // Running it unconditionally does work, and costs twenty-eight seconds
    // on every no-op build, because a rerun relinks all fourteen binaries
    // whether or not the stamp changed. `cargo test` is the ritual before
    // every commit here, so that is the wrong trade.
    //
    // So both halves are watched explicitly. The git files cover the
    // commit; the source trees cover `-dirty`, and cost nothing, because a
    // source edit was going to rebuild those crates anyway. What this
    // cannot see is a new untracked file that no crate compiles - the
    // stamp stays clean for one build longer than it should, which is the
    // one gap left and a smaller lie than either of the above.
    let watch_git = |path: &str| {
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
    watch_git("HEAD");
    watch_git("packed-refs");
    if let Some(head_ref) = Command::new("git")
        .args(["symbolic-ref", "-q", "HEAD"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
    {
        watch_git(&head_ref);
    }
    // Cargo walks a directory watch recursively, so these two cover every
    // source file in the workspace without naming one.
    for dir in ["../core/src", "../widgets/src", "../Cargo.toml", "../Cargo.lock"] {
        println!("cargo:rerun-if-changed={}", dir);
    }
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
