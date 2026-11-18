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
    initial_lflag: libc::tcflag_t,
}

impl Running {
    fn spawn(path: &Path, args: &[&str], force_panic: bool, own_group: bool) -> Self {
        Self::spawn_configured(path, args, force_panic, own_group, None, false)
    }

    fn spawn_configured(
        path: &Path,
        args: &[&str],
        force_panic: bool,
        own_group: bool,
        backtrace: Option<&str>,
        null_stdin: bool,
    ) -> Self {
        let (master, slave) = support::pty(80, 24);
        let mut initial: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(master, &mut initial) },
            0,
            "could not read initial pty settings"
        );
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
            .env_remove("RUST_BACKTRACE")
            .env_remove("OPSCOPE_TEST_PANIC")
            .stdin(if null_stdin { Stdio::null() } else { io(slave) })
            .stdout(io(slave))
            .stderr(io(slave));
        if force_panic {
            command.env("OPSCOPE_TEST_PANIC", "1");
        }
        if let Some(backtrace) = backtrace {
            command.env("RUST_BACKTRACE", backtrace);
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
            initial_lflag: initial.c_lflag,
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

    fn await_count(&mut self, expected: &[u8], count: usize) {
        let until = Instant::now() + Duration::from_secs(5);
        while self
            .output
            .windows(expected.len())
            .filter(|part| *part == expected)
            .count()
            < count
        {
            assert!(
                Instant::now() < until,
                "missing {count} copies of {:?}: {}",
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

    fn await_stopped(&mut self) {
        let until = Instant::now() + Duration::from_secs(3);
        loop {
            self.pump();
            let mut status = 0;
            let waited = unsafe {
                libc::waitpid(
                    self.child.id() as i32,
                    &mut status,
                    libc::WUNTRACED | libc::WNOHANG,
                )
            };
            if waited == self.child.id() as i32 && libc::WIFSTOPPED(status) {
                return;
            }
            assert!(
                Instant::now() < until,
                "widget did not stop: {}",
                dump(&self.output)
            );
            std::thread::sleep(Duration::from_millis(10));
        }
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
        self.output
            .windows(RESTORE.len())
            .filter(|part| *part == RESTORE)
            .count()
    }

    fn assert_termios_restored(&self) {
        let mut current: libc::termios = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe { libc::tcgetattr(self.master, &mut current) },
            0,
            "could not read final pty settings"
        );
        let mask = (libc::ECHO | libc::ICANON) as libc::tcflag_t;
        assert_eq!(
            current.c_lflag & mask,
            self.initial_lflag & mask,
            "echo/canonical input did not return to its initial state"
        );
    }

    fn assert_panic_is_on_the_main_screen(&self) {
        self.assert_restored();
        let left = last_at(&self.output, ALT_LEAVE).expect("the alternate screen was left");
        assert!(
            contains(
                &self.output[left + ALT_LEAVE.len()..],
                b"OPSCOPE_TEST_PANIC requested a first-frame panic"
            ),
            "the reason did not survive on the restored main screen: {}",
            dump(&self.output)
        );
    }
}

/// An isolated launcher beside a widget that ignores every graceful request.
/// /bin/sh loops with no subprocess, so its process group is exactly the
/// launcher and widget and cannot leave an unrelated sleeper in this test.
struct StubbornLauncher {
    dir: PathBuf,
}

impl StubbornLauncher {
    fn new() -> Self {
        use std::os::unix::fs::PermissionsExt;
        let unique = NEXT_HOME.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("opscope-stubborn-{}-{unique}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("stubborn fixture directory");
        let launcher = dir.join("opscope");
        std::fs::copy(env!("CARGO_BIN_EXE_opscope"), &launcher).expect("copy the launcher");
        let matrix = dir.join("matrix");
        std::fs::write(
            &matrix,
            "#!/bin/sh\ntrap '' INT TERM HUP\n/bin/stty -echo -icanon\nprintf 'READY\\n'\nwhile :; do :; done\n",
        )
        .unwrap();
        std::fs::set_permissions(&matrix, std::fs::Permissions::from_mode(0o755)).unwrap();
        Self { dir }
    }

    fn launcher(&self) -> PathBuf {
        self.dir.join("opscope")
    }
}

impl Drop for StubbornLauncher {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
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
    assert_eq!(
        run.restore_count(),
        1,
        "a second restore would clear the shell again: {}",
        dump(&run.output)
    );
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
    run.assert_panic_is_on_the_main_screen();
}

#[test]
fn a_panic_backtrace_is_printed_after_restore() {
    let mut run = Running::spawn_configured(
        Path::new(env!("CARGO_BIN_EXE_matrix")),
        &[],
        true,
        false,
        Some("1"),
        false,
    );
    run.await_bytes(b"[q]uit");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(101));
    run.assert_panic_is_on_the_main_screen();
    let left = last_at(&run.output, ALT_LEAVE).expect("the alternate screen was left");
    assert!(
        contains(&run.output[left + ALT_LEAVE.len()..], b"stack backtrace:"),
        "the requested backtrace was not replayed after restore: {}",
        dump(&run.output)
    );
}

#[test]
fn rust_backtrace_zero_does_not_print_a_backtrace() {
    let mut run = Running::spawn_configured(
        Path::new(env!("CARGO_BIN_EXE_matrix")),
        &[],
        true,
        false,
        Some("0"),
        false,
    );
    run.await_bytes(b"[q]uit");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(101));
    run.assert_panic_is_on_the_main_screen();
    assert!(!run
        .output
        .windows(b"stack backtrace:".len())
        .any(|part| part == b"stack backtrace:"));
}

#[test]
fn a_panic_without_tty_stdin_still_restores_stdout() {
    let mut run = Running::spawn_configured(
        Path::new(env!("CARGO_BIN_EXE_matrix")),
        &[],
        true,
        false,
        None,
        true,
    );
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(101));
    run.assert_panic_is_on_the_main_screen();
}

fn assert_launcher_reaped(
    mut run: Running,
    expected_restores: usize,
    expected_code: i32,
    sig: i32,
) {
    let group = run.group.expect("the launcher has its own process group");
    let sent = Instant::now();
    unsafe {
        libc::kill(run.child.id() as i32, sig);
    }
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(expected_code));
    if expected_code == 128 + libc::SIGKILL {
        assert!(
            sent.elapsed() >= Duration::from_millis(900),
            "SIGKILL skipped the child's grace period"
        );
    }
    run.assert_restored();
    assert_eq!(
        run.restore_count(),
        expected_restores,
        "the launcher did not restore after its child: {}",
        dump(&run.output)
    );

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
    run.assert_termios_restored();
}

#[test]
fn sigterm_to_the_direct_launcher_takes_its_widget_too() {
    let mut run = Running::spawn(
        Path::new(env!("CARGO_BIN_EXE_opscope")),
        &["matrix"],
        false,
        true,
    );
    run.await_bytes(b"[q]uit");
    assert_launcher_reaped(run, 2, 128 + libc::SIGTERM, libc::SIGTERM);
}

#[test]
fn sighup_to_the_launcher_is_repaired_by_the_parent() {
    let mut run = Running::spawn(
        Path::new(env!("CARGO_BIN_EXE_opscope")),
        &["matrix"],
        false,
        true,
    );
    run.await_bytes(b"[q]uit");
    // SIGHUP kills the child without its cleanup handler. The launcher's own
    // restore is therefore the only one, and 129 is inherited from the child.
    assert_launcher_reaped(run, 1, 128 + libc::SIGHUP, libc::SIGHUP);
}

#[test]
fn sigterm_to_the_menu_launcher_takes_its_widget_too() {
    let mut run = Running::spawn(Path::new(env!("CARGO_BIN_EXE_opscope")), &[], false, true);
    run.await_bytes(b"21 widgets");
    // matrix is the fourteenth row, after thirteen ordinary down keys.
    // Enter starts it in the same supervised handoff the footer offers.
    run.send(b"jjjjjjjjjjjjj\r");
    // The menu's entry is the first 1049h; matrix's startup is the second.
    // The footer itself cannot be the readiness marker because both screens
    // deliberately say [q]uit.
    run.await_count(ALT_ENTER, 2);
    // One restore hands the menu to the child. The signalled child and the
    // launcher's own final restore supply the other two.
    assert_launcher_reaped(run, 3, 128 + libc::SIGTERM, libc::SIGTERM);
}

#[test]
fn a_stubborn_widget_is_killed_after_its_grace_period() {
    let fixture = StubbornLauncher::new();
    let mut run = Running::spawn(&fixture.launcher(), &["matrix"], false, true);
    run.await_bytes(b"READY");
    // The fake widget never entered 1049, so the launcher's own forced
    // restore is the only one. SIGKILL turns the child's status into 137.
    assert_launcher_reaped(run, 1, 128 + libc::SIGKILL, libc::SIGTERM);
}

#[test]
fn a_menu_child_returning_can_quit_normally() {
    let mut run = Running::spawn(Path::new(env!("CARGO_BIN_EXE_opscope")), &[], false, true);
    run.await_bytes(b"21 widgets");
    run.send(b"jjjjjjjjjjjjj\r");
    run.await_count(ALT_ENTER, 2);
    run.send(b"q");
    // The menu reclaims after the child quit, then quits normally itself.
    run.await_count(ALT_ENTER, 3);
    run.send(b"q");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(0));
    run.assert_restored();
    // Parent handoff, child q and parent q. Each restore is emitted once.
    assert_eq!(
        run.restore_count(),
        3,
        "a normal handoff duplicated or lost an exit: {}",
        dump(&run.output)
    );
    run.assert_termios_restored();
    run.group = None;
}

#[test]
fn suspend_and_resume_enter_a_fresh_alternate_screen() {
    let mut run = Running::matrix(false);
    run.await_bytes(b"[q]uit");
    unsafe {
        libc::kill(run.child.id() as i32, libc::SIGTSTP);
    }
    run.await_stopped();
    assert_eq!(
        run.restore_count(),
        1,
        "suspend did not leave 1049: {}",
        dump(&run.output)
    );
    run.assert_termios_restored();

    unsafe {
        libc::kill(run.child.id() as i32, libc::SIGCONT);
    }
    run.await_count(ALT_ENTER, 2);
    run.send(b"q");
    let status = run.wait(Duration::from_secs(3));
    assert_eq!(status.code(), Some(0));
    assert_eq!(
        run.restore_count(),
        2,
        "resume did not return to a fresh screen: {}",
        dump(&run.output)
    );
    run.assert_termios_restored();
}
