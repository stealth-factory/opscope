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

//! What the tests that start a binary on a terminal of its own share.

/// A pseudo-terminal `cols` x `rows`: the master, non-blocking, and the
/// slave's descriptor. Neither is inherited by a child, so a widget never
/// holds a copy of the master and keeps the terminal open after a test
/// closed it.
pub fn pty(cols: u16, rows: u16) -> (i32, i32) {
    unsafe {
        let master = libc::posix_openpt(libc::O_RDWR | libc::O_NOCTTY);
        assert!(master >= 0, "no pty");
        assert_eq!(libc::grantpt(master), 0);
        assert_eq!(libc::unlockpt(master), 0);
        let slave = open_slave(master);
        assert!(slave >= 0, "could not open the slave");
        let size = libc::winsize { ws_row: rows, ws_col: cols, ws_xpixel: 0, ws_ypixel: 0 };
        libc::ioctl(slave, libc::TIOCSWINSZ, &size);
        for fd in [master, slave] {
            libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC);
        }
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
        (master, slave)
    }
}

/// Open the slave side of `master`.
///
/// `ptsname` answers in one buffer for the whole process, and cargo runs
/// tests in parallel: another test's `ptsname` between this one's and its
/// `open` hands this test the other's terminal. Linux has `ptsname_r`,
/// which writes into the caller's buffer.
#[cfg(target_os = "linux")]
unsafe fn open_slave(master: i32) -> i32 {
    let mut name = [0 as libc::c_char; 128];
    assert_eq!(unsafe { libc::ptsname_r(master, name.as_mut_ptr(), name.len()) }, 0);
    unsafe { libc::open(name.as_ptr(), libc::O_RDWR | libc::O_NOCTTY) }
}

/// Elsewhere the name and the `open` are held under one lock, so no other
/// test's `ptsname` can land between them.
#[cfg(not(target_os = "linux"))]
unsafe fn open_slave(master: i32) -> i32 {
    static NAMING: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _held = NAMING.lock().unwrap_or_else(|e| e.into_inner());
    let name = unsafe { libc::ptsname(master) };
    assert!(!name.is_null());
    unsafe { libc::open(name, libc::O_RDWR | libc::O_NOCTTY) }
}
