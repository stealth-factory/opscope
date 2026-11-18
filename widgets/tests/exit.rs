// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Every ordinary way out restores the same terminal and no child outlives
//! its launcher. These are real processes on test-owned ptys: source checks
//! cannot see a missing final write or an orphaned widget.

mod support;

use std::os::fd::FromRawFd;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const ALT_ENTER: &[u8] = b"\x1b[?1049h";
const ALT_LEAVE: &[u8] = b"\x1b[?1049l";
const RESTORE: &[u8] =
    b"\x1b[?1006l\x1b[?1000l\x1b[?2031l\x1b[?2026l\x1b[?25h\x1b[0m\x1b[2J\x1b[H\x1b[?1049l";
static NEXT_HOME: AtomicUsize = AtomicUsize::new(0);

fn dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .flat_map(|b| std::ascii::escape_default(*b))
        .map(char::from)
        .collect()
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.windows(needle.len()).any(|part| part == needle)
}

fn last_at(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .rposition(|part| part == needle)
}

struct Running {
    child: Child,
    master: i32,
    output: Vec<u8>,
    reaped: bool,
    home: PathBuf,
    /// Set while this launcher's process group may still hold an orphan.
    group: Option<i32>,
}

impl Running {
    fn spawn(path: &Path, args: &[&str], force_panic: bool, own_group: bool) -> Self {
        let (master, slave) = support::pty(80, 24);
        let unique = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let home =
            std::env::temp_dir().join(format!("opscope-exit-{}-{unique}", std::process::id()));
        let config = home.join("config/opscope");
        let state = home.join("state/opscope");
        std::fs::create_dir_all(&config).expect("test config directory");
        std::fs::create_dir_all(&state).expect("test state directory");
        // Answer the first-run consent question out of band, and mark its
        // opt-out event sent, so no screen or network send precedes matrix.
        std::fs::write(
            config.join("config.json"),
            "{\"analytics\":{\"enabled\":false}}\n",
        )
        .unwrap();
        std::fs::write(state.join("usage.json"), "{\"decided_at\":1}\n").unwrap();
        let io = |fd: i32| unsafe { Stdio::from_raw_fd(libc::dup(fd)) };
        let mut command = Command::new(path);
        command
            .args(args)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("TERM", "xterm-256color")
            .env("OPSCOPE_GRAPHICS", "text")
            .env_remove("OPSCOPE_CONFIG")
            .env_remove("HERDR_ENV")
            .env_remove("TMUX")
            .env_remove("OPSCOPE_TEST_PANIC")
            .stdin(io(slave))
            .stdout(io(slave))
            .stderr(io(slave));
        if force_panic {
            command.env("OPSCOPE_TEST_PANIC", "1");
        }
        if own_group {
            // The launcher and widget share this group. Counting the group
            // after the launcher exits proves that the widget went too,
            // without a platform-specific process-table reader.
            use std::os::unix::process::CommandExt;
            unsafe {
                command.pre_exec(|| {
                    if libc::setpgid(0, 0) == 0 {
                        Ok(())
                    } else {
                        Err(std::io::Error::last_os_error())
                    }
                });
            }
        }
        let child = command
            .spawn()
            .unwrap_or_else(|error| panic!("{} did not start: {error}", path.display()));
        unsafe { libc::close(slave) };
        let group = own_group.then_some(child.id() as i32);
        Self {
            child,
            master,
            output: Vec::new(),
            reaped: false,
            home,
            group,
        }
    }

    fn matrix(force_panic: bool) -> Self {
        Self::spawn(
            Path::new(env!("CARGO_BIN_EXE_matrix")),
            &[],
            force_panic,
            false,
        )
    }

    fn pump(&mut self) {
        let mut buf = [0u8; 65536];
        loop {
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr().cast(), buf.len()) };
            if n <= 0 {
                break;
            }
            self.output.extend_from_slice(&buf[..n as usize]);
        }
    }

    fn await_bytes(&mut self, expected: &[u8]) {
        let until = Instant::now() + Duration::from_secs(5);
        while !contains(&self.output, expected) {
            assert!(
                Instant::now() < until,
                "missing {:?}: {}",
                String::from_utf8_lossy(expected),
                dump(&self.output)
            );
            self.pump();
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn send(&self, bytes: &[u8]) {
        let wrote = unsafe { libc::write(self.master, bytes.as_ptr().cast(), bytes.len()) };
        assert_eq!(
            wrote,
            bytes.len() as isize,
            "could not write to the test pty"
        );
    }

    fn wait(&mut self, timeout: Duration) -> ExitStatus {
        let until = Instant::now() + timeout;
        let status = loop {
            self.pump();
            if let Some(status) = self
                .child
                .try_wait()
                .expect("could not wait for the test child")
            {
                break Some(status);
            }
            if Instant::now() >= until {
                break None;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        let Some(status) = status else {
            panic!(
                "process did not exit within {timeout:?}: {}",
                dump(&self.output)
            );
        };
        self.reaped = true;
        // The final write can race waitpid's readiness notification.
        for _ in 0..5 {
            self.pump();
            std::thread::sleep(Duration::from_millis(5));
        }
        status
    }

    fn assert_restored(&self) {
        assert!(
            contains(&self.output, ALT_ENTER),
            "never entered the alternate screen: {}",
            dump(&self.output)
        );
        assert!(
            contains(&self.output, RESTORE),
            "shared restore sequence missing: {}",
            dump(&self.output)
        );
    }

    fn restore_count(&self) -> usize {
        self.output.windows(RESTORE.len()).filter(|part| *part == RESTORE).count()
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if let Some(group) = self.group {
            // Also runs when a group-emptiness assertion failed: a failing
            // test must not leave the very orphan it was checking for.
            unsafe {
                libc::kill(-group, libc::SIGKILL);
            }
        }
        if !self.reaped {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        unsafe { libc::close(self.master) };
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

fn signal_widget(sig: libc::c_int) {
    let mut run = Running::matrix(false);
    run.await_bytes(b"[q]uit");
    unsafe {
        libc::kill(run.child.id() as i32, sig);
    }
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(128 + sig));
    run.assert_restored();
}

#[test]
fn q_leaves_the_original_screen() {
    let mut run = Running::matrix(false);
    run.await_bytes(b"[q]uit");
    run.send(b"q");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(0));
    run.assert_restored();
    assert_eq!(run.restore_count(), 1, "a second restore would clear the shell again: {}", dump(&run.output));
}

#[test]
fn sigint_leaves_the_original_screen() {
    signal_widget(libc::SIGINT);
}

#[test]
fn sigterm_leaves_the_original_screen() {
    signal_widget(libc::SIGTERM);
}

#[test]
fn a_panic_restores_before_printing_its_reason() {
    let mut run = Running::matrix(true);
    run.await_bytes(b"[q]uit");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(101));
    run.assert_restored();
    let left = last_at(&run.output, ALT_LEAVE).expect("the alternate screen was left");
    assert!(
        contains(
            &run.output[left + ALT_LEAVE.len()..],
            b"OPSCOPE_TEST_PANIC requested a first-frame panic"
        ),
        "the reason did not survive on the restored main screen: {}",
        dump(&run.output)
    );
}

#[test]
fn sigterm_to_the_launcher_takes_its_widget_too() {
    let mut run = Running::spawn(
        Path::new(env!("CARGO_BIN_EXE_opscope")),
        &["matrix"],
        false,
        true,
    );
    let group = run.group.expect("the launcher has its own process group");
    run.await_bytes(b"[q]uit");
    unsafe {
        libc::kill(run.child.id() as i32, libc::SIGTERM);
    }
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(128 + libc::SIGTERM));
    run.assert_restored();
    assert_eq!(run.restore_count(), 2, "the launcher did not restore after its child: {}", dump(&run.output));

    let until = Instant::now() + Duration::from_secs(2);
    let empty = loop {
        if unsafe { libc::kill(-group, 0) } != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
        {
            break true;
        }
        if Instant::now() >= until {
            break false;
        }
        std::thread::sleep(Duration::from_millis(10));
    };
    if empty {
        run.group = None;
    }
    assert!(empty, "the widget survived its launcher by two seconds");
}
