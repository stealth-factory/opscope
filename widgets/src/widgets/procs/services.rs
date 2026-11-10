// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! The services view (`g`, Linux): one row per cgroup with `cpu` or
//! `memory` enabled, from two walks of the cgroup v2 tree.
//!
//! Attribution is core's [`cgroup::attribute`]: each cgroup gets its own
//! share, its nearest enabled descendants taken out, so nothing is counted
//! twice. %CPU is that share of `usage_usec` over the window between two
//! walks - the tree subtracted first, then attributed, never the other way
//! round - and MEMORY is the own share of `memory.current` now. A cgroup
//! that lacks both controllers rolls up into the nearest ancestor that has
//! one, and so do its processes.
//!
//! Everything here is pure: `linux.rs` walks the tree and hands the
//! [`State`] in, and tests build trees by hand.

use std::collections::{BTreeMap, HashMap};

use opscope_core::cgroup::{self, CpuStat, Reading, State, Tree};

/// One row: a cgroup with `cpu` or `memory` enabled.
#[derive(Clone, Debug, PartialEq)]
pub struct Service {
    /// Relative to the cgroup root, `""` for the root itself.
    pub path: String,
    /// Its own share of every core over the window, as a %. `None` before
    /// a second walk, without `cpu` here, or when the share is a gap.
    pub cpu: Option<f64>,
    /// Its own `usage_usec` over the window: what decides "empty".
    pub cpu_usec: Option<u64>,
    /// Its own share of `memory.current`. `None` without `memory` here (the
    /// root has no `memory.current`) or when the share is a gap.
    pub memory: Option<u64>,
    /// Read a moment after this cgroup, a descendant moved more than it did
    /// (or could not be read), so a share could not be taken: drawn as a gap.
    pub gap: bool,
    /// A cgroup under it with the controller could not be read, so that
    /// cgroup's usage could not be taken out and sits in this one's: the
    /// share is a measured total but not this cgroup's, so it is not drawn
    /// as one.
    pub unread: bool,
    /// Its processes, with those of any cgroup under it that rolls up.
    pub pids: Vec<i32>,
    /// Controllers enabled on it.
    pub enabled: Vec<String>,
    /// `cpu.stat` as read, which includes every cgroup under it.
    pub stat: Option<CpuStat>,
    /// The change in `stat` over the window.
    pub stat_delta: Option<CpuStat>,
    /// `memory.current` as read, which includes every cgroup under it.
    pub memory_total: Option<u64>,
}

impl Service {
    /// Nothing measured: no CPU over the window, no memory, no processes.
    /// A gap is not empty - its share could not be taken this poll, which
    /// is the opposite of having measured nothing - so it stays drawn. A
    /// share holding an unread child's usage is the same.
    pub fn empty(&self) -> bool {
        !self.gap
            && !self.unread
            && self.cpu_usec.unwrap_or(0) == 0
            && self.memory.unwrap_or(0) == 0
            && self.pids.is_empty()
    }

    /// The row's name: its path, `/` for the root. A delegated subtree
    /// lets a user name a cgroup anything, so the path is drawn as text
    /// with its escapes taken out; `path` itself stays exact, since it is
    /// what `memory.max` is read through.
    pub fn name(&self) -> String {
        if self.path.is_empty() {
            "/".to_string()
        } else {
            opscope_core::source_text(&self.path)
        }
    }
}

/// One walk's rows.
#[derive(Clone, Debug, PartialEq)]
pub struct Table {
    pub rows: Vec<Service>,
    /// Seconds between the two walks; `None` on the first.
    pub window: Option<f64>,
    /// How deep accounting goes, as core says it: `cpu`'s, and `memory`'s
    /// when it reads differently.
    pub depth: Vec<String>,
    /// The rows are this container's own cgroups, not the host's.
    pub container: bool,
    /// A controller enabled on nothing below the root, and the line that
    /// enables it; the other one's figures still draw.
    pub off: Option<(String, String)>,
}

/// What the view draws instead of an empty table.
#[derive(Clone, Debug, PartialEq)]
pub enum Services {
    Table(Table),
    /// cgroup v1 or hybrid.
    V1,
    /// Neither controller is enabled below the root: the line naming it.
    NotEnabled(String),
    Unreadable(String),
}

/// What `s` cycles through in this view.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Sort {
    Cpu,
    Memory,
    Procs,
}

impl Sort {
    pub fn next(self) -> Sort {
        match self {
            Sort::Cpu => Sort::Memory,
            Sort::Memory => Sort::Procs,
            Sort::Procs => Sort::Cpu,
        }
    }

    pub fn hint(self) -> String {
        let next = match self.next() {
            Sort::Cpu => "cpu",
            Sort::Memory => "memory",
            Sort::Procs => "procs",
        };
        format!("[s] sort by {next}")
    }

    pub fn in_force(self) -> &'static str {
        match self {
            Sort::Cpu => "sorted by %CPU",
            Sort::Memory => "sorted by memory",
            Sort::Procs => "sorted by processes",
        }
    }
}

/// The line for a controller that is off everywhere.
pub fn off_line(controller: &str, state: &State) -> String {
    let remedy = state
        .remedy()
        .unwrap_or_else(|| format!("systemctl set-property <unit> {controller} accounting"));
    format!("{controller} accounting is off everywhere · {remedy}")
}

/// `next - prev` for `cpu.stat`, per cgroup, as a tree `attribute` reads.
/// A cgroup new since the last walk started inside the window, so all of
/// its count is this window's; so is one whose counter went backwards (it
/// was removed and made again under the same name). One that was there
/// without a reading and has one now gets none: its count could be from
/// any time.
pub fn delta_tree(prev: &Tree, next: &Tree) -> Tree {
    let before: HashMap<&str, &cgroup::Node> =
        prev.nodes.iter().map(|n| (n.path.as_str(), n)).collect();
    let nodes = next
        .nodes
        .iter()
        .map(|node| {
            let cpu = match (before.get(node.path.as_str()), node.cpu) {
                (_, None) => None,
                (None, Some(now)) => Some(now),
                (Some(old), Some(now)) => match old.cpu {
                    None => None,
                    Some(then) if now.usage_usec >= then.usage_usec => Some(CpuStat {
                        usage_usec: now.usage_usec - then.usage_usec,
                        user_usec: now.user_usec.saturating_sub(then.user_usec),
                        system_usec: now.system_usec.saturating_sub(then.system_usec),
                        nr_throttled: now.nr_throttled.saturating_sub(then.nr_throttled),
                        throttled_usec: now.throttled_usec.saturating_sub(then.throttled_usec),
                    }),
                    Some(_) => Some(now),
                },
            };
            cgroup::Node {
                path: node.path.clone(),
                enabled: node.enabled.clone(),
                io: None,
                cpu,
                memory: None,
                procs: None,
            }
        })
        .collect();
    Tree { nodes }
}

fn parent(path: &str) -> Option<&str> {
    if path.is_empty() {
        None
    } else {
        Some(path.rsplit_once('/').map(|(p, _)| p).unwrap_or(""))
    }
}

/// The rows from one walk, against the previous walk `elapsed` seconds
/// before (`None` on the first). `cores` normalises %CPU to the machine.
pub fn rows(next: &Tree, prev: Option<(&Tree, f64)>, cores: usize) -> Vec<Service> {
    let delta = prev.map(|(prev, elapsed)| (delta_tree(prev, next), elapsed));
    // Each share with its two flags: (own, inconsistent, unread).
    let mut cpu: HashMap<String, (u64, bool, bool)> = HashMap::new();
    if let Some((delta, _)) = &delta {
        for row in cgroup::attribute(delta, "cpu") {
            if let Reading::Cpu(usec) = row.own {
                cpu.insert(row.path, (usec, row.inconsistent, row.unread));
            }
        }
    }
    let cpu_rows: Vec<String> = cgroup::attribute(next, "cpu")
        .into_iter()
        .map(|r| r.path)
        .collect();
    let mut memory: HashMap<String, (u64, bool, bool)> = HashMap::new();
    for row in cgroup::attribute(next, "memory") {
        if let Reading::Memory(bytes) = row.own {
            memory.insert(row.path, (bytes, row.inconsistent, row.unread));
        }
    }
    let deltas: HashMap<&str, CpuStat> = delta
        .as_ref()
        .map(|(tree, _)| {
            tree.nodes
                .iter()
                .filter_map(|n| Some((n.path.as_str(), n.cpu?)))
                .collect()
        })
        .unwrap_or_default();
    let is_row = |path: &str| cpu_rows.iter().any(|p| p == path) || memory.contains_key(path);
    // Every cgroup's processes land on itself when it is a row, otherwise on
    // the nearest ancestor that is.
    let mut pids: BTreeMap<String, Vec<i32>> = BTreeMap::new();
    for node in &next.nodes {
        let mut at = Some(node.path.as_str());
        while let Some(path) = at {
            if is_row(path) {
                pids.entry(path.to_string())
                    .or_default()
                    .extend(node.procs.iter().flatten().copied());
                break;
            }
            at = parent(path);
        }
    }
    let scale =
        |usec: u64, elapsed: f64| usec as f64 / (elapsed * 1e6 * cores.max(1) as f64) * 100.0;
    let elapsed = delta.as_ref().map(|(_, e)| *e).filter(|e| *e > 0.0);
    next.nodes
        .iter()
        .filter(|n| is_row(&n.path))
        .map(|node| {
            let own_cpu = cpu.get(&node.path).copied();
            let own_mem = memory.get(&node.path).copied();
            let gap =
                own_cpu.is_some_and(|(_, bad, _)| bad) || own_mem.is_some_and(|(_, bad, _)| bad);
            let unread = own_cpu.is_some_and(|(_, _, part)| part)
                || own_mem.is_some_and(|(_, _, part)| part);
            // A share is a figure only when nothing below it was short or
            // unread; otherwise it is drawn as `—` and said why.
            let figure = |(v, bad, part): (u64, bool, bool)| (!bad && !part).then_some(v);
            let cpu_usec = own_cpu.and_then(figure);
            let mut list = pids.remove(&node.path).unwrap_or_default();
            list.sort_unstable();
            list.dedup();
            Service {
                path: node.path.clone(),
                cpu: cpu_usec.zip(elapsed).map(|(usec, e)| scale(usec, e)),
                cpu_usec,
                memory: own_mem.and_then(figure),
                gap,
                unread,
                pids: list,
                enabled: node.enabled.clone(),
                stat: node.cpu,
                stat_delta: deltas.get(node.path.as_str()).copied(),
                memory_total: node.memory,
            }
        })
        .collect()
}

/// The depth sentences to draw: `cpu`'s, and `memory`'s when it says
/// something different about the tree.
pub fn depth(tree: &Tree, controllers: &[&str]) -> Vec<String> {
    let said: Vec<String> = controllers
        .iter()
        .map(|c| cgroup::depth_sentence(tree, c))
        .collect();
    let rest = |line: &str| line.split_once(' ').map(|(_, r)| r.to_string());
    let mut out: Vec<String> = Vec::new();
    for line in said {
        if !out.iter().any(|kept| rest(kept) == rest(&line)) {
            out.push(line);
        }
    }
    out
}

/// What the poller keeps between walks: the last tree and when.
#[derive(Default)]
pub struct Memo {
    prev: Option<(Tree, f64)>,
}

impl Memo {
    /// Nothing kept: the next walk measures afresh.
    pub fn forget(&mut self) {
        self.prev = None;
    }
}

/// One walk turned into what the view draws. `off` names a controller the
/// walk found enabled nowhere, when the other one was walked alone.
/// `now` is monotonic seconds.
pub fn step(
    memo: &mut Memo,
    state: State,
    off: Option<(String, String)>,
    now: f64,
    cores: usize,
) -> Services {
    let (tree, container) = match state {
        State::Ok(tree) => (tree, false),
        State::Container(tree) => (tree, true),
        State::V1 => {
            memo.forget();
            return Services::V1;
        }
        state @ State::NotEnabled(_) => {
            memo.forget();
            let State::NotEnabled(c) = &state else {
                unreachable!()
            };
            return Services::NotEnabled(off_line(c, &state));
        }
        State::Unreadable(why) => {
            memo.forget();
            return Services::Unreadable(why);
        }
    };
    let prev = memo
        .prev
        .as_ref()
        .filter(|(_, at)| now > *at)
        .map(|(tree, at)| (tree, now - at));
    let controllers: Vec<&str> = ["cpu", "memory"]
        .into_iter()
        .filter(|c| off.as_ref().is_none_or(|(o, _)| o != c))
        .collect();
    let table = Table {
        rows: rows(&tree, prev, cores),
        window: prev.map(|(_, e)| e),
        depth: depth(&tree, &controllers),
        container,
        off,
    };
    memo.prev = Some((tree, now));
    Services::Table(table)
}

/// Whether a row passes `/` (on its path) and `u` (only yours: under
/// `user.slice/user-<uid>.slice`).
pub fn passes(row: &Service, needle: &str, all_users: bool, me: u32) -> bool {
    if !all_users {
        let mine = format!("user.slice/user-{me}.slice");
        if row.path != mine && !row.path.starts_with(&format!("{mine}/")) {
            return false;
        }
    }
    needle.is_empty() || row.path.to_lowercase().contains(&needle.to_lowercase())
}

/// The rows shown, in order, how many the filters left before empties
/// were hidden, and how many of those were empty.
pub fn shown<'a>(
    rows: &'a [Service],
    needle: &str,
    all_users: bool,
    me: u32,
    by: Sort,
) -> (Vec<&'a Service>, usize) {
    let passing: Vec<&Service> = rows
        .iter()
        .filter(|r| passes(r, needle, all_users, me))
        .collect();
    let empty = passing.iter().filter(|r| r.empty()).count();
    let mut out: Vec<&Service> = passing.into_iter().filter(|r| !r.empty()).collect();
    let cpu = |s: &Service| s.cpu.unwrap_or(-1.0);
    let mem = |s: &Service| s.memory.unwrap_or(0);
    out.sort_by(|a, b| {
        let first = match by {
            Sort::Cpu => cpu(b).total_cmp(&cpu(a)).then(mem(b).cmp(&mem(a))),
            Sort::Memory => mem(b).cmp(&mem(a)),
            Sort::Procs => b.pids.len().cmp(&a.pids.len()),
        };
        first.then(a.path.cmp(&b.path))
    });
    (out, empty)
}

/// The filters in force, as `filter_row` draws them: the needle, `yours`,
/// and how many empty rows are hidden.
pub fn filters(needle: &str, all_users: bool, empty: usize) -> Vec<String> {
    let mut out = super::model::filters(needle, all_users);
    if empty > 0 {
        out.push(format!("{empty} empty hidden"));
    }
    out
}

#[cfg(test)]
pub mod fixtures {
    use super::*;
    use opscope_core::cgroup::Node;

    pub fn node(
        path: &str,
        enabled: &[&str],
        usage: Option<u64>,
        memory: Option<u64>,
        procs: &[i32],
    ) -> Node {
        Node {
            path: path.into(),
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            io: None,
            cpu: usage.map(|usage_usec| CpuStat {
                usage_usec,
                user_usec: usage_usec * 3 / 4,
                system_usec: usage_usec / 4,
                ..CpuStat::default()
            }),
            memory,
            procs: Some(procs.to_vec()),
        }
    }

    const G: u64 = 1 << 30;
    const M: u64 = 1 << 20;

    /// The mock's host at `usage` scale: cpu and memory down to services,
    /// user sessions rolling up to user.slice. `t` moves the CPU counters
    /// on: at `t = 1` against `t = 0`, two seconds apart on 8 cores,
    /// example-db used 31.2% of the machine and user.slice 88.4%.
    pub fn host(t: u64) -> Tree {
        let both = ["cpu", "memory", "pids"];
        // usec over 2 s on 8 cores: share% × 16e6 / 100.
        let db = 4_992_000 * t;
        let log = 64_000 * t;
        let user = 14_144_000 * t;
        let system = db + log + 1_000 * t;
        let root = system + user + 50_000 * t + 9;
        Tree {
            nodes: vec![
                node("", &["cpu", "io", "memory", "pids"], Some(root), None, &[2]),
                node("init.scope", &both, Some(0), Some(0), &[1]),
                node(
                    "system.slice",
                    &both,
                    Some(system),
                    Some(4 * G + 3_920 * M + 120 * M),
                    &[],
                ),
                node(
                    "system.slice/example-db.service",
                    &both,
                    Some(db),
                    Some(3_900 * M + 20 * M),
                    &(100..112).collect::<Vec<_>>(),
                ),
                node(
                    "system.slice/example-log.service",
                    &both,
                    Some(log),
                    Some(120 * M),
                    &[300],
                ),
                node(
                    "system.slice/example-idle.service",
                    &both,
                    Some(0),
                    Some(0),
                    &[],
                ),
                node("user.slice", &both, Some(user), Some(9_830 * M), &[]),
                node("user.slice/user-1000.slice", &[], None, None, &[]),
                node(
                    "user.slice/user-1000.slice/session-2.scope",
                    &[],
                    None,
                    None,
                    &(1000..1203).collect::<Vec<_>>(),
                ),
            ],
        }
    }

    pub fn table() -> Table {
        let mut memo = Memo::default();
        step(&mut memo, State::Ok(host(0)), None, 10.0, 8);
        match step(&mut memo, State::Ok(host(1)), None, 12.0, 8) {
            Services::Table(t) => t,
            other => panic!("{other:?}"),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::fixtures::*;
    use super::*;

    fn row<'a>(t: &'a Table, path: &str) -> &'a Service {
        t.rows.iter().find(|r| r.path == path).expect(path)
    }

    #[test]
    fn the_mock_rows_are_own_figures_and_nothing_counts_twice() {
        let t = table();
        assert_eq!(t.window, Some(2.0));
        let db = row(&t, "system.slice/example-db.service");
        assert!((db.cpu.unwrap() - 31.2).abs() < 1e-9, "{:?}", db.cpu);
        assert_eq!(db.memory, Some((3_900 + 20) << 20));
        assert_eq!(db.pids.len(), 12);
        let user = row(&t, "user.slice");
        assert!((user.cpu.unwrap() - 88.4).abs() < 1e-9, "{:?}", user.cpu);
        assert_eq!(user.pids.len(), 203, "sessions roll up into user.slice");
        let log = row(&t, "system.slice/example-log.service");
        assert!((log.cpu.unwrap() - 0.4).abs() < 1e-9);
        assert_eq!(log.memory, Some(120 << 20));
        // The parent's row excludes its enabled children: system.slice's
        // own memory is what its services do not account for.
        let slice = row(&t, "system.slice");
        assert_eq!(slice.memory, Some(4 << 30));
        assert_eq!(slice.cpu_usec, Some(1_000));
        // The root has cpu but no memory.current.
        let root = row(&t, "");
        assert_eq!(root.memory, None);
        assert_eq!(root.name(), "/");
        // Own shares of usage add back up to the root's delta.
        let total: u64 = t.rows.iter().filter_map(|r| r.cpu_usec).sum();
        assert_eq!(total, 4_992_000 + 64_000 + 1_000 + 14_144_000 + 50_000);
        assert!(t.rows.iter().all(|r| !r.gap));
        assert_eq!(
            t.depth,
            vec!["cpu accounting down to: services, scopes; user sessions roll up to user.slice"]
        );
    }

    #[test]
    fn the_first_walk_is_measuring_and_memory_is_already_there() {
        let mut memo = Memo::default();
        let Services::Table(t) = step(&mut memo, State::Ok(host(0)), None, 1.0, 8) else {
            panic!()
        };
        assert_eq!(t.window, None);
        let db = row(&t, "system.slice/example-db.service");
        assert_eq!(db.cpu, None);
        assert!(db.memory.is_some());
    }

    #[test]
    fn empty_rows_are_hidden_and_counted() {
        let t = table();
        let (shown, empty) = shown(&t.rows, "", true, 1000, Sort::Cpu);
        assert_eq!(empty, 1, "example-idle has nothing");
        assert_eq!(
            shown.iter().map(|r| r.path.as_str()).collect::<Vec<_>>(),
            vec![
                "user.slice",
                "system.slice/example-db.service",
                "system.slice/example-log.service",
                "",
                "system.slice",
                "init.scope",
            ]
        );
        assert_eq!(filters("", true, empty), vec!["1 empty hidden"]);
        let (by_mem, _) = super::shown(&t.rows, "", true, 1000, Sort::Memory);
        assert_eq!(by_mem[0].path, "user.slice");
        let (by_procs, _) = super::shown(&t.rows, "", true, 1000, Sort::Procs);
        assert_eq!(by_procs[0].path, "user.slice");
        assert_eq!(by_procs[1].path, "system.slice/example-db.service");
    }

    #[test]
    fn a_gap_is_drawn_never_hidden_as_empty() {
        let t = table();
        let mut rows = t.rows.clone();
        let idle = rows
            .iter_mut()
            .find(|r| r.empty())
            .expect("the fixture has an empty row");
        let path = idle.path.clone();
        idle.gap = true;
        let (shown, empty) = shown(&rows, "", true, 1000, Sort::Cpu);
        assert_eq!(empty, 0, "a gap is not counted as empty");
        assert!(shown.iter().any(|r| r.path == path), "{path} is drawn");
    }

    #[test]
    fn the_filter_reads_the_path_and_yours_is_your_user_slice() {
        let t = table();
        let (hit, _) = shown(&t.rows, "DB", true, 1000, Sort::Cpu);
        assert_eq!(hit.len(), 1);
        // Here user-1000.slice rolls up into user.slice, so no row is yours.
        let (mine, _) = shown(&t.rows, "", false, 1000, Sort::Cpu);
        assert!(mine.is_empty());
        let mut deeper = host(1);
        deeper.nodes[7].enabled = vec!["cpu".into(), "memory".into()];
        deeper.nodes[7].memory = Some(1 << 30);
        deeper.nodes[7].cpu = Some(CpuStat::default());
        let rows = rows(&deeper, None, 8);
        let (mine, _) = shown(&rows, "", false, 1000, Sort::Cpu);
        assert_eq!(mine[0].path, "user.slice/user-1000.slice");
        assert_eq!(mine[0].pids.len(), 203);
        let (theirs, _) = shown(&rows, "", false, 1001, Sort::Cpu);
        assert!(theirs.is_empty());
    }

    #[test]
    fn named_states_never_a_table() {
        let mut memo = Memo::default();
        assert_eq!(step(&mut memo, State::V1, None, 1.0, 8), Services::V1);
        assert_eq!(
            step(&mut memo, State::NotEnabled("cpu".into()), None, 1.0, 8),
            Services::NotEnabled(
                "cpu accounting is off everywhere · systemctl set-property <unit> CPUAccounting=yes"
                    .into()
            )
        );
        assert_eq!(
            step(&mut memo, State::Unreadable("denied".into()), None, 1.0, 8),
            Services::Unreadable("denied".into())
        );
        let Services::Table(t) = step(&mut memo, State::Container(host(0)), None, 1.0, 8) else {
            panic!()
        };
        assert!(t.container);
    }

    #[test]
    fn a_counter_that_went_backwards_is_a_new_cgroup() {
        let a = host(2);
        let mut b = host(3);
        b.nodes[4].cpu = Some(CpuStat {
            usage_usec: 7,
            ..CpuStat::default()
        });
        let d = delta_tree(&a, &b);
        assert_eq!(d.nodes[4].cpu.unwrap().usage_usec, 7);
        // A child that moved more than its parent over the window (a
        // walk torn across a burst): the parent's share is a gap.
        b.nodes[4].cpu = Some(CpuStat {
            usage_usec: 100_000_000,
            ..CpuStat::default()
        });
        let rows = rows(&b, Some((&a, 2.0)), 8);
        let slice = rows.iter().find(|r| r.path == "system.slice").unwrap();
        assert!(slice.gap && slice.cpu.is_none(), "{slice:?}");
    }

    #[test]
    fn a_share_holding_an_unread_childs_usage_is_not_a_figure() {
        let a = host(0);
        let mut b = host(1);
        // example-db's cpu.stat and memory.current fail this walk: its usage
        // cannot be taken out of system.slice's.
        b.nodes[3].cpu = None;
        b.nodes[3].memory = None;
        let rows = rows(&b, Some((&a, 2.0)), 8);
        let slice = rows.iter().find(|r| r.path == "system.slice").unwrap();
        assert!(slice.unread, "{slice:?}");
        assert!(!slice.gap, "nothing was floored");
        assert_eq!(slice.cpu, None, "db's CPU is not system.slice's own");
        assert_eq!(slice.memory, None, "db's memory is not system.slice's own");
        assert!(!slice.empty(), "an unread share is drawn, not hidden");
        let log = rows
            .iter()
            .find(|r| r.path == "system.slice/example-log.service")
            .unwrap();
        assert!(!log.unread && log.cpu.is_some() && log.memory.is_some());
    }

    #[test]
    fn sort_cycles_and_names_the_next() {
        assert_eq!(Sort::Cpu.hint(), "[s] sort by memory");
        assert_eq!(Sort::Memory.hint(), "[s] sort by procs");
        assert_eq!(Sort::Procs.hint(), "[s] sort by cpu");
    }
}
