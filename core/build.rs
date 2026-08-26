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

    // Run on every build, which is the only thing that makes the stamp
    // true - and getting that takes a directive, not the absence of one.
    //
    // "Emit no `rerun-if-*` and cargo reruns when any file in the package
    // changes" is true only while the script emits none at all. This one
    // has always emitted `rerun-if-env-changed` below, which overrides the
    // default outright: the script then reruns when SOURCE_DATE_EPOCH
    // changes and at no other time. Removing the watches did not make it
    // run always, it made it run almost never - a full rebuild of every
    // crate left the stamp naming a commit four ahead of it and carrying a
    // `-dirty` the tree had not had for hours.
    //
    // A path that does not exist cannot be stat-ed, and cargo answers that
    // by rerunning. It is a sentinel, not a file: nothing should ever
    // create it.
    //
    // Watching `.git/HEAD` does not work: on a branch checkout that file
    // holds `ref: refs/heads/<branch>` and does not move when a commit
    // lands. Watching the ref it names fixes the commit half - but nothing
    // in `.git` moves when a source file is edited, so `-dirty` stayed
    // absent while the tree was dirty. A marker that says "clean" over a
    // modified tree is worse than no marker: it is a claim, and it is false.
    //
    // The cost was measured rather than feared. The script is three git
    // calls; cargo compares the environment it emits and only rebuilds
    // dependents when it changes, so a no-op rebuild is 0.03s. The first
    // build after the tree goes from clean to dirty relinks the fourteen
    // binaries, which is exactly when their stamp has genuinely changed.
    println!("cargo:rerun-if-changed=.stamp-every-build");
    println!("cargo:rerun-if-env-changed=SOURCE_DATE_EPOCH");
}
