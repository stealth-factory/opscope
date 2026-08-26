// terminal-toys - small dependency-free terminal widgets
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

use std::process::Command;

#[test]
fn node_tests() {
    let npm = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../npm");
    let out = Command::new("node")
        .args(["--test", "test.js"])
        .current_dir(&npm)
        .output()
        .unwrap_or_else(|e| panic!("node is required to test the npm packages: {e}"));
    if !out.status.success() {
        panic!(
            "npm tests failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
    }
}
