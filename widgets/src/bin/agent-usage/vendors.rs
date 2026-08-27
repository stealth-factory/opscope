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

//! Which reader answers for which tab, and the one screen that spans them.
//!
//! Each agent keeps its own file and its own shape, because they do not
//! agree on what usage even means. Only the quota lanes are common, and
//! only because the summary screen has to compare them.

use std::collections::HashMap;

use opscope_core as tc;

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

pub fn read_all(caches: &mut Caches, cfg: &Config) -> State {
    State {
        claude: crate::claude::read(caches, cfg),
        codex: crate::codex::read(caches, cfg),
        cursor: crate::cursor::read(caches, cfg),
        grok: crate::grok::read(caches, cfg),
        copilot: crate::copilot::read(caches, cfg),
        antigravity: crate::antigravity::read(caches, cfg),
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
/// Order provider groups by the lane closest to running out.
///
/// A provider with one lane at 88% outranks one whose lanes all sit at 40%,
/// however many of them there are: the structure says who owns what, the
/// ordering answers which one stops working first.
///
/// Lifted out of `summary_tab` because its test sorted a vector built in the
/// test body with a comparator written in the test body, so this ordering -
/// the whole point of that screen - was never run by it.
fn rank_by_worst_lane<T>(groups: &mut [(T, Vec<Lane>)]) {
    let worst = |g: &Vec<Lane>| g.iter().map(|l| l.pct).fold(0.0f64, f64::max);
    groups.sort_by(|a, b| worst(&b.1).total_cmp(&worst(&a.1)));
}

/// The agents publishing nothing, each under its own name.
///
/// The order used to be the other way round: one "No quota published by:
/// grok, antigravity." and then, below it, the headings explaining them.
/// That reads backwards - the reader meets a list of names and has to carry
/// them down to the paragraphs - and it stated the same fact twice for
/// every agent that had a reason, because each reason already opens by
/// saying there is no quota.
///
/// So the roll-call is what is left over. Only agents with nothing to say
/// appear in it, and when every quiet agent has explained itself there is
/// no such line at all.
///
/// Why this agent has no lane, and whether that reason is the reader's to
/// fix. Token and setting failures warn; a server that published nothing
/// does not.
fn quiet_of(name: &str, s: &State) -> (String, bool) {
    let note = match name {
        "claude" => crate::claude::why_no_lane(&s.claude),
        "codex" => crate::codex::why_no_lane(&s.codex),
        "cursor" => crate::cursor::why_no_lane(&s.cursor),
        "grok" => crate::grok::why_no_lane(&s.grok),
        "copilot" => crate::copilot::why_no_lane(&s.copilot),
        "antigravity" => crate::antigravity::why_no_lane(&s.antigravity),
        _ => String::new(),
    };
    let warn = quiet_is_actionable(&note);
    (note, warn)
}

/// Token and setting failures are the reader's to fix; a server that
/// answered and published no percentage is not.
fn quiet_is_actionable(note: &str) -> bool {
    !note.contains("answered, and published no")
}

fn quiet_from(quiet: &[&str], s: &State, w: usize, p: &Palette) -> Vec<String> {
    let said: Vec<(&str, String, bool)> = quiet
        .iter()
        .map(|name| {
            let (note, warn) = quiet_of(name, s);
            (*name, note, warn)
        })
        .collect();
    quiet_block(&said, w, p)
}

/// Split out from summary_tab because the State it needs cannot be built
/// from another module - every agent's Data keeps its fields private - so
/// this is the only shape the ordering is testable in.
fn quiet_block(said: &[(&str, String, bool)], w: usize, p: &Palette) -> Vec<String> {
    let mut rows = Vec::new();
    let mut unexplained: Vec<&str> = Vec::new();
    for (name, note, warn) in said {
        if note.is_empty() {
            unexplained.push(name);
            continue;
        }
        rows.push(tc::seg(
            &[(p.lbl.as_str(), format!("  {}", name.to_uppercase()))],
            w - 1,
        ));
        let tone = if *warn { p.warn.as_str() } else { p.dim.as_str() };
        rows.extend(
            wrap_text(note, w.saturating_sub(5).max(20))
                .into_iter()
                .map(|l| tc::seg(&[(tone, format!("   {}", l))], w - 1)),
        );
        rows.push(String::new());
    }
    if !unexplained.is_empty() {
        rows.extend(no_local(
            &format!("No quota published by: {}.", unexplained.join(", ")),
            "",
            w,
            p,
        ));
    }
    rows
}

#[cfg(test)]
fn summary_tab(s: &State, w: usize, p: &Palette) -> Vec<String> {
    summary_for(s, w, p, ORDER)
}

/// The summary, limited to the agents that have a tab on this machine.
///
/// Discovery and `agent_usage.agents` already decided who is worth showing; `[+]`
/// used to walk every name in ORDER instead, so a quiet agent the reader
/// never enabled still landed in a roll-call at the bottom, and a quiet
/// agent they *did* enable got only that roll-call rather than a section.
fn summary_for(s: &State, w: usize, p: &Palette, names: &[&str]) -> Vec<String> {
    let mut groups: Vec<(&str, Vec<Lane>)> = Vec::new();
    let mut quiet: Vec<&str> = Vec::new();
    for &name in names {
        let got = lanes_of(name, s);
        if got.is_empty() {
            quiet.push(name);
        } else {
            groups.push((name, got));
        }
    }
    if groups.is_empty() {
        let rows = quiet_from(&quiet, s, w, p);
        if rows.is_empty() {
            return no_local("No agent is publishing a quota right now.", "", w, p);
        }
        return rows;
    }
    // Grouped by provider, but the groups are ordered by their worst lane:
    // the structure says who owns what, the ordering still answers which
    // one runs out first.
    rank_by_worst_lane(&mut groups);
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
    //
    // Staleness is a mark beside the bar rather than words in this cell:
    // spelling it out cost the countdown, and a cached reading of a window
    // still open has a real countdown worth keeping. One cell for the star,
    // one line at the foot saying what it means.
    let any_stale = groups.iter().flat_map(|(_, g)| g.iter()).any(|l| l.stale);
    // Three more when anything is cached: two for the star's column and one
    // for the tilde the pace figure grows. Counted rather than guessed - the
    // pace cell is four wide for "+40%" and five for "~+40%".
    let fixed = 15 + label_w + usize::from(any_stale) * 3;
    let show_reset = (w - 1).saturating_sub(fixed + 8) >= 16;
    let tail = if show_reset { 16 } else { 0 };
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
        // Ranked by usage, except where the agent's own order already means
        // something. Claude's five-hour window sits inside its weekly one,
        // which contains the model-scoped limit in turn; Cursor's three plan
        // lanes are a widening scope - api inside auto inside included -
        // followed by an allowance that is not part of them at all. For both,
        // reading them in the agent's order says more than reading them by
        // percentage, and percentage reorders itself as the numbers move, so
        // the bar under the cursor is not the one that was there a refresh
        // ago. It also made this screen disagree with the agent's own tab
        // about the order of the very same bars.
        let mut inner = lanes.clone();
        if !matches!(*name, "claude" | "cursor") {
            inner.sort_by(|a, b| b.pct.total_cmp(&a.pct));
        }
        for lane in &inner {
            // The same break the agent's tab draws, for the same reason.
            if lane.apart {
                rows.push(String::new());
            }
            let used = (lane.pct / 100.0).clamp(0.0, 1.0);
            // "cached" used to replace the countdown outright, which threw
            // away a fact to report an adjective. A reading a few minutes old
            // still describes the window we are in: its percentage is real,
            // its reset is real, and the burn between them is worth having.
            // Cached is a note on the end of that, not a substitute for it.
            //
            // A cached reading whose window has *closed* is the other case,
            // and it is the one Claude was in - a figure fetched nine days
            // ago, for a window that ended on the sixteenth. There is no
            // countdown there to keep, and no burn either: what has been
            // spent in the current window is exactly what nobody knows.
            let ahead = lane.reset.map(|r| r - now());
            let (when, tone) = if !show_reset {
                (String::new(), p.dim.clone())
            } else {
                match ahead {
                    Some(left) if left > 0.0 => (
                        format!(
                            "  {}{}",
                            if lane.projected { "~" } else { "" },
                            left_span(left)
                        ),
                        if lane.stale { p.warn.clone() } else { p.dim.clone() },
                    ),
                    Some(_) => ("  resetting".to_string(), p.dim.clone()),
                    None => (String::new(), p.dim.clone()),
                }
            };
            // A projected window is one we rolled forward because the
            // agent's own reading had expired - so the percentage beside it
            // was measured in a window that has since closed and reset.
            //
            // The countdown survives that: the grid the window sits on is
            // still the grid. The pace does not. Pace is usage against time
            // elapsed, and how much of *this* window has been used is
            // exactly what nobody knows - computing it from the last
            // window's figure produces a confident number about a quantity
            // that was never measured.
            // Shown for a cached reading too, marked rather than withheld: a
            // blank cell tells the reader nothing, and the percentage it
            // rests on is on the same row with a star beside it.
            let closed = ahead.is_some_and(|left| left <= 0.0);
            let guessed = lane.stale || lane.projected || closed;
            let cushion = lead(lane.pct, lane.window_secs, lane.reset);
            let (pace_colour, pace_txt) = pace_cell_of(cushion, guessed, p);
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
            if any_stale {
                // Held for every lane, so the percentages stay in one
                // column whether the row beside them is cached or not.
                line.push((
                    p.warn.clone(),
                    if lane.stale { " *".into() } else { "  ".to_string() },
                ));
            }
            line.push((pct_colour(lane.pct, hue, p), pct_text(lane.pct)));
            line.push((pace_colour, pace_txt));
            line.push((tone, when));
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
        // Grok alone can be reading a file rather than a server, and a
        // reader who does not know that has no way to tell this row from
        // the five above it. Said here only while nothing is asking on
        // their behalf - once it is, the tab reports the interval and this
        // line would be repeating a setting back at them.
        if *name == "grok" && crate::grok::asks_nobody(&s.grok) {
            // Two lines because both halves are worth having and neither
            // fits beside the other at the widths these panes are dragged
            // to: what the number is, and what to do about it. Clipping one
            // to keep them on a single row would leave "agent_usage.grok_pin",
            // which names a setting that does not exist.
            rows.push(tc::seg(
                &[
                    (p.warn.as_str(), "     not live".into()),
                    (
                        p.dim.as_str(),
                        " · only your own Grok sessions update this".into(),
                    ),
                ],
                w - 1,
            ));
            rows.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    "     Set agent_usage.grok_ping in config.json to poll x.ai instead.".into(),
                )],
                w - 1,
            ));
        }
    }
    if any_stale {
        rows.push(String::new());
        rows.extend(
            wrap_text(
                "cached - the agent's own last reading rather than one fetched just \
                 now. Its own tab says when it was taken, and why.",
                w.saturating_sub(6).max(20),
            )
            .into_iter()
            .enumerate()
            .map(|(i, line)| {
                tc::seg(
                    &[
                        (p.warn.as_str(), if i == 0 { "  * " } else { "    " }.into()),
                        (p.dim.as_str(), line),
                    ],
                    w - 1,
                )
            }),
        );
    }
    if !quiet.is_empty() {
        rows.push(String::new());
        rows.extend(quiet_from(&quiet, s, w, p));
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
    tabs: &[String],
) -> Vec<String> {
    match name {
        SUMMARY_TAB => {
            let shown: Vec<&str> = ORDER
                .iter()
                .copied()
                .filter(|n| tabs.iter().any(|t| t == *n))
                .collect();
            let names: &[&str] = if shown.is_empty() { ORDER } else { &shown };
            summary_for(s, w, p, names)
        }
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

    /// Rendered rows with the colour escapes stripped, so a test can read
    /// what is on screen rather than how it was painted.
    fn plain(rows: &[String]) -> Vec<String> {
        let strip = |s: &String| {
            let mut out = String::new();
            let mut chars = s.chars();
            while let Some(c) = chars.next() {
                if c == '\u{1b}' {
                    for c in chars.by_ref() {
                        if c.is_ascii_alphabetic() {
                            break;
                        }
                    }
                } else {
                    out.push(c);
                }
            }
            out.trim_end().to_string()
        };
        rows.iter().map(strip).collect()
    }

    #[test]
    fn a_quiet_agent_is_explained_under_its_own_name() {
        let p = palette();
        let said = vec![(
            "antigravity",
            "no quota - it publishes none to any server.".to_string(),
            true,
        )];
        let rows = plain(&quiet_block(&said, 90, &p));
        let head = rows
            .iter()
            .position(|r| r.contains("ANTIGRAVITY"))
            .expect("a heading");
        let line = rows
            .iter()
            .position(|r| r.contains("publishes none"))
            .expect("the reason");
        assert!(head < line, "the sentence came before the name it is about:\n{:#?}", rows);

        // And when every quiet agent has said why, the roll-call that used
        // to lead is gone rather than repeating them.
        assert!(
            !rows.iter().any(|r| r.contains("No quota published by")),
            "named twice:\n{:#?}",
            rows
        );
    }

    #[test]
    fn an_agent_with_nothing_to_say_is_still_named() {
        // The roll-call is not dropped, only reduced to what is left.
        let p = palette();
        let said = vec![
            ("antigravity", "no quota - the app is closed.".to_string(), true),
            ("copilot", String::new(), false),
        ];
        let rows = plain(&quiet_block(&said, 90, &p));
        let roll = rows
            .iter()
            .find(|r| r.contains("No quota published by"))
            .expect("a roll-call for the one with no reason");
        assert!(roll.contains("copilot"), "{}", roll);
        assert!(!roll.contains("antigravity"), "explained and listed: {}", roll);
        // The explained one still leads with its heading.
        let head = rows.iter().position(|r| r.contains("ANTIGRAVITY")).unwrap();
        let rollat = rows.iter().position(|r| r.contains("No quota published by")).unwrap();
        assert!(head < rollat, "roll-call above the explanations:\n{:#?}", rows);
    }

    #[test]
    fn an_agent_with_no_quota_is_named_rather_than_dropped() {
        // Six agents, none publishing anything: each gets a heading and a
        // reason, the way Antigravity always did. Dumping the names into
        // one footer line was the other answer, and it taught nothing
        // about why `[+]` was empty while the agent's own tab was not.
        let p = palette();
        let s = State::default();
        let rows = plain(&summary_tab(&s, 90, &p));
        for name in ["CLAUDE", "CODEX", "CURSOR", "GROK", "COPILOT", "ANTIGRAVITY"] {
            assert!(
                rows.iter().any(|r| r.contains(name)),
                "quiet {name} had no section:\n{rows:#?}"
            );
        }
        assert!(
            !rows.iter().any(|r| r.contains("No quota published by")),
            "explained agents still in the roll-call:\n{rows:#?}"
        );
        assert!(
            !rows.iter().any(|r| r.contains("No agent is publishing a quota")),
            "generic empty-screen line hid the per-agent reasons:\n{rows:#?}"
        );
    }

    #[test]
    fn a_server_that_published_nothing_does_not_warn() {
        // The tone is about who can act, not about who is quiet. A missing
        // token is the reader's; a 200 with no percentage is the vendor's.
        assert!(quiet_is_actionable(
            "no quota · no token - Cursor has not signed in here"
        ));
        assert!(quiet_is_actionable(
            "no quota · asking x.ai is off (set agent_usage.grok_ping to poll)"
        ));
        assert!(!quiet_is_actionable(
            "no quota · Anthropic answered, and published no limit percentages."
        ));
        assert!(!quiet_is_actionable(
            "no quota · Cursor answered, and published no plan percentages for this period."
        ));
        assert!(!quiet_is_actionable(
            "no quota · GitHub answered, and published no metered pool for this period."
        ));
        assert!(!quiet_is_actionable(
            "no quota · Codex answered, and published no used_percent for this period."
        ));
        assert!(!quiet_is_actionable(
            "no quota · x.ai answered, and published no credit figure for this period."
        ));
    }

    #[test]
    fn a_hidden_agent_is_not_on_the_summary() {
        // Discovery already decided these two are the tabs; the others
        // must not show up as quiet just because ORDER still lists them.
        let p = palette();
        let s = State::default();
        let rows = plain(&summary_for(&s, 90, &p, &["claude", "cursor"]));
        assert!(rows.iter().any(|r| r.contains("CLAUDE")), "{rows:#?}");
        assert!(rows.iter().any(|r| r.contains("CURSOR")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("GROK")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("COPILOT")), "{rows:#?}");
        assert!(!rows.iter().any(|r| r.contains("ANTIGRAVITY")), "{rows:#?}");
    }

    #[test]
    fn the_summary_ranks_the_worst_agent_first() {
        // Deliberately built rather than read from disk: the ordering is
        // the whole point of this screen and must not depend on what this
        // machine happens to have installed today.
        let lane = |pct: f64| Lane {
            label: String::new(),
            pct,
            window_secs: None,
            reset: None,
            stale: false,
            projected: false,
                    apart: false,
        };
        // grok has more lanes and a higher total, and still ranks below the
        // provider with the single worst one - which a flat sort by
        // percentage would get wrong, and which the old test-local
        // comparator could not have exercised at all.
        let mut groups = vec![
            ("claude", vec![lane(12.0)]),
            ("grok", vec![lane(40.0), lane(39.0), lane(38.0)]),
            ("codex", vec![lane(88.0)]),
        ];
        rank_by_worst_lane(&mut groups);
        let order: Vec<&str> = groups.iter().map(|(n, _)| *n).collect();
        assert_eq!(order, vec!["codex", "grok", "claude"]);
    }
}
