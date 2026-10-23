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
    pub claude: Vec<crate::claude::Data>,
    pub codex: crate::codex::Data,
    pub cursor: crate::cursor::Data,
    pub grok: crate::grok::Data,
    pub copilot: crate::copilot::Data,
    pub antigravity: crate::antigravity::Data,
    pub notion: crate::notion::Data,
    pub perplexity: crate::perplexity::Data,
    pub devin: crate::devin::Data,
    pub droid: crate::droid::Data,
    pub installed: HashMap<String, Presence>,
    pub fetched: f64,
    pub err: String,
}

/// Whether Notion may be asked, on the same terms: it sends the reader's
/// own session to Notion, so a tab the fallback brought back does not
/// count as a choice.
fn notion_asked(installed: &HashMap<String, Presence>, cfg: &Config) -> bool {
    chosen_agents(installed, cfg).iter().any(|t| t == "notion")
}

/// Whether Perplexity may be asked, on the same terms: it sends the reader's
/// own session to Perplexity.
fn perplexity_asked(installed: &HashMap<String, Presence>, cfg: &Config) -> bool {
    chosen_agents(installed, cfg).iter().any(|t| t == "perplexity")
}

/// Whether Devin may be asked. It sends the reader's token to Devin, so a
/// tab the fallback brought back does not count as a choice.
fn devin_asked(installed: &HashMap<String, Presence>, cfg: &Config) -> bool {
    chosen_agents(installed, cfg).iter().any(|t| t == "devin")
}

/// Whether Droid may be asked, on the same terms: the Factory API key leaves
/// this machine only when the reader actually picked Droid.
fn droid_asked(installed: &HashMap<String, Presence>, cfg: &Config) -> bool {
    chosen_agents(installed, cfg).iter().any(|t| t == "droid")
}

pub fn read_all(
    caches: &mut Caches,
    cfg: &Config,
    installed: HashMap<String, Presence>,
) -> State {
    // The poller already checked presence for this pass. Checking again
    // would repeat the binary, file, and credential lookups, and a second
    // answer could disagree with the map the tabs were drawn from.
    let notion_shown = notion_asked(&installed, cfg);
    let perplexity_shown = perplexity_asked(&installed, cfg);
    let devin_shown = devin_asked(&installed, cfg);
    let droid_shown = droid_asked(&installed, cfg);
    let mut state = State {
        claude: crate::claude::read(caches, cfg),
        codex: crate::codex::read(caches, cfg),
        cursor: crate::cursor::read(caches, cfg),
        grok: crate::grok::read(caches, cfg),
        copilot: crate::copilot::read(caches, cfg),
        antigravity: crate::antigravity::read(caches, cfg),
        notion: crate::notion::read(caches, cfg, notion_shown),
        perplexity: crate::perplexity::read(caches, cfg, perplexity_shown),
        devin: crate::devin::read(caches, cfg, devin_shown),
        droid: crate::droid::read(caches, cfg, droid_shown),
        installed,
        fetched: 0.0,
        err: String::new(),
    };
    link_spacexsi(&mut state);
    state
}

/// Whether two weekly windows are the same window, to the millisecond.
///
/// Both vendors state the pool's window to the millisecond - Cursor as
/// epoch milliseconds, Grok as an ISO stamp with a fraction - and on the
/// measured account the two were identical. Two separate accounts sharing
/// a window that precisely is not a coincidence worth planning for, which
/// is what makes this the test for one account seen from two products.
fn same_window(a: Option<(f64, f64)>, b: Option<(f64, f64)>) -> bool {
    match (a, b) {
        (Some((a0, a1)), Some((b0, b1))) => a0.round() == b0.round() && a1.round() == b1.round(),
        _ => false,
    }
}

/// Where Cursor and Grok each stand relative to the unified plan, from
/// what each one says on its own and whether their windows match.
///
/// Cursor's plan says outright whether it is unified. Grok's says so only
/// on the credits endpoint, never on the log - so a Grok reading whose
/// window is a unified Cursor's, to the millisecond, is that account seen
/// from Grok, whatever markers it lacks. A Grok that is unified on its own
/// markers with a window of its own is a different account, and both keep
/// sections of their own.
///
/// Pure, so the whole matrix is testable without either vendor's private
/// fields.
fn place_spacexsi(
    cursor_unified: bool,
    grok_unified: bool,
    cursor_window: Option<(f64, f64)>,
    grok_window: Option<(f64, f64)>,
    last: Option<(Spacexsi, Spacexsi)>,
) -> (Spacexsi, Spacexsi) {
    let same = cursor_unified && same_window(cursor_window, grok_window);
    // Both unified but a window missing - a dropped request on either
    // side - is evidence of neither one account nor two. The relationship
    // last seen holds until the windows can be compared again; with none
    // seen, nothing is merged, because merging two accounts hides one of
    // them, where keeping one account apart for a pass only draws it twice.
    let unknown = cursor_unified && grok_unified && (cursor_window.is_none() || grok_window.is_none());
    if unknown {
        return match last {
            Some(seen @ (c, g)) if c.unified() && g.unified() => seen,
            _ => (Spacexsi::Separate, Spacexsi::Separate),
        };
    }
    match (cursor_unified, grok_unified || same) {
        (true, true) if same => (Spacexsi::Shared, Spacexsi::Shared),
        (true, true) => (Spacexsi::Separate, Spacexsi::Separate),
        (true, false) => (Spacexsi::Alone, Spacexsi::No),
        (false, true) => (Spacexsi::No, Spacexsi::Alone),
        (false, false) => (Spacexsi::No, Spacexsi::No),
    }
}

/// Tell each vendor where it stands, once both have been read.
/// The relationship the last poll settled on, for a pass that cannot
/// compare the two windows - with the pair of accounts it was settled for,
/// so a sign-in as someone else on either side never inherits it.
static LAST_PLACED: std::sync::Mutex<Option<(String, (Spacexsi, Spacexsi))>> = std::sync::Mutex::new(None);

/// The remembered relationship, only for the same pair of accounts.
fn remembered(last: Option<&(String, (Spacexsi, Spacexsi))>, who: &str) -> Option<(Spacexsi, Spacexsi)> {
    last.filter(|(pair, _)| pair == who).map(|(_, placed)| *placed)
}

fn link_spacexsi(s: &mut State) {
    let who = format!(
        "{}|{}",
        crate::cursor::account_key(),
        crate::grok::account_id().unwrap_or_default()
    );
    let mut last = LAST_PLACED.lock().unwrap_or_else(|e| e.into_inner());
    let seen = remembered(last.as_ref(), &who);
    let (cursor, grok) = place_spacexsi(
        crate::cursor::unified(&s.cursor),
        crate::grok::unified(&s.grok),
        crate::cursor::pool_window(&s.cursor),
        crate::grok::pool_window(&s.grok),
        seen,
    );
    *last = Some((who, (cursor, grok)));
    s.cursor.spacexsi = cursor;
    s.grok.spacexsi = grok;
    // The pool's split by product is Grok's to state; on the same account
    // it is the same pool Cursor's section draws.
    // Only a current reading: Grok's split is drawn under Cursor's `live`
    // heading, and a share from a request that has since failed, or a log
    // line, would read as live there when it is not.
    let current = cursor == Spacexsi::Shared && crate::grok::reading_is_current(&s.grok);
    s.cursor.pool_split = if current { crate::grok::pool_split(&s.grok) } else { Vec::new() };
    // Grok's figure for the pool, on the same terms, for a Cursor response
    // that carries none of its own.
    s.cursor.pool_from_grok = if current { pool_lane(&crate::grok::lanes(&s.grok)).map(|l| l.pct) } else { None };
}

/// The weekly pool among a vendor's lanes, if it published one.
fn pool_lane(lanes: &[Lane]) -> Option<&Lane> {
    lanes.iter().find(|l| l.label == crate::cursor::POOL_LABEL)
}

/// The tab strip with the unified plan folded in, on the same rules as the
/// summary.
///
/// Same account: CURSOR and GROK become one SPACEXSI tab, at Cursor's place
/// in the strip. One vendor unified on its own: its tab is the SPACEXSI
/// tab, and the other keeps its own. Two unified accounts: a SPACEXSI tab
/// for each, named for the product that reported it. Neither: the strip is
/// untouched.
///
/// Built from the strip discovery and config already decided on, so a
/// vendor the reader hid stays hidden and nothing appears that had no tab.
pub fn spacexsi_tabs(tabs: &[String], s: &State) -> Vec<String> {
    let has = |n: &str| tabs.iter().any(|t| t == n);
    let rename = |name: &str, place: Spacexsi| -> Option<String> {
        match place {
            Spacexsi::No => None,
            Spacexsi::Separate => Some(format!("spacexsi:{name}")),
            Spacexsi::Alone | Spacexsi::Shared => Some("spacexsi".to_string()),
        }
    };
    let merged = has("cursor") && has("grok") && s.cursor.spacexsi == Spacexsi::Shared;
    let mut out: Vec<String> = Vec::new();
    for t in tabs {
        let next = match t.as_str() {
            "grok" if merged => continue,
            "cursor" => rename("cursor", s.cursor.spacexsi),
            "grok" => rename("grok", s.grok.spacexsi),
            _ => None,
        }
        .unwrap_or_else(|| t.clone());
        if !out.contains(&next) {
            out.push(next);
        }
    }
    out
}

/// Where a selection lands when the strip changes under it: the same tab
/// if it is still there, otherwise the tab its vendor folded into or out
/// of. None when nothing corresponds, and the position stands.
pub fn retarget_tab(was: &str, tabs: &[String]) -> Option<usize> {
    let at = |name: &str| tabs.iter().position(|t| t == name);
    if let Some(i) = at(was) {
        return Some(i);
    }
    match was {
        "cursor" | "grok" => at(&format!("spacexsi:{was}")).or_else(|| at("spacexsi")),
        "spacexsi" => at("cursor").or_else(|| at("grok")),
        other => other.strip_prefix("spacexsi:").and_then(|v| at(v).or_else(|| at("spacexsi"))),
    }
}

/// The merged SpaceXSI tab: the shared pool, the credits and the limit
/// once, then what each product recorded, as one report.
///
/// No vendor headings: the plan is one product now. Cloud figures only -
/// the pool, extra usage, activity, spend, metered and the plan - since
/// SpaceXSI is an account, and this machine's tracking database and Grok
/// session files are one machine's share of it. Cursor's reading of the
/// pool leads because it is the live one - its usage call is asked every
/// two minutes, where Grok's may be a log line.
fn spacexsi_tab_with_plots(s: &State, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let mut rows: Vec<String> = Vec::new();
    if crate::cursor::has_live(&s.cursor) {
        tc::append_plot_rows(&mut rows, plots, |local| {
            crate::cursor::spacexsi_block_with_plots(&s.cursor, w, p, local)
        });
    } else {
        // Cursor's usage call did not answer, and Grok holds a reading of
        // the same pool: that is drawn, as the summary already does, with
        // Cursor's reason beside it - not Cursor's empty block alone.
        let mut grok = s.grok.clone();
        grok.spacexsi = Spacexsi::Alone;
        tc::append_plot_rows(&mut rows, plots, |local| {
            crate::grok::tab_with_plots(&grok, w, 0, &Config::default(), p, local)
        });
        rows.extend(no_local(&crate::cursor::why_no_lane(&s.cursor), "", w, p));
    }
    while rows.last().is_some_and(|r| r.is_empty()) {
        rows.pop();
    }
    rows.push(String::new());
    tc::append_plot_rows(&mut rows, plots, |local| crate::cursor::details_with_plots(&s.cursor, w, p, local));
    rows
}

/// The summary heading a vendor's lanes go under: its own name on its own
/// plan, SpaceXSI on the unified one, and SpaceXSI naming the product when
/// two unified accounts each need a section.
fn spacexsi_heading(name: &str, place: Spacexsi) -> String {
    match place {
        Spacexsi::No => name.to_uppercase(),
        Spacexsi::Separate => {
            let product = if name == "cursor" { "Cursor" } else { "Grok" };
            format!("{} · via {}", SPACEXSI, product)
        }
        Spacexsi::Alone | Spacexsi::Shared => SPACEXSI.to_string(),
    }
}

fn claude_of<'a>(s: &'a State, tab: &str) -> Option<&'a crate::claude::Data> {
    if let Some(label) = tab.strip_prefix("claude:") {
        s.claude.iter().find(|p| p.label == label)
    } else if let Some(default) = s.claude.iter().find(|p| p.label.is_empty()) {
        // The default `~/.claude` keeps the bare `claude` tab even when it
        // is not first on the list. `first()` would then hand that tab the
        // custom account sitting above it, and the default would be
        // unreachable from the heading that still names it.
        Some(default)
    } else {
        s.claude.first()
    }
}

/// Every quota an agent publishes, in the one shape they can be compared in.
fn lanes_of(name: &str, s: &State) -> Vec<Lane> {
    match agent_family(name) {
        "claude" => claude_of(s, name)
            .map(crate::claude::lanes)
            .unwrap_or_default(),
        "codex" => crate::codex::lanes(&s.codex),
        "cursor" => crate::cursor::lanes(&s.cursor),
        "grok" => crate::grok::lanes(&s.grok),
        "copilot" => crate::copilot::lanes(&s.copilot),
        "antigravity" => crate::antigravity::lanes(&s.antigravity),
        "notion" => crate::notion::lanes(&s.notion),
        "perplexity" => crate::perplexity::lanes(&s.perplexity),
        "devin" => crate::devin::lanes(&s.devin),
        "droid" => crate::droid::lanes(&s.droid),
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
/// The agent a summary heading belongs to. Extra Claude profiles are
/// `{label} - CLAUDE` rather than CLAUDE, and still use Claude's hue and
/// nest order.
fn group_agent(heading: &str) -> String {
    if heading == "CLAUDE" || heading.ends_with(" - CLAUDE") {
        "claude".into()
    } else if heading.starts_with(SPACEXSI) {
        "spacexsi".into()
    } else {
        heading.to_lowercase()
    }
}

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
    let note = match agent_family(name) {
        "claude" => claude_of(s, name)
            .map(crate::claude::why_no_lane)
            .unwrap_or_else(|| crate::claude::why_no_lane(&crate::claude::Data::default())),
        "codex" => crate::codex::why_no_lane(&s.codex),
        "cursor" => crate::cursor::why_no_lane(&s.cursor),
        "grok" => crate::grok::why_no_lane(&s.grok),
        "copilot" => crate::copilot::why_no_lane(&s.copilot),
        "antigravity" => crate::antigravity::why_no_lane(&s.antigravity),
        "notion" => crate::notion::why_no_lane(&s.notion),
        "perplexity" => crate::perplexity::why_no_lane(&s.perplexity),
        "devin" => crate::devin::why_no_lane(&s.devin),
        "droid" => crate::droid::why_no_lane(&s.droid),
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

fn quiet_from(quiet: &[(String, String)], s: &State, w: usize, p: &Palette) -> Vec<String> {
    let said: Vec<(String, String, bool)> = quiet
        .iter()
        .map(|(heading, id)| {
            let (note, warn) = quiet_of(id, s);
            (heading.clone(), note, warn)
        })
        .collect();
    quiet_block(&said, s, w, p)
}

/// The reset line under an account on `[+]`, when the inventory was read and the
/// count is greater than zero.
///
/// Indented under the account rows, the same inset a collapsed provider uses
/// for a note under its summary, with one empty row above it so the count
/// is not read as another quota line. Zero and an unread inventory add
/// nothing, including that empty row. The count and `available` stay. When
/// the timestamp does not fit beside them it moves to the next lines,
/// broken between the datetime's parts. A word or a part wider than the
/// pane is broken by display width, so it is not skipped and a clock is
/// not shortened into a different time.
fn push_reset_summary(rows: &mut Vec<String>, heading: &str, s: &State, w: usize, p: &Palette) {
    let line = if heading == "CODEX" {
        s.codex.reset_summary_line()
    } else {
        s.claude.iter()
            .find(|profile| crate::claude::summary_heading(&profile.label, s.claude.len() > 1) == heading)
            .and_then(crate::claude::Data::reset_summary_line)
    };
    let Some(line) = line else {
        return;
    };
    rows.push(String::new());
    crate::codex::append_reset_summary(&mut *rows, &line, w.saturating_sub(1), p.txt.as_str());
}

/// Split out from summary_tab because the State it needs cannot be built
/// from another module - every agent's Data keeps its fields private - so
/// this is the only shape the ordering is testable in.
fn quiet_block(said: &[(String, String, bool)], s: &State, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = Vec::new();
    let mut unexplained: Vec<&str> = Vec::new();
    for (name, note, warn) in said {
        if note.is_empty() {
            unexplained.push(name);
            continue;
        }
        rows.push(tc::seg(&[(p.lbl.as_str(), format!("  {}", name))], w - 1));
        let tone = if *warn {
            p.warn.as_str()
        } else {
            p.dim.as_str()
        };
        rows.extend(
            wrap_text(note, w.saturating_sub(5).max(20))
                .into_iter()
                .map(|l| tc::seg(&[(tone, format!("   {}", l))], w - 1)),
        );
        push_reset_summary(&mut rows, name, s, w, p);
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
fn summary_with_plots(s: &State, w: usize, p: &Palette, names: &[&str], plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let mut groups: Vec<(String, Vec<Lane>)> = Vec::new();
    let mut quiet: Vec<(String, String)> = Vec::new();
    for &name in names {
        if name == "claude" {
            let multi = s.claude.len() > 1;
            if s.claude.is_empty() {
                quiet.push(("CLAUDE".into(), "claude".into()));
                continue;
            }
            for profile in &s.claude {
                let heading = crate::claude::summary_heading(&profile.label, multi);
                let id = crate::claude::tab_id(
                    &crate::claude::ClaudeDir {
                        path: profile.dir.clone(),
                        label: profile.label.clone(),
                    },
                    multi,
                );
                let got = crate::claude::lanes(profile);
                if got.is_empty() {
                    quiet.push((heading, id));
                } else {
                    groups.push((heading, got));
                }
            }
            continue;
        }
        // Shared, the pool, the credits and the limit are one allowance,
        // and Cursor's reading of it is the live one. Grok's lanes are then
        // the same pool a second time, so Grok adds a section only when
        // Cursor has nothing to say for it - and when Cursor has the money
        // but no pool figure, Grok's pool joins Cursor's group below.
        let place = match name {
            "cursor" => s.cursor.spacexsi,
            "grok" => s.grok.spacexsi,
            _ => Spacexsi::No,
        };
        // Both quiet is still one account: Cursor's entry says why, once.
        if name == "grok"
            && place == Spacexsi::Shared
            && names.contains(&"cursor")
            && (!lanes_of("cursor", s).is_empty() || lanes_of("grok", s).is_empty())
        {
            continue;
        }
        // The mirror of that: Cursor quiet and Grok carrying the shared
        // pool. One SpaceXSI group, from Grok, with Cursor's reason under
        // it - not a second SpaceXSI heading saying the account has no quota.
        if name == "cursor"
            && place == Spacexsi::Shared
            && names.contains(&"grok")
            && lanes_of("cursor", s).is_empty()
            && !lanes_of("grok", s).is_empty()
        {
            continue;
        }
        let heading = spacexsi_heading(name, place);
        let mut got = lanes_of(name, s);
        if name == "cursor"
            && place == Spacexsi::Shared
            && names.contains(&"grok")
            && !got.is_empty()
            && pool_lane(&got).is_none()
        {
            got.extend(pool_lane(&lanes_of("grok", s)).cloned());
        }
        if got.is_empty() {
            quiet.push((heading, name.to_string()));
        } else {
            groups.push((heading, got));
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
    // 3 lead + label + 1 + pct(6) + pace(7: "  -100%", the widest it
    // draws). The pace was counted as five, which only ever fitted because
    // the labels left a spare cell - a fifteen-cell label took it and cut
    // `-30%` to `-30`.
    let fixed = 17 + label_w + usize::from(any_stale) * 3;
    // The countdown at its widest is `  resetting` or `  ~29d 23h`: eleven.
    // Sixteen was reserved for it, which dropped it on panes it fitted.
    let show_reset = (w - 1).saturating_sub(fixed + 8) >= 11;
    let tail = if show_reset { 11 } else { 0 };
    // A short bar is still a bar; the figures after it are the reading, and
    // an eight-cell floor pushed them off the edge of a narrow pane.
    let bar_room = (w - 1).saturating_sub(fixed + tail).max(3);
    for (i, (name, lanes)) in groups.iter().enumerate() {
        if i > 0 {
            rows.push(String::new());
        }
        let hue = agent_hue(&group_agent(name));
        rows.push(tc::seg(
            &[(
                &hue.map(|(r, g, b)| tc::rgb(r, g, b))
                    .unwrap_or_else(|| p.txt.clone()),
                format!("  {}", name),
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
        // Devin's daily sits inside its weekly, and Droid's five-hour window
        // sits inside weekly inside monthly. Percentage order would reshuffle
        // that on every refresh and disagree with the agent's own tab.
        // Perplexity's pools are listed in the order they are spent.
        if !matches!(
            group_agent(name).as_str(),
            "claude" | "cursor" | "devin" | "droid" | "spacexsi" | "antigravity" | "perplexity"
        ) {
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
                        if lane.stale {
                            p.warn.clone()
                        } else {
                            p.dim.clone()
                        },
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
            let guessed = pace_guessed(lane.stale, lane.projected, lane.reset);
            let cushion = lead(lane.pct, lane.window_secs, lane.reset);
            let (pace_colour, pace_txt) = pace_cell(cushion, guessed, p);
            let mut line: Vec<(String, String)> = vec![(
                p.dim.clone(),
                format!("   {} ", tc::pad(&lane.label, label_w)),
            )];
            line.extend(paced_bar_with_plot(plots, rows.len(), &line, used,
                elapsed_of(lane.window_secs, lane.reset), bar_room, hue, p));
            if any_stale {
                // Held for every lane, so the percentages stay in one
                // column whether the row beside them is cached or not.
                line.push((
                    p.warn.clone(),
                    if lane.stale {
                        " *".into()
                    } else {
                        "  ".to_string()
                    },
                ));
            }
            line.push((
                pct_colour(lane.pct, hue, p),
                pct_text(lane.pct),
            ));
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
        if group_agent(name) == "spacexsi"
            && s.cursor.spacexsi == Spacexsi::Shared
            && lanes_of("cursor", s).is_empty()
        {
            let why = crate::cursor::why_no_lane(&s.cursor);
            rows.extend(tc::wrap_words(&format!("Cursor {why}"), w.saturating_sub(6).max(1)).into_iter().map(|line| {
                tc::seg(&[(p.dim.as_str(), format!("     {line}"))], w.saturating_sub(1).max(1))
            }));
        }
        // The same note for a SpaceXSI section Grok is the one reading -
        // including a shared one whose pool came from Grok, because Cursor
        // is hidden or sent no pool figure of its own.
        let grok_supplies_shared = s.cursor.spacexsi == Spacexsi::Shared
            && names.contains(&"grok")
            && (!names.contains(&"cursor") || pool_lane(&lanes_of("cursor", s)).is_none());
        let grok_reads = group_agent(name) == "grok"
            || (group_agent(name) == "spacexsi"
                && s.grok.spacexsi.unified()
                && (s.cursor.spacexsi == Spacexsi::No || name.ends_with("via Grok") || grok_supplies_shared));
        if grok_reads && crate::grok::asks_nobody(&s.grok) {
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
            let said = tc::missing_config("Set agent_usage.grok_ping to poll x.ai instead.");
            rows.extend(
                tc::wrap_words(&said, w.saturating_sub(6).max(1))
                    .into_iter()
                    .map(|line| {
                        tc::seg(
                            &[(p.dim.as_str(), format!("     {line}"))],
                            w.saturating_sub(1).max(1),
                        )
                    }),
            );
        }
        push_reset_summary(&mut rows, name, s, w, p);
        // Notion's endpoint is unsupported and can answer with less than a
        // whole reading; the lanes above are then a subset, said here so
        // they are not read as all of it.
        if group_agent(name) == "notion" {
            if let Some(gaps) = crate::notion::partial(&s.notion) {
                let said = format!("partial · {} · its tab says what that costs", gaps);
                rows.extend(
                    tc::wrap_words(&said, w.saturating_sub(6).max(1))
                        .into_iter()
                        .map(|line| {
                            tc::seg(
                                &[(p.warn.as_str(), format!("     {line}"))],
                                w.saturating_sub(1).max(1),
                            )
                        }),
                );
            }
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

/// Agents read with a token the reader pastes, which discovery cannot find
/// on this machine, with the setting that takes it.
const PASTED_TOKEN_AGENTS: [(&str, &str); 2] = [("notion", "Notion AI"), ("perplexity", "Perplexity")];

/// A line under the overview naming the agents discovery cannot find: the
/// ones signed in with a pasted token that has not been pasted. Without it
/// a reader on discovery has no way to learn they could be here at all.
/// Nothing when the reader named the agents themselves, since that list is
/// already their choice, and nothing for one they excluded.
fn token_agents_hint(cfg: &Config, shown: &[&str], w: usize, p: &Palette) -> Vec<String> {
    if !discovers(cfg) {
        return Vec::new();
    }
    let missing: Vec<&str> = PASTED_TOKEN_AGENTS
        .iter()
        .filter(|(id, _)| !shown.contains(id) && !cfg.exclude_agents.iter().any(|x| x == id))
        .map(|(_, name)| *name)
        .collect();
    if missing.is_empty() {
        return Vec::new();
    }
    let text = format!(
        "{} {} read with a session token you paste: press , to add one.",
        missing.join(" and "),
        if missing.len() == 1 { "is" } else { "are" },
    );
    let content_width = w.saturating_sub(3);
    let mut rows = vec![String::new()];
    rows.extend(
        tc::wrap_words(&text, content_width)
            .into_iter()
            .map(|line| tc::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1))),
    );
    rows
}

pub fn tab_body_with_plots(name: &str, s: &State, w: usize, h: usize, cfg: &Config, p: &Palette, tabs: &[String], plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let listed = |vendor: &str| tabs.iter().any(|t| t == vendor);
    match name {
        SUMMARY_TAB => {
            let shown: Vec<&str> = ORDER
                .iter()
                .copied()
                .filter(|n| {
                    tabs.iter()
                        .any(|t| t == *n || t.starts_with(&format!("{n}:")))
                })
                .collect();
            // No agent tab means discovery has not found one, or the reader
            // excluded every one it found. Walking ORDER here would name
            // agents that have no tab, which is the same claim a tab would
            // have made.
            let mut rows = summary_with_plots(s, w, p, &shown, plots);
            rows.extend(token_agents_hint(cfg, &shown, w, p));
            rows
        }
        name if agent_family(name) == "claude" => {
            let fallback = crate::claude::Data::default();
            crate::claude::tab_with_plots(claude_of(s, name).unwrap_or(&fallback), w, h, cfg, p, plots)
        }
        "codex" => crate::codex::tab_with_plots(&s.codex, w, h, cfg, p, plots),
        // The merged body only when the reader kept both vendors; a vendor
        // they hid stays hidden, and the tab is then the visible one's.
        "spacexsi" if s.cursor.spacexsi == Spacexsi::Shared && listed("cursor") && listed("grok") => {
            spacexsi_tab_with_plots(s, w, p, plots)
        }
        "spacexsi" | "spacexsi:cursor" if s.cursor.spacexsi.unified() && listed("cursor") => {
            // Grok hidden: nothing of Grok's on the tab either - neither
            // its product split nor its figure for the pool.
            if listed("grok") {
                crate::cursor::tab_with_plots(&s.cursor, w, h, cfg, p, plots)
            } else {
                let mut cursor = s.cursor.clone();
                cursor.pool_split.clear();
                cursor.pool_from_grok = None;
                crate::cursor::tab_with_plots(&cursor, w, h, cfg, p, plots)
            }
        }
        "spacexsi" | "spacexsi:grok" => {
            // With Cursor hidden there is no CURSOR tab to point at, so the
            // shared reading is drawn as Grok's own.
            if s.grok.spacexsi == Spacexsi::Shared && !listed("cursor") {
                let mut grok = s.grok.clone();
                grok.spacexsi = Spacexsi::Alone;
                crate::grok::tab_with_plots(&grok, w, h, cfg, p, plots)
            } else {
                crate::grok::tab_with_plots(&s.grok, w, h, cfg, p, plots)
            }
        }
        "cursor" => crate::cursor::tab_with_plots(&s.cursor, w, h, cfg, p, plots),
        "grok" => crate::grok::tab_with_plots(&s.grok, w, h, cfg, p, plots),
        "copilot" => crate::copilot::tab_with_plots(&s.copilot, w, h, cfg, p, plots),
        "antigravity" => crate::antigravity::tab_with_plots(&s.antigravity, w, h, cfg, p, plots),
        "notion" => crate::notion::tab_with_plots(&s.notion, w, h, cfg, p, plots),
        "perplexity" => crate::perplexity::tab_with_plots(&s.perplexity, w, h, cfg, p, plots),
        "devin" => crate::devin::tab_with_plots(&s.devin, w, h, cfg, p, plots),
        "droid" => crate::droid::tab_with_plots(&s.droid, w, h, cfg, p, plots),
        other => unknown(other, &s.installed, w, p),
    }
}

/// A backstop for an agent added to the list without a reader: it says so
/// rather than raising in the draw loop.
fn unknown(
    name: &str,
    installed: &HashMap<String, Presence>,
    w: usize,
    p: &Palette,
) -> Vec<String> {
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
    fn the_overview_names_the_pasted_token_agents_discovery_cannot_find() {
        let p = palette();
        let text = |cfg: &Config, shown: &[&str], w: usize| {
            token_agents_hint(cfg, shown, w, &p).iter().map(|r| plain(&[r.clone()]).join("")).collect::<Vec<_>>().join(" ")
        };
        let found = Config::default();
        let both = text(&found, &["claude"], 60);
        assert!(both.contains("Notion AI and Perplexity are read with a session token"), "{both}");
        assert!(both.contains("press , to add one"), "{both}");
        // One already has its tab: only the other is named.
        let one = text(&found, &["claude", "notion"], 60);
        assert!(one.contains("Perplexity is read with") && !one.contains("Notion"), "{one}");
        // Both have tabs, or the reader named their agents: nothing.
        assert!(token_agents_hint(&found, &["notion", "perplexity"], 60, &p).is_empty());
        let named = Config { agents: vec!["claude".into()], ..Config::default() };
        assert!(token_agents_hint(&named, &["claude"], 60, &p).is_empty());
        // An excluded agent is not offered back.
        let excluded = Config { exclude_agents: vec!["notion".into()], ..Config::default() };
        let rest = text(&excluded, &["claude"], 60);
        assert!(!rest.contains("Notion") && rest.contains("Perplexity"), "{rest}");
        let expected = "Notion AI and Perplexity are read with a session token you paste: press , to add one.";
        let compact = |s: &str| s.chars().filter(|c| !c.is_whitespace()).collect::<String>();
        for w in [8usize, 9, 10, 11, 12, 30, 40, 60, 100] {
            let rows = token_agents_hint(&found, &[], w, &p);
            for r in &rows {
                assert!(tc::display_width(&plain(&[r.clone()]).join("")) <= w - 1, "width {w}: {r:?}");
            }
            assert_eq!(compact(&plain(&rows).join(" ")), compact(expected), "width {w}: {rows:?}");
        }
    }

    #[test]
    fn an_agent_this_build_no_longer_knows_is_ignored() {
        // A config from before CodeRabbit was dropped still names it; it is
        // an unknown agent now, like `foo`, so no tab and no panic - in
        // either list.
        let cfg = Config {
            agents: vec!["coderabbit".into(), "claude".into()],
            exclude_agents: vec!["coderabbit".into()],
            auto_detect_agent: Some(false),
            ..Config::default()
        };
        let visible = visible_agents(&HashMap::new(), &cfg);
        assert!(visible.iter().any(|t| t == "claude"), "{visible:?}");
        assert!(!visible.iter().any(|t| t == "coderabbit"), "{visible:?}");
    }

    /// The summary follows the tabs. With only `[+]` up — discovery has
    /// not found an agent yet — naming every provider would claim they
    /// are here.
    #[test]
    fn the_summary_does_not_name_agents_with_no_tab() {
        let p = palette();
        let rows = plain(&tab_body(
            "+",
            &State::default(),
            90,
            40,
            &Config::default(),
            &p,
            &["+".into()],
        ));
        let joined = rows.join("\n");
        assert!(
            joined.contains("No agent is publishing a quota"),
            "{joined}"
        );
        for name in ["CLAUDE", "CODEX", "CURSOR", "GROK", "COPILOT", "NOTION", "DROID"] {
            assert!(
                !joined.contains(name),
                "{name} was named with no tab:\n{joined}"
            );
        }
    }

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
            "ANTIGRAVITY".into(),
            "no quota - it publishes none to any server.".to_string(),
            true,
        )];
        let rows = plain(&quiet_block(&said, &State::default(), 90, &p));
        let head = rows
            .iter()
            .position(|r| r.contains("ANTIGRAVITY"))
            .expect("a heading");
        let line = rows
            .iter()
            .position(|r| r.contains("publishes none"))
            .expect("the reason");
        assert!(
            head < line,
            "the sentence came before the name it is about:\n{:#?}",
            rows
        );

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
            (
                "ANTIGRAVITY".into(),
                "no quota - the app is closed.".to_string(),
                true,
            ),
            ("COPILOT".into(), String::new(), false),
        ];
        let rows = plain(&quiet_block(&said, &State::default(), 90, &p));
        let roll = rows
            .iter()
            .find(|r| r.contains("No quota published by"))
            .expect("a roll-call for the one with no reason");
        assert!(roll.contains("COPILOT"), "{}", roll);
        assert!(
            !roll.contains("antigravity"),
            "explained and listed: {}",
            roll
        );
        // The explained one still leads with its heading.
        let head = rows.iter().position(|r| r.contains("ANTIGRAVITY")).unwrap();
        let rollat = rows
            .iter()
            .position(|r| r.contains("No quota published by"))
            .unwrap();
        assert!(
            head < rollat,
            "roll-call above the explanations:\n{:#?}",
            rows
        );
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
        for name in [
            "CLAUDE",
            "CODEX",
            "CURSOR",
            "GROK",
            "COPILOT",
            "ANTIGRAVITY",
        ] {
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
            !rows
                .iter()
                .any(|r| r.contains("No agent is publishing a quota")),
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
        assert!(quiet_is_actionable(&tc::missing_config(
            "no quota · asking x.ai is off (set agent_usage.grok_ping to poll)"
        )));
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

    #[test]
    fn two_claude_profiles_are_separate_summary_groups() {
        let p = palette();
        let s = State {
            claude: vec![
                crate::claude::Data::with_session_quota("main", 25),
                crate::claude::Data::with_session_quota("overflow", 80),
            ],
            ..State::default()
        };
        let rows = plain(&summary_for(&s, 90, &p, &["claude", "cursor"]));
        let joined = rows.join("\n");
        assert!(joined.contains("main - CLAUDE"), "{joined}");
        assert!(joined.contains("overflow - CLAUDE"), "{joined}");
        // One CLAUDE heading would be the old merge. The word still appears
        // in each group title, so look for a lone CLAUDE line.
        assert!(
            !rows.iter().any(|r| r.trim() == "CLAUDE"),
            "two accounts shared one heading:\n{joined}"
        );
        assert!(joined.contains("CURSOR"), "{joined}");
        // Ranked as extra entries: overflow at 80% sits above main at 25%.
        let overflow = rows
            .iter()
            .position(|r| r.contains("overflow - CLAUDE"))
            .unwrap();
        let main = rows
            .iter()
            .position(|r| r.contains("main - CLAUDE"))
            .unwrap();
        assert!(overflow < main, "profiles were not ranked apart:\n{joined}");
    }

    #[test]
    fn one_claude_profile_keeps_the_claude_heading() {
        let p = palette();
        let s = State {
            claude: vec![crate::claude::Data::with_session_quota("main", 25)],
            ..State::default()
        };
        let rows = plain(&summary_for(&s, 90, &p, &["claude"]));
        let joined = rows.join("\n");
        assert!(joined.contains("CLAUDE"), "{joined}");
        assert!(!joined.contains("main - CLAUDE"), "{joined}");
    }

    #[test]
    fn overview_and_provider_plots_keep_the_pace_marker_background() {
        let state = State {
            notion: crate::notion::Data::answered(&serde_json::json!({
                "status": "within_limit",
                "billingPeriodWindow": { "used": 50, "limit": 100, "periodEndMs": (now() + 86400.0) * 1000.0 }
            }).to_string()),
            ..State::default()
        };
        for width in [30, 60, 120] {
            let p = palette();
            for overview in [true, false] {
                let render = |palette: &Palette| {
                    let mut plots = Vec::new();
                    if overview {
                        summary_with_plots(&state, width, palette, &["notion"], &mut plots);
                    } else {
                        crate::notion::tab_with_plots(&state.notion, width, 40, &Config::default(), palette, &mut plots);
                    }
                    plots
                };
                let plots = render(&p);
                assert!(!plots.is_empty());
                // Plot's cells are private to core. Inspect the recorded style,
                // not whole-plot inequality: time advances between two renders
                // and can move the reference even when its style is unchanged.
                assert!(format!("{plots:?}").contains(&format!("{:?}", p.pace_mark)),
                    "marker background lost: width={width}, overview={overview}");
            }
        }
    }

    #[test]
    fn vendor_detail_meters_cover_only_quota_cells() {
        let state = State {
            notion: crate::notion::Data::answered(
                r#"{"status":"within_limit","billingPeriodWindow":{"used":18,"limit":100}}"#,
            ),
            ..State::default()
        };
        for width in [40, 60, 120] {
            let mut plots = Vec::new();
            let rows = plain(&crate::notion::tab_with_plots(
                &state.notion, width, 40, &Config::default(), &palette(), &mut plots,
            ));
            assert_eq!(plots.len(), 1);
            for plot in plots {
                assert_eq!(plot.height, 1);
                assert!(plot.x > 0);
                for glyph in rows[plot.y].chars().skip(plot.x).take(plot.width) {
                    assert!(['█', '░', '┃'].contains(&glyph), "covered label: {glyph}");
                }
            }
        }
    }

    #[test]
    fn summary_meter_rectangles_cover_only_the_bar() {
        let state = State {
            notion: crate::notion::Data::answered(
                r#"{"status":"within_limit","billingPeriodWindow":{"used":18,"limit":100}}"#,
            ),
            ..State::default()
        };
        for width in [30, 60, 120] {
            let mut plots = Vec::new();
            let rows = plain(&summary_with_plots(&state, width, &palette(), &["notion"], &mut plots));
            assert_eq!(plots.len(), 1);
            let plot = &plots[0];
            assert_eq!(plot.height, 1);
            let glyphs: Vec<_> = rows[plot.y].chars().collect();
            for glyph in glyphs.iter().skip(plot.x).take(plot.width) {
                assert!(['█', '░', '┃'].contains(glyph), "plot would cover a label: {glyph}");
            }
            assert!(rows[plot.y].contains("18%"));
        }
    }

    #[test]
    fn a_partial_notion_reading_is_marked_on_the_summary() {
        // The summary draws the lanes it is given, so one surviving window
        // looked like the whole Notion reading there; only the tab said it
        // was not.
        let p = palette();
        let part = State {
            notion: crate::notion::Data::answered(
                r#"{"status":"within_limit",
                    "billingPeriodWindow":{"used":18,"limit":100}}"#,
            ),
            ..State::default()
        };
        for w in [30usize, 60, 120] {
            let rows = plain(&summary_for(&part, w, &p, &["notion"]));
            for r in &rows {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            let words = rows.join(" ").split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("partial · no rolling window"), "{words}");
            assert!(words.contains("no billing-period end"), "{words}");
        }
        let whole = State {
            notion: crate::notion::Data::answered(
                r#"{"status":"within_limit",
                    "window":{"window":"6h","used":42,"limit":100},"resetsInSeconds":600,
                    "billingPeriodWindow":{"used":18,"limit":100,"periodEndMs":1788000000000}}"#,
            ),
            ..State::default()
        };
        let rows = plain(&summary_for(&whole, 120, &p, &["notion"]));
        assert!(rows.iter().any(|r| r.contains("NOTION")), "{rows:?}");
        assert!(!rows.iter().any(|r| r.contains("partial")), "{rows:?}");
        // An end date past any month we can measure is a length we do not have.
        let unread = State {
            notion: crate::notion::Data::answered(
                r#"{"status":"within_limit",
                    "window":{"window":"6h","used":42,"limit":100},"resetsInSeconds":600,
                    "billingPeriodWindow":{"used":18,"limit":100,"periodEndMs":1e20}}"#,
            ),
            ..State::default()
        };
        let words = plain(&summary_for(&unread, 120, &p, &["notion"]))
            .join(" ")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(words.contains("no billing-period length"), "{words}");
    }

    #[test]
    fn claude_reset_summaries_stay_with_their_accounts_with_or_without_lanes() {
        let grants = |left| serde_json::json!([{
            "resets_left":left,"usable_now":true,"ends_at":"2126-01-02T00:00:00Z"
        }]);
        for pct in [Some(25), None] {
            let s = State {
                claude: vec![
                    crate::claude::Data::with_reset_grants("", pct, grants(2), true),
                    crate::claude::Data::with_reset_grants("work", Some(80), grants(3), false),
                ],
                ..Default::default()
            };
            let rows = plain(&summary_for(&s, 120, &palette(), &["claude"]));
            let mut heading = "";
            let mut counts = 0;
            for (i, row) in rows.iter().enumerate() {
                if row.trim() == "CLAUDE" || row.trim() == "work - CLAUDE" {
                    heading = row.trim();
                }
                if row.contains("reset available") {
                    counts += 1;
                    assert!(rows[i - 1].is_empty(), "{rows:?}");
                    let (count, cached) = if heading == "CLAUDE" {
                        (2, false)
                    } else {
                        (3, false)
                    };
                    assert!(
                        row.starts_with(&format!("     {count} reset available ")),
                        "{rows:?}"
                    );
                    assert!(
                        !row.contains(['(', ')']) && !row.contains("2126"),
                        "{rows:?}"
                    );
                    assert_eq!(row.contains("cached"), cached, "{rows:?}");
                }
            }
            assert_eq!(counts, 2, "{rows:?}");
        }
    }

    #[test]
    fn claude_reset_summaries_wrap_and_omit_empty_or_unread_banks() {
        let excluded = State {
            claude: vec![crate::claude::Data::with_reset_grants(
                "work",
                Some(25),
                serde_json::json!([
                    {"resets_left":2,"usable_now":true,"ends_at":"2126-01-02T00:00:00Z"}
                ]),
                true,
            )],
            ..Default::default()
        };
        for live in [true, false] {
            let s = State {
                claude: vec![crate::claude::Data::with_reset_grants(
                    "work",
                    Some(25),
                    serde_json::json!([
                        {"resets_left":2,"usable_now":true,"ends_at":"2126-01-02T00:00:00Z"}
                    ]),
                    live,
                )],
                ..Default::default()
            };
            let line = s.claude[0].reset_summary_line().unwrap();
            for width in 20..=120 {
                let mut rows = Vec::new();
                push_reset_summary(&mut rows, "CLAUDE", &s, width, &palette());
                let bare = plain(&rows);
                let joined = bare.iter().map(|r| r.trim()).collect::<Vec<_>>().join(" ");
                assert_eq!(joined.trim(), line, "width {width}");
                assert!(
                    bare.iter().all(|r| tc::display_width(r) < width),
                    "{bare:?}"
                );
            }
        }
        for grants in [serde_json::json!([]), serde_json::Value::Null, serde_json::json!([
            {"resets_left":3,"usable_now":true,"ends_at":"2000-01-01T00:00:00Z"}
        ])] {
            let s = State {
                claude: vec![crate::claude::Data::with_reset_grants("", Some(25), grants, true)],
                ..Default::default()
            };
            let mut rows = Vec::new();
            push_reset_summary(&mut rows, "CLAUDE", &s, 120, &palette());
            assert!(rows.is_empty(), "no empty separator either: {rows:?}");
        }
        let rows = plain(&summary_for(&excluded, 120, &palette(), &["codex"]));
        assert!(
            !rows.iter().any(|r| r.contains("reset available")),
            "excluded Claude leaked: {rows:?}"
        );
    }

    #[test]
    fn the_codex_summary_keeps_its_title_and_states_the_resets_under_it() {
        let p = palette();
        let soon = crate::iso_epoch("2026-10-04T11:03:00Z").expect("soon");
        let stamp = crate::codex::summary_expiry(soon).expect("a local expiry");
        let with = State {
            codex: crate::codex::Data::with_reset(Some(29.0), Some((2, Some(soon)))),
            ..State::default()
        };
        let rows = plain(&summary_for(&with, 120, &p, &["codex"]));
        let head = rows
            .iter()
            .find(|r| r.contains("CODEX"))
            .expect("a Codex line");
        assert_eq!(head, "  CODEX", "the title gained a count: {head}");
        let line = format!("2 reset available {stamp}");
        let at = rows
            .iter()
            .position(|r| r.contains(&line))
            .expect("the reset line");
        assert_eq!(rows[at], format!("     {line}"));
        assert!(
            rows[at - 1].is_empty(),
            "the reset line lost the blank line above it: {rows:#?}"
        );
        assert!(
            rows[at - 2].contains('%'),
            "the reset line does not follow a Codex summary row: {rows:#?}"
        );
        assert!(at > rows.iter().position(|r| r.contains("CODEX")).unwrap());

        let one = State {
            codex: crate::codex::Data::with_reset(Some(29.0), Some((1, Some(soon)))),
            ..State::default()
        };
        let one_line = crate::codex::Data::with_reset(Some(29.0), Some((1, Some(soon))))
            .reset_summary_line()
            .expect("one credit");
        assert_eq!(one_line, format!("1 reset available {stamp}"));
        assert!(
            plain(&summary_for(&one, 120, &p, &["codex"]))
                .iter()
                .any(|r| r == &format!("     {one_line}")),
            "one credit was not worded the same way"
        );

        let unread = State {
            codex: crate::codex::Data::with_window_and_bank(Some(29.0), None),
            ..State::default()
        };
        let unread = plain(&summary_for(&unread, 120, &p, &["codex"]));
        assert!(
            !unread.iter().any(|r| r.contains("reset available")),
            "a count was drawn without an inventory:\n{unread:#?}"
        );
        assert!(
            !unread
                .windows(2)
                .any(|pair| pair[0].is_empty() && pair[1].is_empty()),
            "an unread inventory added a blank line:\n{unread:#?}"
        );

        let zero = State {
            codex: crate::codex::Data::with_window_and_bank(Some(29.0), Some(0)),
            ..State::default()
        };
        let zero = plain(&summary_for(&zero, 120, &p, &["codex"]));
        let head = zero
            .iter()
            .find(|r| r.contains("CODEX"))
            .expect("a Codex line");
        assert_eq!(head, "  CODEX");
        assert!(
            !zero.iter().any(|r| r.contains("reset available")),
            "a zero bank drew a reset line:\n{zero:#?}"
        );
        assert_eq!(
            zero, unread,
            "a zero bank changed the summary an unread inventory draws:\n{zero:#?}\n{unread:#?}"
        );

        // No quota lanes, so Codex is the quiet heading. The title stays
        // the name, and the reset line follows it.
        let quiet = State {
            codex: crate::codex::Data::with_reset(None, Some((2, Some(soon)))),
            ..State::default()
        };
        let quiet = plain(&summary_for(&quiet, 120, &p, &["codex"]));
        let head = quiet
            .iter()
            .find(|r| r.contains("CODEX"))
            .expect("a quiet Codex line");
        assert_eq!(head, "  CODEX", "{head}");
        let at = quiet
            .iter()
            .position(|r| r.contains(&line))
            .expect("the reset line");
        assert_eq!(quiet[at], format!("     {line}"));
        assert!(
            quiet[at - 1].is_empty(),
            "the quiet reset lost the blank line above it: {quiet:#?}"
        );
        assert!(
            quiet[at - 2].contains("no quota"),
            "the quiet reset does not follow the Codex note: {quiet:#?}"
        );
        let quiet_zero = plain(&summary_for(
            &State {
                codex: crate::codex::Data::with_window_and_bank(None, Some(0)),
                ..State::default()
            },
            120,
            &p,
            &["codex"],
        ));
        let quiet_unread = plain(&summary_for(
            &State {
                codex: crate::codex::Data::with_reset(None, None),
                ..State::default()
            },
            120,
            &p,
            &["codex"],
        ));
        assert_eq!(
            quiet_zero, quiet_unread,
            "a quiet zero bank added a blank the unread inventory does not:\n{quiet_zero:#?}\n{quiet_unread:#?}"
        );

        // Narrower than the datetime: the count and `available` stay, the
        // timestamp moves down, and the clock token stays whole.
        // Width 23 leaves room for the five-space inset plus
        // `2 reset available`, and not for the datetime beside them.
        let clock = stamp
            .split_whitespace()
            .find(|word| word.contains(':'))
            .expect("the clock");
        let wanted: String = format!("2 reset available {stamp}")
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let tight = plain(&summary_for(&with, 23, &p, &["codex"]));
        assert!(
            tight.iter().any(|row| row == "     2 reset available"),
            "the count and available were split while they fit: {tight:#?}"
        );
        assert!(
            !tight
                .iter()
                .any(|row| row.contains("reset available") && row.contains(clock)),
            "the timestamp stayed on the count line: {tight:#?}"
        );
        assert!(
            tight
                .iter()
                .any(|row| row.starts_with("     ") && row.contains(clock)),
            "the clock left the Codex inset: {tight:#?}"
        );
        let flat: String = tight
            .iter()
            .flat_map(|row| row.trim().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            flat.contains(&wanted),
            "a token was skipped\n{wanted}\n{flat}"
        );

        // Width 11 leaves five columns beside the inset, which is the
        // clock and nothing wider.
        let squeezed = plain(&summary_for(&with, 11, &p, &["codex"]));
        let flat: String = squeezed
            .iter()
            .flat_map(|row| row.trim().chars())
            .filter(|c| !c.is_whitespace())
            .collect();
        assert!(
            flat.contains(&wanted),
            "a narrow pane dropped the label or the zone\n{wanted}\n{flat}\n{squeezed:#?}"
        );
        assert!(
            squeezed
                .iter()
                .any(|row| row.starts_with("     ") && row.contains(clock)),
            "the clock left the Codex inset: {squeezed:#?}"
        );
    }

    #[test]
    fn the_bare_claude_tab_is_the_default_account_even_when_it_is_not_first() {
        let s = State {
            claude: vec![
                crate::claude::Data::with_session_quota("work", 80),
                crate::claude::Data::with_session_quota("", 25),
            ],
            ..State::default()
        };
        let default = claude_of(&s, "claude").expect("the CLAUDE tab");
        assert_eq!(default.label, "", "{:?}", default.label);
        assert_eq!(crate::claude::lanes(default)[0].pct, 25.0);
        let extra = claude_of(&s, "claude:work").expect("the work tab");
        assert_eq!(extra.label, "work");
        assert_eq!(crate::claude::lanes(extra)[0].pct, 80.0);

        // One custom directory still uses the bare tab, and still has a
        // label of its own — first() is then the only profile there is.
        let one = State {
            claude: vec![crate::claude::Data::with_session_quota("work", 40)],
            ..State::default()
        };
        assert_eq!(claude_of(&one, "claude").expect("lone tab").label, "work");
    }

    #[test]
    fn spacexsi_placement_covers_every_pairing() {
        let w = Some((1_791_859_163_429.0, 1_792_463_963_429.0));
        let other = Some((1_791_600_000_000.0, 1_792_204_800_000.0));
        use Spacexsi::*;
        // (cursor unified, grok unified, cursor window, grok window) -> placement
        let cases = [
            ((true, true, w, w), (Shared, Shared)),
            // Grok's log carries no marker; the window is what joins it.
            ((true, false, w, w), (Shared, Shared)),
            ((true, true, w, other), (Separate, Separate)),
            // A missing window with nothing seen before: never merged.
            ((true, true, w, None), (Separate, Separate)),
            ((true, true, None, w), (Separate, Separate)),
            ((true, false, w, other), (Alone, No)),
            ((true, false, w, None), (Alone, No)),
            ((false, true, None, w), (No, Alone)),
            // A Cursor on its own plan never pulls Grok in by window alone.
            ((false, false, w, w), (No, No)),
            ((false, false, None, None), (No, No)),
        ];
        for ((cu, gu, cw, gw), want) in cases {
            assert_eq!(place_spacexsi(cu, gu, cw, gw, None), want, "{cu} {gu} {cw:?} {gw:?}");
        }
        // Milliseconds, not seconds: a window a second off is a different one.
        let nudged = Some((1_791_859_164_429.0, 1_792_463_964_429.0));
        assert_eq!(place_spacexsi(true, false, w, nudged, None), (Alone, No));
        // A missing window holds the relationship last seen.
        assert_eq!(place_spacexsi(true, true, w, None, Some((Shared, Shared))), (Shared, Shared));
        assert_eq!(place_spacexsi(true, true, None, w, Some((Separate, Separate))), (Separate, Separate));
        // And with none seen, two accounts are never merged on a guess.
        assert_eq!(place_spacexsi(true, true, w, None, Some((Alone, No))), (Separate, Separate));
    }

    fn summary_text(s: &State) -> String {
        plain(&summary_for(s, 120, &palette(), &["cursor", "grok"])).join("\n")
    }

    fn headings(text: &str) -> Vec<String> {
        // A group heading is two spaces and then the name, nothing else on
        // the row.
        text.lines()
            .filter(|l| l.starts_with("  ") && !l.starts_with("   ") && !l.starts_with("  *"))
            .filter(|l| !l.trim().is_empty())
            .map(|l| l.trim().to_string())
            .collect()
    }

    #[test]
    fn the_summary_puts_a_shared_unified_plan_under_one_spacexsi_heading() {
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let text = summary_text(&s);
        assert_eq!(headings(&text), ["SpaceXSI"], "{text}");
        assert_eq!(text.matches("weekly pool").count(), 1, "the pool counted twice: {text}");
        assert!(!text.contains("CURSOR") && !text.contains("GROK"), "{text}");
    }

    #[test]
    fn one_unified_vendor_goes_under_spacexsi_and_the_other_keeps_its_own_heading() {
        // Cursor unified, Grok on its own plan.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::own_plan_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Alone;
        let text = summary_text(&s);
        let mut got = headings(&text);
        got.sort();
        assert_eq!(got, ["GROK", "SpaceXSI"], "{text}");
        assert!(text.contains("credits"), "Grok's own plan lost its lane: {text}");
        // Grok unified, Cursor on its own plan: three lanes under CURSOR.
        let mut s = State {
            cursor: crate::cursor::Data::own_plan_fixture(),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.grok.spacexsi = Spacexsi::Alone;
        let text = summary_text(&s);
        let mut got = headings(&text);
        got.sort();
        assert_eq!(got, ["CURSOR", "SpaceXSI"], "{text}");
        assert!(text.contains("cursor models") && text.contains("other models"), "{text}");
    }

    #[test]
    fn two_unified_accounts_each_keep_a_section_and_neither_is_merged() {
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Separate;
        s.grok.spacexsi = Spacexsi::Separate;
        let text = summary_text(&s);
        let mut got = headings(&text);
        got.sort();
        assert_eq!(got, ["SpaceXSI · via Cursor", "SpaceXSI · via Grok"], "{text}");
    }

    #[test]
    fn neither_unified_draws_no_spacexsi_heading() {
        let s = State {
            cursor: crate::cursor::Data::own_plan_fixture(),
            grok: crate::grok::Data::own_plan_fixture(),
            ..State::default()
        };
        let text = summary_text(&s);
        let mut got = headings(&text);
        got.sort();
        assert_eq!(got, ["CURSOR", "GROK"], "{text}");
        assert!(!text.contains("SpaceXSI"), "{text}");
    }

    fn strip_of(cursor: Spacexsi, grok: Spacexsi, tabs: &[&str]) -> Vec<String> {
        let mut s = State::default();
        s.cursor.spacexsi = cursor;
        s.grok.spacexsi = grok;
        let tabs: Vec<String> = tabs.iter().map(|t| t.to_string()).collect();
        spacexsi_tabs(&tabs, &s)
    }

    #[test]
    fn the_tab_strip_folds_the_unified_plan_on_the_summarys_rules() {
        use Spacexsi::*;
        let all = ["+", "claude", "cursor", "grok", "copilot"];
        assert_eq!(strip_of(Shared, Shared, &all), ["+", "claude", "spacexsi", "copilot"]);
        assert_eq!(strip_of(Alone, No, &all), ["+", "claude", "spacexsi", "grok", "copilot"]);
        assert_eq!(strip_of(No, Alone, &all), ["+", "claude", "cursor", "spacexsi", "copilot"]);
        assert_eq!(
            strip_of(Separate, Separate, &all),
            ["+", "claude", "spacexsi:cursor", "spacexsi:grok", "copilot"]
        );
        assert_eq!(strip_of(No, No, &all), all);
        // A vendor the reader hid stays hidden: shared, but only Grok has a
        // tab, so Grok's tab is the SpaceXSI one.
        assert_eq!(strip_of(Shared, Shared, &["+", "grok"]), ["+", "spacexsi"]);
        // Review: and the body of each single-vendor strip draws only the
        // vendor that is listed.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Shared;
        s.grok.spacexsi = Shared;
        for (listed, own, other) in [(["+", "cursor"], "EXTRA USAGE", "of the weekly pool"), (["+", "grok"], "of the weekly pool", "EXTRA USAGE")] {
            let tabs: Vec<String> = listed.iter().map(|t| t.to_string()).collect();
            let folded = spacexsi_tabs(&tabs, &s);
            assert_eq!(folded, ["+", "spacexsi"]);
            let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
            assert!(body.contains(own), "{listed:?} lost its own body: {body}");
            assert!(!body.contains(other), "{listed:?} drew the hidden vendor: {body}");
        }
    }

    #[test]
    fn the_merged_tab_draws_the_shared_block_once_then_each_product() {
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        s.cursor.pool_split = crate::grok::pool_split(&s.grok);
        let tabs: Vec<String> = ["+", "cursor", "grok"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert_eq!(body.matches("── SpaceXSI ──").count(), 1, "{body}");
        assert_eq!(body.matches("── EXTRA USAGE ──").count(), 1, "{body}");
        assert_eq!(body.matches("left of $20.00 paid").count(), 1, "the balance stated twice: {body}");
        // One report, cloud figures only: no vendor headings, and nothing
        // read from this machine - no Grok session totals or second
        // heatmap, no Cursor authorship counts.
        assert!(!body.contains('▍'), "a vendor heading on the merged tab: {body}");
        for local in ["GROK BUILD", "TOTALS", "TOKENS / DAY", "AI-WRITTEN CODE", "WHO WROTE IT", "BY MODEL", "Grok sessions"] {
            assert!(!body.contains(local), "local data {local:?} on the SpaceXSI tab: {body}");
        }
        // Grok's part keeps what only Grok says about the pool, and does not
        // draw the pool a second time.
        // The split sits under the pool's bar, named, once.
        // Each product on a line of its own under the pool bar, once.
        let flat: String = body.split(' ').filter(|w| !w.is_empty()).collect::<Vec<_>>().join(" ");
        assert_eq!(flat.matches("■ Cursor ~3%").count(), 1, "{body}");
        assert_eq!(flat.matches("■ Grok Bot ~2%").count(), 1, "{body}");
        assert!(body.find("■ Cursor").unwrap() < body.find("EXTRA USAGE").unwrap(), "{body}");
        assert!(!body.contains("of the weekly pool"), "Grok's pool bar drawn again: {body}");
        // The summary still sees Cursor and Grok, though the strip folded them.
        let summary = plain(&tab_body(SUMMARY_TAB, &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(summary.contains("SpaceXSI"), "{summary}");
        // Percentages only on [+]: the dollars, the refill rule and the
        // top-ups are the detailed tab's.
        assert!(summary.contains("prepaid credits") && summary.contains("usage limit"), "{summary}");
        for gone in ["$20", "$100", "auto top-up", "top-ups", "payment method", "by product"] {
            assert!(!summary.contains(gone), "{gone:?} on [+]: {summary}");
        }
        for w in [30usize, 58, 120] {
            for row in plain(&tab_body("spacexsi", &s, w, 40, &Config::default(), &palette(), &tabs)) {
                assert!(row.chars().count() <= w - 1, "{} wide at {w}: {row:?}", row.chars().count());
            }
        }
    }

    #[test]
    fn no_summary_row_cuts_its_pace_or_countdown() {
        // The pace figure was budgeted at five cells and draws up to seven,
        // so the widest label left it one short and `-30%` became `-30`.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::own_plan_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Alone;
        for w in [40usize, 50, 57, 58, 64, 80, 120] {
            for row in plain(&summary_for(&s, w, &palette(), &["cursor", "grok"])) {
                let n = row.chars().count();
                assert!(n <= w - 1, "{n} wide at {w}: {row:?}");
                // A lane row ends in its percentage, its pace, or its
                // countdown - never in a figure cut short.
                if row.starts_with("   ") && row.contains('%') {
                    let end = row.trim_end();
                    assert!(
                        end.ends_with('%') || end.ends_with('h') || end.ends_with('m') || end.ends_with("resetting"),
                        "a cut row at {w}: {row:?}"
                    );
                }
            }
        }
        // And at the width these panes sit at, the countdown still fits.
        let rows = plain(&summary_for(&s, 57, &palette(), &["cursor", "grok"])).join("\n");
        assert!(rows.lines().any(|l| l.contains("weekly pool") && l.trim_end().ends_with('h')), "{rows}");
    }

    #[test]
    fn a_hidden_vendor_stays_hidden_on_the_spacexsi_tab() {
        // Review: with one vendor excluded, the folded tab still drew the
        // merged body - and with it the hidden vendor.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let body = |listed: &[&str]| {
            let tabs: Vec<String> = listed.iter().map(|t| t.to_string()).collect();
            plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n")
        };
        // Grok hidden: Cursor's own tab, no Grok-only product split.
        let cursor_only = body(&["+", "cursor"]);
        assert!(cursor_only.contains("SpaceXSI") && cursor_only.contains("EXTRA USAGE"), "{cursor_only}");
        // Cursor hidden: Grok's own tab, none of Cursor's spend or plan.
        let grok_only = body(&["+", "grok"]);
        assert!(grok_only.contains("SpaceXSI"), "{grok_only}");
        for cursor_part in ["SPEND", "EXTRA USAGE", "ACTIVITY", "METERED"] {
            assert!(!grok_only.contains(cursor_part), "hidden Cursor's {cursor_part:?}: {grok_only}");
        }
    }

    #[test]
    fn a_shared_pool_read_from_grok_s_log_says_it_is_not_live() {
        // Review: with Cursor quiet or hidden, the SpaceXSI group's pool is
        // Grok's, and with nothing polling x.ai it is a log figure - shown
        // without the note saying only local sessions move it.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::polling_nobody(crate::grok::Data::unified_fixture()),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let text = |s: &State, names: &[&str]| plain(&summary_for(s, 120, &palette(), names)).join("\n");
        // Cursor's live pool leads: no note.
        assert!(!text(&s, &["cursor", "grok"]).contains("not live"));
        // Cursor hidden: the pool is Grok's.
        assert!(text(&s, &["grok"]).contains("not live"));
        // Cursor answered without a pool figure: the pool is Grok's.
        s.cursor = crate::cursor::Data::without_pool_figure(s.cursor.clone());
        assert!(text(&s, &["cursor", "grok"]).contains("not live"));
        // Cursor quiet altogether: the pool is Grok's.
        s.cursor = crate::cursor::Data::without_live(s.cursor.clone());
        assert!(text(&s, &["cursor", "grok"]).contains("not live"));
    }

    #[test]
    fn a_cursor_tab_with_grok_hidden_shows_nothing_of_grok_s() {
        // Review: with Grok excluded, the folded tab still drew Grok's
        // product rows and Grok's figure for the pool, copied over by
        // link_spacexsi before the tab knew Grok was hidden.
        let mut s = State {
            cursor: crate::cursor::Data::without_pool_figure(crate::cursor::Data::unified_fixture(true)),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        s.cursor.pool_split = vec![("Cursor".into(), 3.0), ("Grok Bot".into(), 2.0)];
        s.cursor.pool_from_grok = Some(5.0);
        let tabs: Vec<String> = ["+", "cursor"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(!body.contains("Grok Bot"), "hidden Grok's product split: {body}");
        assert!(!body.contains("from Grok"), "hidden Grok's pool figure: {body}");
        assert!(body.contains("no pool figure"), "{body}");
    }

    #[test]
    fn a_remembered_relationship_belongs_to_the_accounts_it_was_seen_for() {
        // Review: after signing in as someone else on either side, the
        // remembered Shared could fold two unrelated accounts together.
        let last = ("cursor-a|grok-a".to_string(), (Spacexsi::Shared, Spacexsi::Shared));
        assert_eq!(remembered(Some(&last), "cursor-a|grok-a"), Some((Spacexsi::Shared, Spacexsi::Shared)));
        assert_eq!(remembered(Some(&last), "cursor-b|grok-a"), None);
        assert_eq!(remembered(Some(&last), "cursor-a|grok-b"), None);
        assert_eq!(remembered(None, "cursor-a|grok-a"), None);
    }

    #[test]
    fn a_selection_follows_its_vendor_when_the_strip_folds() {
        // Review: folding [+, cursor, grok, copilot] into [+, spacexsi,
        // copilot] moved a selection on GROK to COPILOT.
        let strip = |t: &[&str]| t.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        let folded = strip(&["+", "spacexsi", "copilot"]);
        assert_eq!(retarget_tab("grok", &folded), Some(1));
        assert_eq!(retarget_tab("cursor", &folded), Some(1));
        assert_eq!(retarget_tab("copilot", &folded), Some(2));
        let unfolded = strip(&["+", "cursor", "grok", "copilot"]);
        assert_eq!(retarget_tab("spacexsi", &unfolded), Some(1));
        let separate = strip(&["+", "spacexsi:cursor", "spacexsi:grok"]);
        assert_eq!(retarget_tab("grok", &separate), Some(2));
        assert_eq!(retarget_tab("spacexsi:grok", &unfolded), Some(2));
        assert_eq!(retarget_tab("devin", &folded), None);
    }

    #[test]
    fn with_cursor_quiet_the_shared_pool_comes_from_grok_once() {
        // Review: Cursor's usage call failing left the merged tab with only
        // Cursor's empty block, and [+] with a second, quota-less SpaceXSI.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor = crate::cursor::Data::without_live(s.cursor.clone());
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let tabs: Vec<String> = ["+", "cursor", "grok"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(body.contains("of the weekly pool"), "Grok's pool not drawn: {body}");
        assert!(!body.contains("CURSOR tab"), "pointing at a tab with nothing on it: {body}");
        let summary = summary_text(&s);
        assert_eq!(summary.matches("SpaceXSI").count(), 1, "two SpaceXSI groups: {summary}");
        assert!(summary.contains("Cursor no quota"), "Cursor's reason lost: {summary}");
    }

    #[test]
    fn a_cursor_answer_without_a_pool_figure_takes_grok_s_beside_its_money() {
        // Review: Cursor answering with its money but no pool percentage
        // printed "no pool figure" on the tab, and [+] dropped Grok's pool
        // because Cursor had other lanes.
        let mut s = State {
            cursor: crate::cursor::Data::without_pool_figure(crate::cursor::Data::unified_fixture(true)),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let grok_pool = pool_lane(&crate::grok::lanes(&s.grok)).expect("Grok's fixture has a pool").pct;
        let summary = summary_text(&s);
        assert_eq!(summary.matches("SpaceXSI").count(), 1, "two SpaceXSI groups: {summary}");
        assert!(summary.contains("weekly pool"), "Grok's pool dropped: {summary}");
        assert!(summary.contains("usage limit"), "Cursor's money dropped: {summary}");
        s.cursor.pool_from_grok = Some(grok_pool);
        let tabs: Vec<String> = ["+", "cursor", "grok"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(!body.contains("no pool figure"), "Grok's figure not used: {body}");
        assert!(body.contains("pool figure from Grok"), "the figure's source unsaid: {body}");
        assert!(body.contains("EXTRA USAGE"), "Cursor's money lost: {body}");
    }

    #[test]
    fn a_cursor_pool_cached_from_an_ended_week_gives_way_to_grok_s() {
        // Review: a two-minute cache spanning the weekly reset kept last
        // week's Cursor pool on screen, labelled live, beside this week's
        // Grok split.
        let mut s = State {
            cursor: crate::cursor::Data::from_an_ended_week(crate::cursor::Data::unified_fixture(true)),
            grok: crate::grok::Data::unified_fixture(),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let grok_pool = pool_lane(&crate::grok::lanes(&s.grok)).expect("Grok's fixture has a pool").pct;
        let summary = summary_text(&s);
        assert_eq!(summary.matches("SpaceXSI").count(), 1, "two SpaceXSI groups: {summary}");
        assert!(summary.contains("weekly pool"), "Grok's pool dropped: {summary}");
        s.cursor.pool_from_grok = Some(grok_pool);
        let tabs: Vec<String> = ["+", "cursor", "grok"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(body.contains("pool figure from Grok"), "last week's Cursor figure still drawn: {body}");
    }

    #[test]
    fn a_shared_account_quiet_on_both_sides_is_listed_once() {
        // Review: with neither vendor publishing a lane, [+] listed the
        // same SpaceXSI account twice, each with its own reason.
        let mut s = State {
            cursor: crate::cursor::Data::without_live(crate::cursor::Data::unified_fixture(true)),
            ..State::default()
        };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let summary = summary_text(&s);
        assert_eq!(summary.matches("SpaceXSI").count(), 1, "the account listed twice: {summary}");
    }

    #[test]
    fn a_grok_tab_with_cursor_hidden_does_not_point_at_cursor() {
        // Review: with Cursor excluded, the shared Grok tab still said its
        // credits were on the CURSOR tab.
        let mut s = State { grok: crate::grok::Data::unified_fixture(), ..State::default() };
        s.cursor.spacexsi = Spacexsi::Shared;
        s.grok.spacexsi = Spacexsi::Shared;
        let tabs: Vec<String> = ["+", "grok"].iter().map(|t| t.to_string()).collect();
        let body = plain(&tab_body("spacexsi", &s, 120, 40, &Config::default(), &palette(), &tabs)).join("\n");
        assert!(!body.contains("CURSOR tab"), "{body}");
        assert!(body.contains("prepaid $20"), "Grok's own balance hidden: {body}");
    }

    #[test]
    fn linking_reads_both_vendors_and_tells_each_where_it_stands() {
        // The fixtures' windows are built from the clock a moment apart, so
        // this pins the decision on markers alone: both unified on their
        // own markers with windows that do not match are two accounts.
        let mut s = State {
            cursor: crate::cursor::Data::unified_fixture(true),
            grok: crate::grok::Data::own_plan_fixture(),
            ..State::default()
        };
        link_spacexsi(&mut s);
        assert_eq!((s.cursor.spacexsi, s.grok.spacexsi), (Spacexsi::Alone, Spacexsi::No));
        let mut s = State {
            cursor: crate::cursor::Data::own_plan_fixture(),
            grok: crate::grok::Data::own_plan_fixture(),
            ..State::default()
        };
        link_spacexsi(&mut s);
        assert_eq!((s.cursor.spacexsi, s.grok.spacexsi), (Spacexsi::No, Spacexsi::No));
    }

    /// The usage response as a Max account on an exhausted extra-usage
    /// balance sends it: every key and value type of the reset block as
    /// the live endpoint returned it, with made-up values. The grant
    /// carries far more than the parser reads - ids, labels, what it
    /// clears, per-window percentages - and the block carries cooldown and
    /// exhaustion fields beside `eligible`.
    fn max_account_quota() -> serde_json::Value {
        serde_json::json!({
            "limits": [
                {"group": "session", "is_active": true, "kind": "session", "percent": 12,
                 "resets_at": "2126-01-01T00:00:00Z", "scope": null, "severity": "normal"},
                {"group": "weekly", "is_active": true, "kind": "weekly_all", "percent": 40,
                 "resets_at": "2126-01-05T00:00:00Z", "scope": null, "severity": "normal"},
                {"group": "weekly", "is_active": false, "kind": "weekly_scoped", "percent": 3,
                 "resets_at": "2126-01-05T00:00:00Z", "scope": {"model": "fixture"}, "severity": "normal"}
            ],
            "extra_usage": {
                "credits_ever_enabled": true, "currency": "USD", "daily": null,
                "decimal_places": 2, "disabled_reason": "out_of_credits", "is_enabled": false,
                "monthly_limit": 5000, "spend_limit_reached": true, "used_credits": 5000.0,
                "user_disabled": false, "utilization": 100.0, "weekly": null
            },
            "cedar_ember": {
                "at_limit": false,
                "cooldown_until": null,
                "eligible": true,
                "event_props": {"billing_path": "x", "billing_period": "x", "extra_usage_state": "x",
                                "surface": "x", "tenure_bucket": "x", "tier": "x"},
                "exhausted": [],
                "grants": [{
                    "arm": null, "blocking": [], "clears": ["five_hour", "seven_day", "seven_day_opus"],
                    "ends_at": "2126-01-03T00:00:00Z", "gives_back_weekly": true,
                    "id": "fixture-grant", "label": "Fixture grant", "paused": false,
                    "percent_used": {"five_hour": 0, "seven_day": 0, "seven_day_overage_included": 0},
                    "resets_left": 1, "resets_total": 1, "starts_at": "2025-01-01T00:00:00Z",
                    "type": "full", "unavailable_reason": null, "usable_now": true,
                    "use_requires_limit": false
                }],
                "ineligible_reason": null,
                "next_grant_id": "fixture-next",
                "paused_held": false,
                "weekly_resets_at": "2126-01-05T00:00:00Z"
            }
        })
    }

    /// Both screens drawn through the functions the running widget calls -
    /// the tab dispatch for `claude` and for `[+]` - from one profile with
    /// a live-shaped reading, so the section and the line are compared as
    /// they appear, not as their builders return them.
    #[test]
    fn a_live_shaped_claude_reading_draws_the_bank_on_the_tab_and_the_line_on_plus() {
        let s = State {
            claude: vec![crate::claude::Data::with_quota("", max_account_quota())],
            ..Default::default()
        };
        let cfg = Config::default();
        let tabs = vec![SUMMARY_TAB.to_string(), "claude".to_string()];
        let tab = plain(&tab_body_with_plots("claude", &s, 100, 60, &cfg, &palette(), &tabs, &mut Vec::new()));
        let plus = plain(&tab_body_with_plots(SUMMARY_TAB, &s, 100, 60, &cfg, &palette(), &tabs, &mut Vec::new()));
        let stamp = crate::codex::local_expiry(iso_epoch("2126-01-03T00:00:00Z").unwrap()).unwrap();
        let at = tab.iter().position(|row| row.trim_end() == " ── 1 BANK RESET ──")
            .unwrap_or_else(|| panic!("no bank on the tab: {tab:#?}"));
        assert_eq!(tab[at + 1].trim_end(), format!("  Full reset · expires {stamp}"), "{tab:#?}");
        assert!(tab[at + 2].trim().is_empty(), "{tab:#?}");
        let line = s.claude[0].reset_summary_line().expect("the [+] line");
        assert!(plus.iter().any(|row| row.trim() == line), "{plus:#?}");
        let compact = crate::codex::summary_expiry(iso_epoch("2126-01-03T00:00:00Z").unwrap()).unwrap();
        assert_eq!(line, format!("1 reset available {compact}"));
        // The tab's date is the line's date with the year added.
        for part in compact.split_whitespace() {
            assert!(stamp.split_whitespace().any(|p| p == part), "{part} / {stamp}");
        }
    }
}

#[cfg(test)]
fn summary_for(s: &State, w: usize, p: &Palette, names: &[&str]) -> Vec<String> {
    summary_with_plots(s, w, p, names, &mut Vec::new())
}

#[cfg(test)]
pub fn tab_body(
    name: &str,
    s: &State,
    w: usize,
    h: usize,
    cfg: &Config,
    p: &Palette,
    tabs: &[String],
) -> Vec<String> {
    tab_body_with_plots(name, s, w, h, cfg, p, tabs, &mut Vec::new())
}
