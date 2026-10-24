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

//! A widget whose terminal goes away exits, rather than running on with
//! nobody able to see it.
//!
//! The widget is started on a pseudo-terminal that is not its controlling
//! terminal, so closing the master sends it no SIGHUP: the only way it can
//! learn the terminal is gone is by noticing, which is the case that left
//! a copy of agent-usage running for six hours. `matrix` is the one run
//! because it asks nothing of the network or the disk, and the hang-up is
//! noticed in core, where every widget's input comes from.

use std::os::fd::FromRawFd;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn pty() -> (i32, i32) {
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "no pty");
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let name = libc::ptsname(master);
        assert!(!name.is_null());
        let slave = libc::open(name, libc::O_RDWR | libc::O_NOCTTY);
        assert!(slave >= 0, "could not open the slave");
        let size = libc::winsize { ws_row: 24, ws_col: 80, ws_xpixel: 0, ws_ypixel: 0 };
        libc::ioctl(slave, libc::TIOCSWINSZ, &size);
        // Not inherited: a widget holding its own copy of the master would
        // keep the terminal open after this test closed it.
        for fd in [master, slave] {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        (master, slave)
    }
}

/// Read whatever the widget has drawn, so it never blocks on a full pty.
fn drain(master: i32, for_how_long: Duration) {
    let until = Instant::now() + for_how_long;
    let mut buf = [0u8; 65536];
    while Instant::now() < until {
        let n = unsafe { libc::read(master, buf.as_mut_ptr().cast(), buf.len()) };
        if n <= 0 {
            std::thread::sleep(Duration::from_millis(10));
        }
    }
}

#[test]
fn a_widget_exits_within_a_second_of_its_terminal_closing() {
    let (master, slave) = pty();
    let home = std::env::temp_dir().join(format!("opscope-hangup-{}", std::process::id()));
    std::fs::create_dir_all(&home).unwrap();
    let io = |fd: i32| unsafe { Stdio::from_raw_fd(libc::dup(fd)) };
    let mut child = Command::new(env!("CARGO_BIN_EXE_matrix"))
        .env("HOME", &home)
        .env("TERM", "xterm-256color")
        .env_remove("HERDR_ENV")
        .env_remove("TMUX")
        .stdin(io(slave))
        .stdout(io(slave))
        .stderr(io(slave))
        .spawn()
        .expect("matrix did not start");
    unsafe { libc::close(slave) };
    drain(master, Duration::from_millis(1500));
    assert!(
        child.try_wait().unwrap().is_none(),
        "matrix exited before its terminal closed, so this proves nothing"
    );
    unsafe { libc::close(master) };
    let closed = Instant::now();
    let exited = loop {
        if let Some(status) = child.try_wait().unwrap() {
            break Some(status);
        }
        if closed.elapsed() > Duration::from_secs(3) {
            break None;
        }
        std::thread::sleep(Duration::from_millis(20));
    };
    let took = closed.elapsed();
    if exited.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_dir_all(&home);
    assert!(exited.is_some(), "matrix was still running 3s after its terminal closed");
    // 129 is what the hang-up path exits with. Any other code means it
    // left some other way, which would otherwise pass here.
    assert_eq!(
        exited.unwrap().code(),
        Some(128 + libc::SIGHUP),
        "matrix did not exit through the hang-up path"
    );
    assert!(took < Duration::from_secs(1), "matrix took {took:?} to notice its terminal closed");
}
