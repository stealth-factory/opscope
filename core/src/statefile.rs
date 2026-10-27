// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! State files: where a widget keeps what it learned, and how it writes it.
//!
//! Three widgets computed the state directory by hand before this module,
//! and each had its own idea of the fallback. [`state_dir`] is the one
//! answer, and [`write_atomic`] is the writer the settings screen already
//! trusted with `config.json`: a same-directory temporary file created
//! `O_EXCL | O_NOFOLLOW` at `0600`, re-read and re-parsed before it is
//! renamed over the old one, and the directory synced after. A state file
//! is JSON in this tree, so the writer refuses anything that does not
//! parse - which is what keeps an interrupted write from leaving a
//! truncated file for every pane to choke on.
//!
//! [`with_lock`] is for a file several panes read, change and write back:
//! it holds `flock(LOCK_EX)` on `<path>.lock` for as long as the closure
//! runs, so two read-modify-writes merge rather than one replacing the
//! other.
//!
//! Nothing here panics. Every failure is an `Err` naming the path.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::os::unix::io::AsRawFd;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where opscope keeps state: `$XDG_STATE_HOME/opscope`, else
/// `$HOME/.local/state/opscope`.
///
/// With neither set - a service with a scrubbed environment - it is
/// `./.opscope-state`, relative to the working directory, rather than a
/// panic or a path at the root of the filesystem. A writer that then fails
/// says which path it tried.
pub fn state_dir() -> PathBuf {
    state_dir_from(
        std::env::var_os("XDG_STATE_HOME"),
        std::env::var_os("HOME"),
    )
}

/// [`state_dir`] from the two variables it reads, so the rule is tested
/// without changing `HOME` under every other test in the process.
fn state_dir_from(
    xdg: Option<std::ffi::OsString>,
    home: Option<std::ffi::OsString>,
) -> PathBuf {
    if let Some(base) = xdg.filter(|v| !v.is_empty()) {
        return PathBuf::from(base).join("opscope");
    }
    if let Some(home) = home.filter(|v| !v.is_empty()) {
        return PathBuf::from(home).join(".local/state/opscope");
    }
    PathBuf::from(".opscope-state")
}

/// Replace `path` with `contents`, atomically and owner-only.
///
/// The temporary file is `0600` on creation, not chmodded afterwards, so
/// there is no moment another user can open it. `contents` must parse as
/// JSON; a file that does not re-parse after writing is refused and the
/// temporary removed. The parent directory is created if it is missing. A
/// symlink at `path` is kept: the file it points at is replaced instead.
pub fn write_atomic(path: &Path, contents: &str) -> Result<(), String> {
    write(path, contents, None)
}

/// [`write_atomic`], refused unless the file still holds `expected` - an
/// empty `expected` meaning the file is absent or empty. The settings
/// screen's compare-and-swap: it re-reads `config.json`, patches one value,
/// and must not overwrite an edit somebody made in between.
pub fn write_atomic_if(path: &Path, contents: &str, expected: &str) -> Result<(), String> {
    write(path, contents, Some(expected))
}

/// Run `f` holding an exclusive `flock` on `<path>.lock`.
///
/// The lock file is created `0600` beside `path`, with its directory, and
/// is never removed: deleting it while another process waits on it would
/// let a third take a lock on a new file and run alongside the second. The
/// lock is released when `f` returns, by closing the descriptor. `flock`
/// conflicts between two descriptors in the same process too, so threads
/// are serialised as well as processes. A busy lock is taken
/// non-blocking: `EINTR` retries, and `WouldBlock` sleeps 10ms for up to
/// 50 attempts, then the error is returned.
pub fn with_lock<T>(path: &Path, f: impl FnOnce() -> T) -> Result<T, String> {
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("state.json");
    let lock = path.with_file_name(format!("{name}.lock"));
    if let Some(parent) = lock.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(&lock)
        .map_err(|e| format!("{}: {e}", lock.display()))?;
    let mut waits = 0;
    loop {
        // SAFETY: a valid descriptor owned by `file` for this whole call.
        let rc = unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) };
        if rc == 0 {
            break;
        }
        let err = std::io::Error::last_os_error();
        match err.kind() {
            std::io::ErrorKind::Interrupted => continue,
            std::io::ErrorKind::WouldBlock if waits < 50 => {
                waits += 1;
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            _ => return Err(format!("{}: {err}", lock.display())),
        }
    }
    let out = f();
    drop(file);
    Ok(out)
}

fn write(path: &Path, contents: &str, expected: Option<&str>) -> Result<(), String> {
    serde_json::from_str::<Value>(contents)
        .map_err(|e| format!("refusing to write: {e}"))?;
    // Keep an intentional config symlink intact. Rename over its resolved
    // target; renaming over the link itself would silently replace it.
    let destination = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .map_err(|e| format!("{}: {e}", path.display()))?,
        _ => path.to_path_buf(),
    };
    let path = destination.as_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("state.json");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut created = None;
    for attempt in 0..32 {
        let tmp = path.with_file_name(format!(
            ".{name}.tmp-{}-{nonce}-{attempt}",
            std::process::id()
        ));
        let mut opts = OpenOptions::new();
        opts.write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = match opts.open(&tmp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", tmp.display())),
        };
        file.write_all(contents.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", tmp.display())
            })?;
        created = Some(tmp);
        break;
    }
    let tmp = created.ok_or_else(|| {
        format!(
            "{}: could not reserve a private temporary file",
            path.display()
        )
    })?;
    let mode = std::fs::metadata(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o600 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "{} was created with mode {mode:04o}, not 0600",
            tmp.display()
        ));
    }
    let read_back =
        std::fs::read_to_string(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = serde_json::from_str::<Value>(&read_back) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("wrote a file that does not parse: {e}"));
    }
    if let Some(expected) = expected {
        let current = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("{}: {e}", path.display()));
            }
        };
        if current != expected {
            let _ = std::fs::remove_file(&tmp);
            return Err(format!("{name} changed again while saving · retry the edit"));
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// `XDG_STATE_HOME` and `HOME` are process-global and cargo runs tests
    /// in parallel, so every test in core that sets either holds this.
    pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    pub(crate) fn env_lock() -> std::sync::MutexGuard<'static, ()> {
        match ENV_LOCK.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// Set or unset an environment variable, putting it back on drop.
    pub(crate) struct EnvVar {
        name: &'static str,
        held: Option<std::ffi::OsString>,
    }

    impl EnvVar {
        pub(crate) fn set(name: &'static str, value: Option<&std::ffi::OsStr>) -> EnvVar {
            let held = std::env::var_os(name);
            match value {
                Some(v) => std::env::set_var(name, v),
                None => std::env::remove_var(name),
            }
            EnvVar { name, held }
        }
    }

    impl Drop for EnvVar {
        fn drop(&mut self) {
            match &self.held {
                Some(v) => std::env::set_var(self.name, v),
                None => std::env::remove_var(self.name),
            }
        }
    }

    #[test]
    fn the_state_dir_follows_xdg_then_home_then_the_working_directory() {
        let os = |s: &str| Some(std::ffi::OsString::from(s));
        assert_eq!(
            state_dir_from(os("/xdg-state"), os("/home-dir")),
            PathBuf::from("/xdg-state/opscope")
        );
        assert_eq!(
            state_dir_from(os(""), os("/home-dir")),
            PathBuf::from("/home-dir/.local/state/opscope"),
            "an empty XDG_STATE_HOME is unset"
        );
        assert_eq!(
            state_dir_from(None, os("/home-dir")),
            PathBuf::from("/home-dir/.local/state/opscope")
        );
        assert_eq!(state_dir_from(None, None), PathBuf::from(".opscope-state"));
        assert_eq!(state_dir_from(os(""), os("")), PathBuf::from(".opscope-state"));
        // And the real one reads XDG_STATE_HOME.
        let _guard = env_lock();
        let _xdg = EnvVar::set("XDG_STATE_HOME", Some("/xdg-state".as_ref()));
        assert_eq!(state_dir(), PathBuf::from("/xdg-state/opscope"));
    }

    #[test]
    fn atomic_write_is_0600_and_refuses_junk() {
        let dir = std::env::temp_dir().join(format!(
            "opscope-config-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        write_atomic(&path, "{\"a\":1}\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(write_atomic_if(&path, "{", "{\"a\":1}\n").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(
            write_atomic_if(&path, "{\"a\":2}\n", "{\"other\":true}\n").is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftover.len(), 1, "a refused write left a temp file: {leftover:?}");
        let target = dir.join("target.json");
        let linked = dir.join("linked.json");
        std::fs::write(&target, "{\"linked\":false}\n").unwrap();
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        write_atomic_if(
            &linked,
            "{\"linked\":true}\n",
            "{\"linked\":false}\n",
        )
        .unwrap();
        assert!(std::fs::symlink_metadata(&linked).unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"linked\":true}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }


    #[test]
    fn the_lock_file_is_0600_and_serialises_read_modify_write() {
        let dir = temp_dir("state-lock");
        let path = dir.join("count.json");
        write_atomic(&path, "0\n").unwrap();
        let threads: Vec<_> = (0..4)
            .map(|_| {
                let path = path.clone();
                std::thread::spawn(move || {
                    for _ in 0..25 {
                        with_lock(&path, || {
                            let n: u64 = std::fs::read_to_string(&path)
                                .unwrap()
                                .trim()
                                .parse()
                                .unwrap();
                            write_atomic(&path, &format!("{}\n", n + 1)).unwrap();
                        })
                        .unwrap();
                    }
                })
            })
            .collect();
        for t in threads {
            t.join().unwrap();
        }
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "100\n", "no update was lost");
        let lock = dir.join("count.json.lock");
        let mode = std::fs::metadata(&lock).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_write_that_cannot_happen_names_its_path() {
        let dir = temp_dir("state-fail");
        // A file where the directory should be.
        let blocker = dir.join("blocked");
        std::fs::write(&blocker, "").unwrap();
        let path = blocker.join("notices.json");
        let err = write_atomic(&path, "{}").unwrap_err();
        assert!(err.contains("blocked"), "{err}");
        let err = with_lock(&path, || ()).unwrap_err();
        assert!(err.contains("blocked"), "{err}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
