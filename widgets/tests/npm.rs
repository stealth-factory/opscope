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

//! The npm packages are generated, not hand-maintained. A check that
//! lives only in ci.yml is a check that `cargo test` — the gate this
//! repo names — never sees.

use std::io::ErrorKind;
use std::process::Command;

#[test]
fn node_tests() {
    // node is a tool, not a library: rust-only `cargo test` has to stay
    // green without it. CI already runs the same file via `node --test`.
    // Anything other than "no such binary" is still a failure — a node
    // that exists and cannot start is not a skip.
    let npm = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../npm");
    let out = match Command::new("node")
        .args(["--test", "test.js"])
        .current_dir(&npm)
        .output()
    {
        Ok(out) => out,
        Err(e) if e.kind() == ErrorKind::NotFound => return,
        Err(e) => panic!("failed to run node --test npm/test.js: {e}"),
    };
    if !out.status.success() {
        panic!(
            "npm tests failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
