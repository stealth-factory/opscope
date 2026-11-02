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

//! Core shows every binary's notices, and no widget wires them.
//!
//! Each binary is started on a pseudo-terminal with a config file others
//! can read, which is the Core notice every pane shares. The test reads
//! what the binary draws: the `[!]` counter on the top row, the banner,
//! and the notices screen once `!` is typed or the counter is clicked.
//! `matrix` is one of the two because it computes nothing and wires
//! nothing, so anything it shows came from core - and it is the binary
//! that was handed a Core warning it had no way to show. The other is an
//! ordinary widget with a filter box, which also carries the rule that a
//! `!` typed into a text field is text.
//!
//! Neither touches the network: `vercel-deployments` has no token here,
//! so it never asks Vercel for anything.

mod support;

use std::os::fd::FromRawFd;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// A binary on its own pty, with a throwaway config readable by others and
/// a state directory of its own, so no dismissal from a real pane hides
/// the notice this is looking for.
struct Pane {
    child: Child,
    master: i32,
    home: PathBuf,
}

impl Pane {
    fn start(bin: &str, tag: &str) -> Pane {
        Pane::start_sized(bin, tag, ROWS)
    }

    fn start_sized(bin: &str, tag: &str, rows: u16) -> Pane {
        use std::os::unix::fs::PermissionsExt;
        let home = std::env::temp_dir().join(format!("opscope-notices-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&home);
        let config = home.join("config/opscope");
        std::fs::create_dir_all(&config).unwrap();
        std::fs::create_dir_all(home.join("state/opscope")).unwrap();
        let file = config.join("config.json");
        // Analytics answered "no" and its one opt-out event already recorded
        // as sent, so the consent splash does not cover the pane and no
        // event leaves a test run, even one built with the release secrets.
        std::fs::write(&file, "{\"analytics\":{\"enabled\":false}}\n").unwrap();
        std::fs::write(home.join("state/opscope/usage.json"), "{\"decided_at\":1}\n").unwrap();
        std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
        let (master, slave) = support::pty(COLS, rows);
        let io = |fd: i32| unsafe { Stdio::from_raw_fd(libc::dup(fd)) };
        let child = Command::new(bin)
            .current_dir(&home)
            .env("HOME", &home)
            .env("XDG_CONFIG_HOME", home.join("config"))
            .env("XDG_STATE_HOME", home.join("state"))
            .env("TERM", "xterm-256color")
            .env_remove("OPSCOPE_CONFIG")
            .env_remove("VERCEL_TOKEN")
            .env_remove("HERDR_ENV")
            .env_remove("TMUX")
            .stdin(io(slave))
            .stdout(io(slave))
            .stderr(io(slave))
            .spawn()
            .expect("the binary did not start");
        unsafe { libc::close(slave) };
        Pane { child, master, home }
    }

    /// Everything drawn over `for_how_long`, as text with its escapes.
    fn read(&self, for_how_long: Duration) -> String {
        let until = Instant::now() + for_how_long;
        let mut out = Vec::new();
        let mut buf = [0u8; 65536];
        while Instant::now() < until {
            let n = unsafe { libc::read(self.master, buf.as_mut_ptr().cast(), buf.len()) };
            if n > 0 {
                out.extend_from_slice(&buf[..n as usize]);
            } else {
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        String::from_utf8_lossy(&out).into_owned()
    }

    /// Read until `needle` has been drawn, or `limit` passes.
    fn read_until(&self, needle: &str, limit: Duration) -> String {
        let until = Instant::now() + limit;
        let mut seen = String::new();
        while Instant::now() < until && !seen.contains(needle) {
            seen.push_str(&self.read(Duration::from_millis(100)));
        }
        seen
    }

    fn type_keys(&self, keys: &str) {
        let n = unsafe { libc::write(self.master, keys.as_ptr().cast(), keys.len()) };
        assert_eq!(n as usize, keys.len());
    }
}

impl Drop for Pane {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        unsafe { libc::close(self.master) };
        let _ = std::fs::remove_dir_all(&self.home);
    }
}

const COUNTER: &str = "[!] 1 warning";
const BANNER: &str = "! config.json is readable by others; chmod 600 it";

/// The one-based column a click on the counter is sent to: the top row
/// ends in the counter, a space, and ` v<version>`.
fn counter_column() -> usize {
    let version = format!(" v{}", env!("CARGO_PKG_VERSION"));
    COLS as usize - version.chars().count() - COUNTER.len() + 1
}

/// Shows the notice, opens the screen on `!` and on a click on the counter,
/// and comes back from it each time.
fn shows_and_opens(bin: &str, tag: &str, screen: &str) {
    let pane = Pane::start(bin, tag);
    let first = pane.read_until(BANNER, Duration::from_secs(5));
    assert!(first.contains(COUNTER), "{tag}: no counter on the top row");
    assert!(first.contains(BANNER), "{tag}: no banner");

    pane.type_keys("!");
    let opened = pane.read_until(screen, Duration::from_secs(3));
    assert!(opened.contains(screen), "{tag}: `!` did not open the notices screen");
    pane.type_keys("\x1b");
    let back = pane.read_until(COUNTER, Duration::from_secs(3));
    assert!(back.contains(COUNTER), "{tag}: the widget did not come back from the screen");

    // A click on the counter is the second route to `!`.
    let col = counter_column();
    pane.type_keys(&format!("\x1b[<0;{col};1M\x1b[<0;{col};1m"));
    let clicked = pane.read_until(screen, Duration::from_secs(3));
    assert!(clicked.contains(screen), "{tag}: a click on the counter did not open the screen");
    pane.type_keys("\x1b");
    pane.read(Duration::from_millis(300));
}

#[test]
fn matrix_shows_a_core_notice_and_opens_it_on_bang() {
    shows_and_opens(env!("CARGO_BIN_EXE_matrix"), "matrix", "MATRIX NOTICES");
}

#[test]
fn an_ordinary_widget_shows_a_core_notice_and_opens_it_on_bang() {
    shows_and_opens(
        env!("CARGO_BIN_EXE_vercel-deployments"),
        "vercel",
        "VERCEL-DEPLOYMENTS NOTICES",
    );
}

#[test]
fn bang_typed_into_a_filter_is_text() {
    let pane = Pane::start(env!("CARGO_BIN_EXE_vercel-deployments"), "filter");
    pane.read_until(BANNER, Duration::from_secs(5));
    pane.type_keys("/");
    pane.read(Duration::from_millis(300));
    pane.type_keys("a!b");
    let typed = pane.read_until("/a!b", Duration::from_secs(3));
    assert!(typed.contains("/a!b"), "the filter did not take the `!`");
    assert!(!typed.contains("NOTICES"), "`!` in a filter opened the notices screen");
    // Closing the field gives `!` back to core.
    pane.type_keys("\x1b");
    pane.read(Duration::from_millis(300));
    pane.type_keys("!");
    let opened = pane.read_until("VERCEL-DEPLOYMENTS NOTICES", Duration::from_secs(3));
    assert!(opened.contains("VERCEL-DEPLOYMENTS NOTICES"), "`!` after the filter closed did nothing");
}

/// `/a!b` in one write, as a paste or a fast typist sends it. The `!`
/// follows the `/` that opens the filter, so it is text even though the
/// filter was closed when the read began.
#[test]
fn bang_in_the_same_read_as_the_filter_opener_is_text() {
    let pane = Pane::start(env!("CARGO_BIN_EXE_vercel-deployments"), "batched");
    pane.read_until(BANNER, Duration::from_secs(5));
    pane.type_keys("/a!b");
    let typed = pane.read_until("/a!b", Duration::from_secs(3));
    let typed = typed + &pane.read(Duration::from_millis(300));
    assert!(typed.contains("/a!b"), "the filter did not take `a!b` from one read");
    assert!(!typed.contains("NOTICES"), "a `!` after the filter opener opened the notices screen");
}

/// A click on the counter is never typing: it opens the screen from an
/// open filter and leaves the filter's text as it was.
#[test]
fn a_counter_click_opens_the_screen_from_a_filter() {
    let pane = Pane::start(env!("CARGO_BIN_EXE_vercel-deployments"), "click-filter");
    pane.read_until(BANNER, Duration::from_secs(5));
    pane.type_keys("/");
    pane.read(Duration::from_millis(300));
    pane.type_keys("a");
    pane.read_until("/a", Duration::from_secs(3));
    let col = counter_column();
    pane.type_keys(&format!("\x1b[<0;{col};1M\x1b[<0;{col};1m"));
    let opened = pane.read_until("VERCEL-DEPLOYMENTS NOTICES", Duration::from_secs(3));
    assert!(opened.contains("VERCEL-DEPLOYMENTS NOTICES"), "a click on the counter did not open the screen");
    assert!(!opened.contains("/a!"), "the click went into the filter as `!`");
    pane.type_keys("\x1b");
    let back = pane.read(Duration::from_millis(500));
    assert!(!back.contains("/a!"), "the filter came back with a `!` in it");
}

/// clocks builds its world clock from the rows the clock above leaves, so
/// a banner taking them would take the section. With a notice outstanding
/// on an 80 x 23 pane, the banner gives way and the world clock is drawn.
/// 23 is the tight height once the weather row sits under the date: 22
/// fitted the clock, the countdowns and one city, and the weather line
/// took the row that city had.
#[test]
fn the_world_clock_survives_a_pending_banner() {
    let pane = Pane::start_sized(env!("CARGO_BIN_EXE_clocks"), "clocks", 23);
    let seen = pane.read_until("WORLD CLOCK", Duration::from_secs(4));
    let seen = seen + &pane.read(Duration::from_millis(300));
    assert!(seen.contains(COUNTER), "no notice outstanding, so this tests nothing");
    assert!(seen.contains("WORLD CLOCK"), "the banner took the world clock's rows");
}
