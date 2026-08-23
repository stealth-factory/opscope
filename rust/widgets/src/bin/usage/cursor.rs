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

//! Cursor: its edit-tracking database, and the lanes its Usage view shows.
//!
//! Not read yet. Every function here is honest about that rather than
//! returning a plausible zero, which is the rule the whole widget follows
//! for an agent that publishes nothing.

use toys_core as tc;

use crate::shared::*;
use crate::*;

#[derive(Clone, Default)]
pub struct Data {}

pub fn read(_caches: &mut Caches) -> Data {
    Data::default()
}

/// Every quota this agent publishes, for the summary screen.
pub fn lanes(_d: &Data) -> Vec<Lane> {
    Vec::new()
}

pub fn tab(_d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    let mut rows = vec![
        tc::seg(
            &[
                (p.lbl.as_str(), " ── CURSOR ── ".into()),
                (p.dim.as_str(), "no reader in this build yet".into()),
            ],
            w - 1,
        ),
        String::new(),
    ];
    for line in wrap_text(
        "usage.py reads this agent; the Rust port does not yet. Nothing is \
         shown rather than a plausible zero.",
        w.saturating_sub(4).max(20),
    ) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
    }
    rows
}
