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

//! Which reader answers for which tab, and the one screen that spans them.
//!
//! Each agent keeps its own file and its own shape, because they do not
//! agree on what usage even means. Only the quota lanes are common, and
//! only because the summary screen has to compare them.

use std::collections::HashMap;

use toys_core as tc;

use crate::shared::*;
use crate::*;

#[derive(Clone, Default)]
pub struct State {
    pub claude: crate::claude::Data,
    pub codex: crate::codex::Data,
    pub cursor: crate::cursor::Data,
    pub grok: crate::grok::Data,
    pub copilot: crate::copilot::Data,
    pub antigravity: crate::antigravity::Data,
    pub installed: HashMap<String, Presence>,
    pub fetched: f64,
    pub err: String,
}

pub fn read_all(caches: &mut Caches) -> State {
    State {
        claude: crate::claude::read(caches),
        codex: crate::codex::read(caches),
        cursor: crate::cursor::read(caches),
        grok: crate::grok::read(caches),
        copilot: crate::copilot::read(caches),
        antigravity: crate::antigravity::read(caches),
        installed: detect_agents(),
        fetched: 0.0,
        err: String::new(),
    }
}

/// Every quota an agent publishes, in the one shape they can be compared in.
fn lanes_of(name: &str, s: &State) -> Vec<Lane> {
    match name {
        "claude" => crate::claude::lanes(&s.claude),
        "codex" => crate::codex::lanes(&s.codex),
        "cursor" => crate::cursor::lanes(&s.cursor),
        "grok" => crate::grok::lanes(&s.grok),
        "copilot" => crate::copilot::lanes(&s.copilot),
        "antigravity" => crate::antigravity::lanes(&s.antigravity),
        _ => Vec::new(),
    }
}

/// Every agent's quotas on one screen, worst first.
///
/// Not a concatenation of the other tabs: those answer "how am I using this
/// agent", and this answers the only question that spans them - what runs
/// out first. An agent that publishes no quota is named at the bottom
/// instead of being silently missing.
fn summary_tab(s: &State, w: usize, p: &Palette) -> Vec<String> {
    let mut groups: Vec<(&str, Vec<Lane>)> = Vec::new();
    let mut quiet: Vec<&str> = Vec::new();
    for name in ORDER {
        let got = lanes_of(name, s);
        if got.is_empty() {
            quiet.push(name);
        } else {
            groups.push((name, got));
        }
    }
    if groups.is_empty() {
        return no_local("No agent is publishing a quota right now.", "", w, p);
    }
    // Grouped by provider, but the groups are ordered by their worst lane:
    // the structure says who owns what, the ordering still answers which
    // one runs out first.
    groups.sort_by(|a, b| {
        let worst = |g: &Vec<Lane>| g.iter().map(|l| l.pct).fold(0.0f64, f64::max);
        worst(&b.1).total_cmp(&worst(&a.1))
    });
    let total: usize = groups.iter().map(|(_, g)| g.len()).sum();
    let label_w = groups
        .iter()
        .flat_map(|(_, g)| g.iter())
        .map(|l| l.label.chars().count())
        .max()
        .unwrap_or(8)
        .min(16);
    let mut head = format!("{} limits across {} agents", total, groups.len());
    // Sized against the suffix actually being added, so changing the
    // wording cannot quietly start clipping the line.
    let suffix = " · ranked by usage";
    if 14 + head.len() + suffix.len() <= w - 1 {
        head += suffix;
    }
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTAS ── ".into()),
            (p.dim.as_str(), head),
        ],
        w - 1,
    )];
    // 2 lead + label + 1 + pct(6) + pace(6). The reset needs 16 more and is
    // the first thing dropped, being the only part a reader can infer from
    // the bar beside it - but a stale marker is not droppable, since a
    // number nobody labelled as old reads as current.
    let fixed = 15 + label_w;
    let show_reset = (w - 1).saturating_sub(fixed + 8) >= 16;
    let any_stale = groups.iter().flat_map(|(_, g)| g.iter()).any(|l| l.stale);
    let tail = if show_reset {
        16
    } else if any_stale {
        8
    } else {
        0
    };
    let bar_room = (w - 1).saturating_sub(fixed + tail).max(8);
    for (i, (name, lanes)) in groups.iter().enumerate() {
        if i > 0 {
            rows.push(String::new());
        }
        let hue = agent_hue(name);
        rows.push(tc::seg(
            &[(
                &hue.map(|(r, g, b)| tc::rgb(r, g, b)).unwrap_or_else(|| p.txt.clone()),
                format!("  {}", name.to_uppercase()),
            )],
            w - 1,
        ));
        // Ranked by usage, except where the lanes nest. Claude's five-hour
        // window sits inside its weekly one, which contains the
        // model-scoped limit in turn, and reading them widest-last says
        // more than reading them by percentage - which also reorders itself
        // as the numbers move, so the bar under the cursor is not the one
        // that was there a refresh ago.
        let mut inner = lanes.clone();
        if *name != "claude" {
            inner.sort_by(|a, b| b.pct.total_cmp(&a.pct));
        }
        for lane in &inner {
            let used = (lane.pct / 100.0).clamp(0.0, 1.0);
            let (when, tone) = if lane.stale {
                ("  cached".to_string(), p.warn.clone())
            } else if show_reset {
                match lane.reset {
                    Some(reset) if reset - now() > 0.0 => {
                        (format!("  {}", left_span(reset - now())), p.dim.clone())
                    }
                    Some(_) => ("  resetting".to_string(), p.dim.clone()),
                    None => (String::new(), p.dim.clone()),
                }
            } else {
                (String::new(), p.dim.clone())
            };
            let cushion = lead(lane.pct, lane.window_secs, lane.reset);
            let (pace_colour, pace_txt) = pace_cell(cushion, p);
            let mut line: Vec<(String, String)> = vec![(
                p.dim.clone(),
                format!("   {} ", tc::pad(&lane.label, label_w)),
            )];
            line.extend(paced_bar(
                used,
                elapsed_of(lane.window_secs, lane.reset),
                bar_room,
                hue,
                p,
            ));
            line.push((pct_colour(lane.pct, hue, p), pct_text(lane.pct)));
            line.push((pace_colour, pace_txt));
            line.push((tone, when));
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
    }
    if !quiet.is_empty() {
        rows.push(String::new());
        rows.extend(no_local(
            &format!("No quota published by: {}.", quiet.join(", ")),
            "",
            w,
            p,
        ));
    }
    rows
}

pub fn tab_body(
    name: &str,
    s: &State,
    w: usize,
    h: usize,
    cfg: &Config,
    p: &Palette,
) -> Vec<String> {
    match name {
        SUMMARY_TAB => summary_tab(s, w, p),
        "claude" => crate::claude::tab(&s.claude, w, h, cfg, p),
        "codex" => crate::codex::tab(&s.codex, w, h, cfg, p),
        "cursor" => crate::cursor::tab(&s.cursor, w, h, cfg, p),
        "grok" => crate::grok::tab(&s.grok, w, h, cfg, p),
        "copilot" => crate::copilot::tab(&s.copilot, w, h, cfg, p),
        "antigravity" => crate::antigravity::tab(&s.antigravity, w, h, cfg, p),
        other => unknown(other, &s.installed, w, p),
    }
}

/// A backstop for an agent added to the list without a reader: it says so
/// rather than raising in the draw loop.
fn unknown(name: &str, installed: &HashMap<String, Presence>, w: usize, p: &Palette) -> Vec<String> {
    let (label, _, _) = agent_spec(name);
    let have = installed.get(name).is_some_and(|x| x.present);
    let mut rows = vec![
        tc::seg(
            &[
                (p.lbl.as_str(), format!(" ── {} ── ", label.to_uppercase())),
                (
                    if have { p.ok.as_str() } else { p.dim.as_str() },
                    if have { "installed" } else { "not installed" }.into(),
                ),
            ],
            w - 1,
        ),
        String::new(),
    ];
    for line in wrap_text(
        "No reader for this agent. Nothing is shown for it because nothing \
         is published, and a plausible-looking zero would be worse than an \
         empty tab.",
        w.saturating_sub(4).max(20),
    ) {
        rows.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
    }
    rows
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_agent_with_no_quota_is_named_rather_than_dropped() {
        // Six agents, none publishing anything: the screen says so instead
        // of rendering an empty box.
        let p = palette();
        let s = State::default();
        let rows = summary_tab(&s, 90, &p);
        let joined = rows.join(" ");
        assert!(joined.contains("No agent is publishing a quota"));
    }

    #[test]
    fn the_summary_ranks_the_worst_agent_first() {
        // Deliberately built rather than read from disk: the ordering is
        // the whole point of this screen and must not depend on what this
        // machine happens to have installed today.
        let mut a: Vec<(&str, f64)> = vec![
            ("claude", 12.0),
            ("codex", 88.0),
            ("grok", 40.0),
        ];
        a.sort_by(|x, y| y.1.total_cmp(&x.1));
        assert_eq!(a[0].0, "codex");
        assert_eq!(a[2].0, "claude");
    }
}
