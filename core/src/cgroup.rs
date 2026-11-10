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

//! The cgroup v2 tree, and who each reading in it belongs to.
//!
//! `io.stat`, `cpu.stat` and `memory.current` are hierarchical: a parent's
//! figure already contains every child's, so summing the files counts the
//! same byte once per level. [`attribute`] gives each cgroup with a
//! controller enabled its *own* share - its reading minus those of its
//! nearest descendants that carry the controller too - so the shares add
//! back up to the root's total and nothing is counted twice. How deep that
//! goes varies per slice on one host, which is why [`depth_sentence`] says
//! it from the tree rather than assuming it.
//!
//! Parsers, the [`Tree`] value, the attribution and [`walk`] (which reads
//! any directory laid out like cgroupfs, so tests run on a temp dir) are
//! compiled everywhere. Only [`read`], which knows the tree is mounted at
//! `/sys/fs/cgroup` and asks whether this is a container, is Linux. There is no cache and no thread: each call is one pass, and
//! every failure comes back as a [`State`] for the caller's poller to show.

use std::collections::BTreeMap;
use std::path::Path;

/// One `io.stat` line: cumulative counters for one block device.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct IoStat {
    /// `MAJ:MIN`, as the kernel wrote it. Naming the disk is the caller's.
    pub dev: String,
    pub rbytes: u64,
    pub wbytes: u64,
    pub rios: u64,
    pub wios: u64,
    pub dbytes: u64,
    pub dios: u64,
}

impl IoStat {
    /// `self - other` field by field, saturating, and whether a byte
    /// counter had to saturate (the other side counted more than this one).
    ///
    /// Only bytes decide that. The root's `io.stat` is filled from the
    /// disk's own statistics, where an I/O is a request after merging,
    /// while every other cgroup counts bios before it; so on a real host
    /// the root's `rios`/`wios` are routinely below its children's sum.
    /// They still saturate at zero, but flagging them would mark the root
    /// a gap on every sample. Bytes are the same unit at every level.
    fn minus(&self, other: &IoStat) -> (IoStat, bool) {
        let mut short = false;
        let mut bytes = |a: u64, b: u64| {
            short |= b > a;
            a.saturating_sub(b)
        };
        let rbytes = bytes(self.rbytes, other.rbytes);
        let wbytes = bytes(self.wbytes, other.wbytes);
        let dbytes = bytes(self.dbytes, other.dbytes);
        let out = IoStat {
            dev: self.dev.clone(),
            rbytes,
            wbytes,
            rios: self.rios.saturating_sub(other.rios),
            wios: self.wios.saturating_sub(other.wios),
            dbytes,
            dios: self.dios.saturating_sub(other.dios),
        };
        (out, short)
    }

    fn add(&mut self, other: &IoStat) {
        self.rbytes = self.rbytes.saturating_add(other.rbytes);
        self.wbytes = self.wbytes.saturating_add(other.wbytes);
        self.rios = self.rios.saturating_add(other.rios);
        self.wios = self.wios.saturating_add(other.wios);
        self.dbytes = self.dbytes.saturating_add(other.dbytes);
        self.dios = self.dios.saturating_add(other.dios);
    }
}

/// The `cpu.stat` fields a consumer draws. Microseconds, cumulative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CpuStat {
    pub usage_usec: u64,
    pub user_usec: u64,
    pub system_usec: u64,
    pub nr_throttled: u64,
    pub throttled_usec: u64,
}

/// One cgroup directory. Readings are `None` when the controller is not
/// enabled here, was not asked for, or its file could not be read.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Node {
    /// Relative to the walked root, `/`-separated; `""` is the root.
    pub path: String,
    /// Controllers enabled on this cgroup: its parent's
    /// `cgroup.subtree_control`, or the root's own `cgroup.controllers`.
    pub enabled: Vec<String>,
    pub io: Option<Vec<IoStat>>,
    pub cpu: Option<CpuStat>,
    pub memory: Option<u64>,
    pub procs: Option<Vec<i32>>,
}

impl Node {
    fn has(&self, controller: &str) -> bool {
        self.enabled.iter().any(|c| c == controller)
    }

    fn name(&self) -> &str {
        self.path.rsplit('/').next().unwrap_or("")
    }
}

/// A whole walk: the root first, then every directory under it. A plain
/// value - [`walk`] fills it, tests build it by hand, and a consumer diffs
/// two of them over its own window to get a rate.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Tree {
    pub nodes: Vec<Node>,
}

/// One cgroup's own share of a controller's reading.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Reading {
    /// Per device, keyed by `MAJ:MIN`.
    Io(BTreeMap<String, IoStat>),
    /// `usage_usec`.
    Cpu(u64),
    /// Bytes.
    Memory(u64),
}

/// What [`attribute`] gives each cgroup that has the controller enabled.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Attributed {
    pub path: String,
    /// Its reading with its nearest controller-enabled descendants taken
    /// out.
    pub own: Reading,
    /// Processes whose usage lands in `own`: this cgroup's and those of any
    /// descendants without the controller, which roll up into it. 0 when
    /// the walk was not asked for `cgroup.procs`.
    pub procs: usize,
    /// A descendant reported more than this cgroup (a counter reset during
    /// the walk, or one read a moment later than this one). `own` is still
    /// never negative, but the sample is a gap, not a figure.
    pub inconsistent: bool,
    /// A descendant with the controller had no reading, so its own usage
    /// could not be taken out and sits in `own`. The figure is a measured
    /// total, but not this cgroup's.
    pub unread: bool,
}

/// Bytes [`attribute_io`] could not take out of a cgroup's share on one
/// interval because its descendants reported more than it did, per cgroup
/// path and device, owed against the next. Counts that are read at
/// different moments - the root counts completions, its children
/// submissions - settle over later intervals, and without this the bytes
/// floored away on one would come back as the parent's own on the next.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Carry(BTreeMap<String, BTreeMap<String, IoStat>>);

impl Carry {
    /// True when nothing is owed anywhere.
    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }
}

/// What a walk found, for the consumer to draw.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum State {
    Ok(Tree),
    /// `cgroup.controllers` is missing at the root: cgroup v1 or hybrid.
    V1,
    /// The named controller is enabled on nothing below the root.
    /// [`State::remedy`] has the one line that turns it on.
    NotEnabled(String),
    /// PID 1 sits at `0::/` and a runtime marker says this is a container:
    /// the rows are this container's own cgroup namespace, not the host's.
    /// `0::/` alone is not enough - a host whose init stays in the root
    /// cgroup reads the same.
    Container(Tree),
    Unreadable(String),
}

impl State {
    /// For `NotEnabled`, the command that would enable the controller,
    /// with `<unit>` left for the reader to fill.
    pub fn remedy(&self) -> Option<String> {
        let State::NotEnabled(controller) = self else {
            return None;
        };
        let property = match controller.as_str() {
            "io" => "IOAccounting",
            "cpu" => "CPUAccounting",
            "memory" => "MemoryAccounting",
            "pids" => "TasksAccounting",
            _ => return None,
        };
        Some(format!("systemctl set-property <unit> {property}=yes"))
    }
}

/// `cgroup.controllers` or `cgroup.subtree_control`: space-separated names.
pub fn parse_controllers(text: &str) -> Vec<String> {
    text.split_whitespace().map(str::to_string).collect()
}

/// `io.stat`: one device per line, `MAJ:MIN key=value ...`. Unknown keys
/// are ignored, and a line that does not start with `MAJ:MIN` is skipped.
pub fn parse_io_stat(text: &str) -> Vec<IoStat> {
    let mut out = Vec::new();
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let Some(dev) = words.next() else { continue };
        let is_dev = dev
            .split_once(':')
            .map(|(maj, min)| {
                !maj.is_empty()
                    && !min.is_empty()
                    && maj.bytes().all(|b| b.is_ascii_digit())
                    && min.bytes().all(|b| b.is_ascii_digit())
            })
            .unwrap_or(false);
        if !is_dev {
            continue;
        }
        let mut stat = IoStat {
            dev: dev.to_string(),
            ..IoStat::default()
        };
        for word in words {
            let Some((key, value)) = word.split_once('=') else {
                continue;
            };
            let Ok(value) = value.parse::<u64>() else {
                continue;
            };
            match key {
                "rbytes" => stat.rbytes = value,
                "wbytes" => stat.wbytes = value,
                "rios" => stat.rios = value,
                "wios" => stat.wios = value,
                "dbytes" => stat.dbytes = value,
                "dios" => stat.dios = value,
                _ => {}
            }
        }
        out.push(stat);
    }
    out
}

/// `cpu.stat`: `key value` lines. `None` without `usage_usec`, the one
/// field every attribution needs.
pub fn parse_cpu_stat(text: &str) -> Option<CpuStat> {
    let mut stat = CpuStat::default();
    let mut usage = None;
    for line in text.lines() {
        let mut words = line.split_whitespace();
        let (Some(key), Some(value)) = (words.next(), words.next()) else {
            continue;
        };
        let Ok(value) = value.parse::<u64>() else {
            continue;
        };
        match key {
            "usage_usec" => usage = Some(value),
            "user_usec" => stat.user_usec = value,
            "system_usec" => stat.system_usec = value,
            "nr_throttled" => stat.nr_throttled = value,
            "throttled_usec" => stat.throttled_usec = value,
            _ => {}
        }
    }
    stat.usage_usec = usage?;
    Some(stat)
}

/// `memory.current`: one integer, in bytes.
pub fn parse_memory_current(text: &str) -> Option<u64> {
    text.trim().parse().ok()
}

/// `cgroup.procs`: one pid per line.
pub fn parse_cgroup_procs(text: &str) -> Vec<i32> {
    text.lines().filter_map(|l| l.trim().parse().ok()).collect()
}

/// True when pid 1's environment names a container runtime.
///
/// `/proc/1/environ` is NUL-separated `KEY=value` pairs. An empty
/// `container=` does not count. A cgroup path is not this signal: `0::/`
/// is what pid 1 sees in a private cgroup namespace and on a host whose
/// init stays in the root cgroup.
pub fn parse_container_env(text: &str) -> bool {
    text.split('\0').any(|entry| {
        entry
            .strip_prefix("container=")
            .is_some_and(|value| !value.is_empty())
    })
}

/// `/proc/<pid>/cgroup`: the v2 line's path (`0::<path>`), if there is one.
pub fn parse_proc_cgroup(text: &str) -> Option<String> {
    text.lines()
        .find_map(|l| l.strip_prefix("0::"))
        .map(|p| p.trim().to_string())
}

fn parent_of(path: &str) -> Option<&str> {
    if path.is_empty() {
        None
    } else {
        Some(path.rsplit_once('/').map(|(p, _)| p).unwrap_or(""))
    }
}

/// Child indices of every node, by path, so nothing depends on the order
/// the nodes were pushed in.
fn children(tree: &Tree) -> Vec<Vec<usize>> {
    let index: BTreeMap<&str, usize> = tree
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.path.as_str(), i))
        .collect();
    let mut kids = vec![Vec::new(); tree.nodes.len()];
    for (i, n) in tree.nodes.iter().enumerate() {
        if let Some(parent) = parent_of(&n.path).and_then(|p| index.get(p)) {
            kids[*parent].push(i);
        }
    }
    kids
}

/// Below `at`: the nearest descendants with `controller` enabled, and every
/// descendant in between that lacks it (whose usage rolls up into `at`).
fn nearest_enabled(
    tree: &Tree,
    kids: &[Vec<usize>],
    at: usize,
    controller: &str,
) -> (Vec<usize>, Vec<usize>) {
    let mut enabled = Vec::new();
    let mut rolled = Vec::new();
    let mut stack: Vec<usize> = kids[at].clone();
    while let Some(i) = stack.pop() {
        if tree.nodes[i].has(controller) {
            enabled.push(i);
        } else {
            rolled.push(i);
            stack.extend(kids[i].iter().copied());
        }
    }
    (enabled, rolled)
}

fn reading(node: &Node, controller: &str) -> Option<Reading> {
    match controller {
        "io" => node
            .io
            .as_ref()
            .map(|lines| Reading::Io(lines.iter().map(|s| (s.dev.clone(), s.clone())).collect())),
        "cpu" => node.cpu.map(|c| Reading::Cpu(c.usage_usec)),
        "memory" => node.memory.map(Reading::Memory),
        _ => None,
    }
}

/// How far a parent's cpu or memory figure may fall short of its children's
/// before the row is called inconsistent, as a share of the parent's.
///
/// The walk reads one file at a time, so a child is read a moment after its
/// parent and a busy child has used a little more by then. On a live host
/// that made busy parents with almost no CPU of their own `inconsistent` on
/// most polls - two or three rows of gaps every frame, each one a reading
/// that was right to within the time between two reads. A shortfall that
/// small is that skew, not a missing reading; the row's own share still
/// bottoms out at zero.
const READ_SKEW_PERCENT: u64 = 1;

/// Whether children's `taken` exceeds the parent's `own` by more than the
/// read skew allows.
fn beyond_read_skew(own: u64, taken: u64) -> bool {
    taken > own && taken - own > own / 100 * READ_SKEW_PERCENT
}

/// Every cgroup with `controller` enabled and a reading for it, with its
/// own share: the reading minus those of its nearest descendants that
/// also carry the controller. Subtraction saturates at zero and marks the
/// row `inconsistent`. A cgroup with no reading of its own (the root has
/// no `memory.current`) gets no row, but is still not counted twice: its
/// descendants keep their own shares. `controller` is `io`, `cpu` or
/// `memory`; anything else gives no rows.
pub fn attribute(tree: &Tree, controller: &str) -> Vec<Attributed> {
    attribute_with(tree, controller, None)
}

/// [`attribute`] for `io` read as deltas over successive intervals. Bytes
/// a cgroup's descendants reported beyond its own on one interval are owed
/// in `carry` and taken out of its share on the next, so a write its child
/// counted at submission and it counted at completion, an interval apart,
/// is the child's once and never the parent's. Only bytes are owed: the
/// root counts merged requests where its children count bios, so its ios
/// fall short of theirs for good. What is owed by a cgroup that is no
/// longer in the tree, or has no reading this interval, is dropped.
pub fn attribute_io(tree: &Tree, carry: &mut Carry) -> Vec<Attributed> {
    attribute_with(tree, "io", Some(carry))
}

fn attribute_with(
    tree: &Tree,
    controller: &str,
    mut carry: Option<&mut Carry>,
) -> Vec<Attributed> {
    let kids = children(tree);
    let mut owed_next: BTreeMap<String, BTreeMap<String, IoStat>> = BTreeMap::new();
    let mut out = Vec::new();
    for (i, node) in tree.nodes.iter().enumerate() {
        if !node.has(controller) {
            continue;
        }
        let Some(total) = reading(node, controller) else {
            continue;
        };
        let (mut pending, rolled) = nearest_enabled(tree, &kids, i, controller);
        let mut inconsistent = false;
        let mut unread = false;
        // A child with no reading gets no row, so its own nearest enabled
        // descendants - which do get rows - are subtracted in its place.
        // Otherwise their bytes would be counted here and again on their
        // own rows. The child's own bytes stay in this share, so it is
        // marked unread.
        let mut below: Vec<Reading> = Vec::new();
        while let Some(j) = pending.pop() {
            match reading(&tree.nodes[j], controller) {
                Some(r) => below.push(r),
                None => {
                    unread = true;
                    pending.extend(nearest_enabled(tree, &kids, j, controller).0);
                }
            }
        }
        let own = match total {
            Reading::Io(mut devs) => {
                let mut taken: BTreeMap<String, IoStat> = BTreeMap::new();
                for r in &below {
                    if let Reading::Io(child) = r {
                        for (dev, s) in child {
                            taken
                                .entry(dev.clone())
                                .or_insert_with(|| IoStat {
                                    dev: dev.clone(),
                                    ..IoStat::default()
                                })
                                .add(s);
                        }
                    }
                }
                let owed = carry.as_deref().and_then(|c| c.0.get(&node.path));
                for (dev, t) in &taken {
                    let mine = devs.entry(dev.clone()).or_insert_with(|| IoStat {
                        dev: dev.clone(),
                        ..IoStat::default()
                    });
                    let (left, short) = mine.minus(t);
                    inconsistent |= short;
                    if short && carry.is_some() {
                        let (over, _) = t.minus(mine);
                        owed_next
                            .entry(node.path.clone())
                            .or_default()
                            .insert(dev.clone(), bytes_only(over));
                    }
                    *mine = left;
                }
                // What was owed from the last interval comes out of what is
                // left; whatever it still cannot cover is owed again.
                for (dev, debt) in owed.into_iter().flatten() {
                    let zero = IoStat {
                        dev: dev.clone(),
                        ..IoStat::default()
                    };
                    let mine = devs.get_mut(dev);
                    let have = mine.as_deref().unwrap_or(&zero);
                    let still = bytes_only(debt.minus(have).0);
                    if let Some(mine) = mine {
                        // A debt carries no ios, so only bytes move.
                        *mine = mine.minus(debt).0;
                    }
                    if still.rbytes > 0 || still.wbytes > 0 || still.dbytes > 0 {
                        owed_next
                            .entry(node.path.clone())
                            .or_default()
                            .entry(dev.clone())
                            .or_insert_with(|| IoStat {
                                dev: dev.clone(),
                                ..IoStat::default()
                            })
                            .add(&still);
                    }
                }
                Reading::Io(devs)
            }
            Reading::Cpu(v) | Reading::Memory(v) => {
                let taken: u64 = below
                    .iter()
                    .map(|r| match r {
                        Reading::Cpu(x) | Reading::Memory(x) => *x,
                        Reading::Io(_) => 0,
                    })
                    .fold(0u64, u64::saturating_add);
                inconsistent |= beyond_read_skew(v, taken);
                let left = v.saturating_sub(taken);
                if controller == "cpu" {
                    Reading::Cpu(left)
                } else {
                    Reading::Memory(left)
                }
            }
        };
        let procs = std::iter::once(i)
            .chain(rolled)
            .map(|j| tree.nodes[j].procs.as_ref().map_or(0, Vec::len))
            .sum();
        out.push(Attributed {
            path: node.path.clone(),
            own,
            procs,
            inconsistent,
            unread,
        });
    }
    if let Some(carry) = carry.as_deref_mut() {
        carry.0 = owed_next;
    }
    out
}

/// An owed amount: bytes only, since ios never reconcile (see
/// [`attribute_io`]).
fn bytes_only(stat: IoStat) -> IoStat {
    IoStat {
        rios: 0,
        wios: 0,
        dios: 0,
        ..stat
    }
}

/// What a level of units is called, from the unit-type suffix.
fn kind(name: &str) -> &'static str {
    match name.rsplit_once('.').map(|(_, s)| s) {
        Some("service") => "services",
        Some("scope") => "scopes",
        Some("slice") => "slices",
        Some("mount") => "mounts",
        Some("socket") => "sockets",
        Some("swap") => "swaps",
        _ => "cgroups",
    }
}

/// Kinds, most common first, joined for a sentence.
fn kinds_phrase<'a>(names: impl Iterator<Item = &'a str>) -> String {
    let mut count: BTreeMap<&'static str, usize> = BTreeMap::new();
    for n in names {
        *count.entry(kind(n)).or_default() += 1;
    }
    let mut ranked: Vec<(&str, usize)> = count.into_iter().collect();
    ranked.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
    ranked
        .iter()
        .map(|(k, _)| *k)
        .collect::<Vec<_>>()
        .join(", ")
}

/// The header line a consumer draws over [`attribute`]'s rows, saying how
/// deep the accounting reaches on this host, e.g. `io accounting down to:
/// services, scopes; user sessions roll up to user.slice`. Built from the
/// tree: the deepest controller-enabled level is named by its unit suffix,
/// and a top-level slice whose children lack the controller says they
/// roll up into it.
pub fn depth_sentence(tree: &Tree, controller: &str) -> String {
    let kids = children(tree);
    let Some(root) = tree.nodes.iter().position(|n| n.path.is_empty()) else {
        return format!("{controller} accounting: no cgroup tree");
    };
    if !tree.nodes[root].has(controller) {
        return format!("{controller} accounting is not available here");
    }
    let mut deepest: Vec<&str> = Vec::new();
    let mut rollups: Vec<String> = Vec::new();
    let mut stack = vec![root];
    while let Some(i) = stack.pop() {
        let (below, _) = nearest_enabled(tree, &kids, i, controller);
        if !below.is_empty() {
            stack.extend(below);
            continue;
        }
        let node = &tree.nodes[i];
        if i == root {
            return format!("{controller} accounting at the root only; everything rolls up to it");
        }
        let top_level = !node.path.contains('/');
        if top_level && !kids[i].is_empty() {
            let who = if node.name() == "user.slice" {
                "user sessions".to_string()
            } else {
                kinds_phrase(kids[i].iter().map(|&k| tree.nodes[k].name()))
            };
            rollups.push(format!("{who} roll up to {}", node.name()));
        } else {
            deepest.push(node.name());
        }
    }
    rollups.sort();
    let mut parts = Vec::new();
    if !deepest.is_empty() {
        parts.push(format!(
            "{controller} accounting down to: {}",
            kinds_phrase(deepest.into_iter())
        ));
    } else {
        parts.push(format!("{controller} accounting down to: top-level slices"));
    }
    parts.extend(rollups);
    parts.join("; ")
}

fn read_file(path: &Path) -> std::io::Result<String> {
    std::fs::read_to_string(path)
}

/// One pass over a cgroup v2 tree rooted at `root`, reading only what the
/// requested `controllers` need on the cgroups that have them enabled,
/// and `cgroup.procs` only when `with_procs` asks (it is the largest file).
/// One `read_dir` per directory; nothing that is not a directory is
/// entered and symlinks are not followed.
///
/// `Err(V1)` when the root has no `cgroup.controllers`; `Err(NotEnabled)`
/// naming the first requested controller enabled on nothing below the
/// root (walk one controller at a time to keep the others); `Err(Unreadable)`
/// when a directory cannot be listed. A cgroup removed during the walk is
/// simply absent.
pub fn walk(root: &Path, controllers: &[&str], with_procs: bool) -> Result<Tree, State> {
    let root_controllers = match read_file(&root.join("cgroup.controllers")) {
        Ok(text) => parse_controllers(&text),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Err(State::V1),
        Err(e) => return Err(State::Unreadable(format!("{}: {e}", root.display()))),
    };
    let mut tree = Tree::default();
    // (relative path, controllers enabled on it)
    let mut stack: Vec<(String, Vec<String>)> = vec![(String::new(), root_controllers)];
    while let Some((rel, enabled)) = stack.pop() {
        let dir = if rel.is_empty() {
            root.to_path_buf()
        } else {
            root.join(&rel)
        };
        let on = |c: &str| controllers.contains(&c) && enabled.iter().any(|e| e == c);
        let io = on("io")
            .then(|| {
                read_file(&dir.join("io.stat"))
                    .ok()
                    .map(|t| parse_io_stat(&t))
            })
            .flatten();
        let cpu = on("cpu")
            .then(|| {
                read_file(&dir.join("cpu.stat"))
                    .ok()
                    .and_then(|t| parse_cpu_stat(&t))
            })
            .flatten();
        let memory = on("memory")
            .then(|| {
                read_file(&dir.join("memory.current"))
                    .ok()
                    .and_then(|t| parse_memory_current(&t))
            })
            .flatten();
        let procs = with_procs
            .then(|| {
                read_file(&dir.join("cgroup.procs"))
                    .ok()
                    .map(|t| parse_cgroup_procs(&t))
            })
            .flatten();
        let subtree = read_file(&dir.join("cgroup.subtree_control"))
            .map(|t| parse_controllers(&t))
            .unwrap_or_default();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound && !rel.is_empty() => continue,
            Err(e) => return Err(State::Unreadable(format!("{}: {e}", dir.display()))),
        };
        let mut subdirs: Vec<String> = entries
            .flatten()
            .filter(|e| e.file_type().map(|t| t.is_dir()).unwrap_or(false))
            .filter_map(|e| e.file_name().into_string().ok())
            .collect();
        // Reverse-sorted onto the stack, so the walk comes out in order.
        subdirs.sort_unstable_by(|a, b| b.cmp(a));
        for name in subdirs {
            let child = if rel.is_empty() {
                name
            } else {
                format!("{rel}/{name}")
            };
            stack.push((child, subtree.clone()));
        }
        tree.nodes.push(Node {
            path: rel,
            enabled,
            io,
            cpu,
            memory,
            procs,
        });
    }
    for &c in controllers {
        let anywhere = tree.nodes.iter().any(|n| !n.path.is_empty() && n.has(c));
        if !anywhere {
            return Err(State::NotEnabled(c.to_string()));
        }
    }
    Ok(tree)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::{State, parse_container_env, parse_proc_cgroup, walk};
    use std::path::Path;

    /// A runtime marker: the files Docker and Podman leave at the root, or
    /// `container=` in pid 1's environment (readable only as its owner).
    pub fn in_container() -> bool {
        if Path::new("/.dockerenv").is_file() || Path::new("/run/.containerenv").is_file() {
            return true;
        }
        std::fs::read("/proc/1/environ")
            .is_ok_and(|bytes| parse_container_env(&String::from_utf8_lossy(&bytes)))
    }

    /// The host's tree at `/sys/fs/cgroup`, or why it cannot be read.
    pub fn read(controllers: &[&str], with_procs: bool) -> State {
        let tree = match walk(Path::new("/sys/fs/cgroup"), controllers, with_procs) {
            Ok(tree) => tree,
            Err(state) => return state,
        };
        let pid1 = std::fs::read_to_string("/proc/1/cgroup").unwrap_or_default();
        if parse_proc_cgroup(&pid1).as_deref() == Some("/") && in_container() {
            State::Container(tree)
        } else {
            State::Ok(tree)
        }
    }
}

#[cfg(target_os = "linux")]
pub use linux::{in_container, read};

#[cfg(test)]
mod tests {
    use super::*;

    fn node(path: &str, enabled: &[&str]) -> Node {
        Node {
            path: path.to_string(),
            enabled: enabled.iter().map(|s| s.to_string()).collect(),
            ..Node::default()
        }
    }

    fn io(dev: &str, rbytes: u64, wbytes: u64) -> Vec<IoStat> {
        vec![IoStat {
            dev: dev.to_string(),
            rbytes,
            wbytes,
            ..IoStat::default()
        }]
    }

    fn own_io(rows: &[Attributed], path: &str, dev: &str) -> (u64, u64) {
        let row = rows.iter().find(|r| r.path == path).expect(path);
        let Reading::Io(devs) = &row.own else {
            panic!("not io")
        };
        let s = &devs[dev];
        (s.rbytes, s.wbytes)
    }

    #[test]
    fn a_container_variable_names_a_runtime_and_an_empty_one_does_not() {
        assert!(parse_container_env("container=podman\0PATH=/bin"));
        assert!(parse_container_env("PATH=/bin\0container=docker"));
        assert!(!parse_container_env("container=\0PATH=/bin"));
        assert!(!parse_container_env("container_name=web\0"));
        assert!(!parse_container_env(""));
        assert!(!parse_container_env("0::/\n"));
    }

    #[test]
    fn controllers_lines() {
        assert_eq!(
            parse_controllers("cpuset cpu io memory pids\n"),
            vec!["cpuset", "cpu", "io", "memory", "pids"]
        );
        assert!(parse_controllers("\n").is_empty());
        assert!(parse_controllers("").is_empty());
    }

    #[test]
    fn io_stat_two_devices_and_an_unknown_key() {
        let text = "8:0 rbytes=1024 wbytes=2048 rios=3 wios=4 dbytes=5 dios=6 future=9\n\
                    259:1 rbytes=10 wbytes=20 rios=1 wios=2 dbytes=0 dios=0\n\
                    garbage rbytes=1\n\
                    7:7 \n";
        let got = parse_io_stat(text);
        assert_eq!(got.len(), 3);
        assert_eq!(
            got[0],
            IoStat {
                dev: "8:0".into(),
                rbytes: 1024,
                wbytes: 2048,
                rios: 3,
                wios: 4,
                dbytes: 5,
                dios: 6
            }
        );
        assert_eq!(got[1].dev, "259:1");
        assert_eq!(got[1].wbytes, 20);
        // A device the kernel lists with no counters yet reads as zeros.
        assert_eq!(
            got[2],
            IoStat {
                dev: "7:7".into(),
                ..IoStat::default()
            }
        );
    }

    #[test]
    fn cpu_stat() {
        let text = "usage_usec 5000\nuser_usec 3000\nsystem_usec 2000\n\
                    nr_periods 10\nnr_throttled 2\nthrottled_usec 400\nnr_bursts 0\n";
        assert_eq!(
            parse_cpu_stat(text),
            Some(CpuStat {
                usage_usec: 5000,
                user_usec: 3000,
                system_usec: 2000,
                nr_throttled: 2,
                throttled_usec: 400
            })
        );
        assert_eq!(parse_cpu_stat("user_usec 3\n"), None);
    }

    #[test]
    fn memory_current_and_procs() {
        assert_eq!(parse_memory_current("123456789\n"), Some(123_456_789));
        assert_eq!(parse_memory_current("max\n"), None);
        assert_eq!(parse_memory_current(""), None);
        assert_eq!(parse_cgroup_procs("1\n42\n\n7\n"), vec![1, 42, 7]);
        assert!(parse_cgroup_procs("").is_empty());
    }

    #[test]
    fn proc_cgroup_v2_line() {
        assert_eq!(parse_proc_cgroup("0::/\n").as_deref(), Some("/"));
        assert_eq!(
            parse_proc_cgroup("0::/system.slice/example-db.service\n").as_deref(),
            Some("/system.slice/example-db.service")
        );
        assert_eq!(parse_proc_cgroup("12:cpu,cpuacct:/\n"), None);
    }

    /// root -> system.slice -> example-db.service, all three with io: the
    /// owns add back up to the root's hierarchical total, nothing twice.
    #[test]
    fn io_is_never_counted_twice() {
        let all = ["cpu", "io", "memory"];
        let mut root = node("", &all);
        root.io = Some(io("8:0", 1000, 5000));
        let mut slice = node("system.slice", &all);
        slice.io = Some(io("8:0", 700, 3000));
        let mut svc = node("system.slice/example-db.service", &all);
        svc.io = Some(io("8:0", 600, 2500));
        let tree = Tree {
            nodes: vec![svc, root, slice],
        };
        let rows = attribute(&tree, "io");
        assert_eq!(rows.len(), 3);
        assert_eq!(own_io(&rows, "", "8:0"), (300, 2000));
        assert_eq!(own_io(&rows, "system.slice", "8:0"), (100, 500));
        assert_eq!(
            own_io(&rows, "system.slice/example-db.service", "8:0"),
            (600, 2500)
        );
        let (r, w) = rows
            .iter()
            .map(|row| own_io(&rows, &row.path, "8:0"))
            .fold((0, 0), |a, b| (a.0 + b.0, a.1 + b.1));
        assert_eq!((r, w), (1000, 5000));
        assert!(rows.iter().all(|r| !r.inconsistent));
    }

    /// Two devices, subtracted per device; a device only a child touched
    /// is still the child's.
    #[test]
    fn io_subtracts_per_device() {
        let mut root = node("", &["io"]);
        root.io = Some(vec![
            IoStat {
                dev: "8:0".into(),
                wbytes: 100,
                ..IoStat::default()
            },
            IoStat {
                dev: "8:16".into(),
                wbytes: 40,
                ..IoStat::default()
            },
        ]);
        let mut a = node("a.slice", &["io"]);
        a.io = Some(vec![IoStat {
            dev: "8:16".into(),
            wbytes: 40,
            ..IoStat::default()
        }]);
        let rows = attribute(
            &Tree {
                nodes: vec![root, a],
            },
            "io",
        );
        assert_eq!(own_io(&rows, "", "8:0"), (0, 100));
        assert_eq!(own_io(&rows, "", "8:16"), (0, 0));
        assert_eq!(own_io(&rows, "a.slice", "8:16"), (0, 40));
    }

    /// cpu and memory, with processes in a child without the controller
    /// rolling up into the nearest enabled ancestor.
    #[test]
    fn cpu_memory_and_rolled_up_procs() {
        let mut root = node("", &["cpu", "memory"]);
        root.cpu = Some(CpuStat {
            usage_usec: 900,
            ..CpuStat::default()
        });
        root.procs = Some(vec![1]);
        let mut user = node("user.slice", &["cpu", "memory"]);
        user.cpu = Some(CpuStat {
            usage_usec: 400,
            ..CpuStat::default()
        });
        user.memory = Some(4096);
        user.procs = Some(vec![]);
        let mut session = node("user.slice/user-1000.slice", &[]);
        session.cpu = Some(CpuStat {
            usage_usec: 300,
            ..CpuStat::default()
        });
        session.procs = Some(vec![10, 11, 12]);
        let tree = Tree {
            nodes: vec![root, user, session],
        };
        let cpu = attribute(&tree, "cpu");
        assert_eq!(cpu.len(), 2);
        assert_eq!(cpu[0].own, Reading::Cpu(500));
        assert_eq!(cpu[0].procs, 1);
        assert_eq!(cpu[1].own, Reading::Cpu(400));
        assert_eq!(cpu[1].procs, 3);
        // The root has no memory.current: no row, and the slice keeps its own.
        let mem = attribute(&tree, "memory");
        assert_eq!(mem.len(), 1);
        assert_eq!(mem[0].path, "user.slice");
        assert_eq!(mem[0].own, Reading::Memory(4096));
        assert!(attribute(&tree, "pids").is_empty());
    }

    /// A child read a moment after its parent can be a little ahead of it.
    /// Within the read skew that is not a gap: the parent's own share is
    /// zero and the row is not flagged. Past it, the row is flagged.
    #[test]
    fn a_child_just_ahead_of_its_parent_is_read_skew_not_a_gap() {
        let tree_with = |child: u64| {
            let mut root = node("", &["cpu"]);
            root.cpu = Some(CpuStat {
                usage_usec: 1_000_000,
                ..CpuStat::default()
            });
            let mut svc = node("example-db.service", &["cpu"]);
            svc.cpu = Some(CpuStat {
                usage_usec: child,
                ..CpuStat::default()
            });
            Tree { nodes: vec![root, svc] }
        };
        let skewed = attribute(&tree_with(1_009_000), "cpu");
        assert_eq!(skewed[0].own, Reading::Cpu(0));
        assert!(!skewed[0].inconsistent, "0.9% ahead is read skew");
        let short = attribute(&tree_with(1_020_000), "cpu");
        assert_eq!(short[0].own, Reading::Cpu(0));
        assert!(short[0].inconsistent, "2% ahead is a real shortfall");
    }

    /// A child reporting more than its parent: zero, never negative, and
    /// flagged so the consumer draws a gap.
    #[test]
    fn counter_reset_saturates_and_is_flagged() {
        let mut root = node("", &["io", "cpu"]);
        root.io = Some(io("8:0", 100, 100));
        root.cpu = Some(CpuStat {
            usage_usec: 50,
            ..CpuStat::default()
        });
        let mut svc = node("example-db.service", &["io", "cpu"]);
        svc.io = Some(io("8:0", 300, 50));
        svc.cpu = Some(CpuStat {
            usage_usec: 80,
            ..CpuStat::default()
        });
        let tree = Tree {
            nodes: vec![root, svc],
        };
        let rows = attribute(&tree, "io");
        assert_eq!(own_io(&rows, "", "8:0"), (0, 50));
        assert!(rows[0].inconsistent);
        assert!(!rows[1].inconsistent);
        let cpu = attribute(&tree, "cpu");
        assert_eq!(cpu[0].own, Reading::Cpu(0));
        assert!(cpu[0].inconsistent);
    }

    /// The root counts merged requests and its children count bios, so
    /// its ios fall short of theirs on every real host: saturated, not a gap.
    #[test]
    fn root_ios_below_its_children_are_not_a_gap() {
        let mut root = node("", &["io"]);
        root.io = Some(vec![IoStat {
            dev: "8:0".into(),
            wbytes: 900,
            wios: 3,
            ..IoStat::default()
        }]);
        let mut svc = node("example-db.service", &["io"]);
        svc.io = Some(vec![IoStat {
            dev: "8:0".into(),
            wbytes: 800,
            wios: 40,
            ..IoStat::default()
        }]);
        let rows = attribute(
            &Tree {
                nodes: vec![root, svc],
            },
            "io",
        );
        let Reading::Io(devs) = &rows[0].own else {
            panic!("not io")
        };
        assert_eq!((devs["8:0"].wbytes, devs["8:0"].wios), (100, 0));
        assert!(!rows[0].inconsistent);
    }

    /// A descendant whose file could not be read cannot be taken out, so
    /// the parent's share is not a figure.
    #[test]
    fn missing_child_reading_is_flagged() {
        let mut root = node("", &["io"]);
        root.io = Some(io("8:0", 10, 10));
        let child = node("example-db.service", &["io"]);
        let rows = attribute(
            &Tree {
                nodes: vec![root, child],
            },
            "io",
        );
        assert_eq!(rows.len(), 1);
        assert!(rows[0].unread, "the child's bytes are in the root's share");
        assert!(!rows[0].inconsistent, "nothing was floored");
    }

    /// An unreadable middle cgroup: its readable child has a row of its
    /// own, so the parent subtracts that child instead, and the shares
    /// still add up to the root's total rather than counting it twice.
    #[test]
    fn a_missing_middle_reading_does_not_count_its_children_twice() {
        let mut root = node("", &["io"]);
        root.io = Some(io("8:0", 0, 100));
        let middle = node("system.slice", &["io"]);
        let mut leaf = node("system.slice/example-db.service", &["io"]);
        leaf.io = Some(io("8:0", 0, 80));
        let rows = attribute(
            &Tree {
                nodes: vec![root, middle, leaf],
            },
            "io",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(own_io(&rows, "", "8:0"), (0, 20));
        assert_eq!(own_io(&rows, "system.slice/example-db.service", "8:0"), (0, 80));
        assert!(rows[0].unread, "the middle's own bytes are in the root's share");
        assert!(!rows[1].unread);
    }

    /// A write the child counted at submission and the root at completion,
    /// an interval apart: the child's once, and never the root's own.
    #[test]
    fn bytes_floored_on_one_interval_are_owed_on_the_next() {
        let two = |root_w: u64, child_w: u64| {
            let mut root = node("", &["io"]);
            root.io = Some(io("8:0", 0, root_w));
            let mut child = node("example-db.service", &["io"]);
            child.io = Some(io("8:0", 0, child_w));
            Tree {
                nodes: vec![root, child],
            }
        };
        let mut carry = Carry::default();
        let first = attribute_io(&two(0, 1 << 20), &mut carry);
        assert_eq!(own_io(&first, "", "8:0"), (0, 0));
        assert_eq!(own_io(&first, "example-db.service", "8:0"), (0, 1 << 20));
        assert!(!carry.is_empty());
        let second = attribute_io(&two(1 << 20, 0), &mut carry);
        assert_eq!(
            own_io(&second, "", "8:0"),
            (0, 0),
            "the completion is not the root's own"
        );
        assert!(carry.is_empty(), "the debt is settled");
        // Settled, the root's own writes count again.
        let third = attribute_io(&two(4096, 0), &mut carry);
        assert_eq!(own_io(&third, "", "8:0"), (0, 4096));
        // Without the carry, the same intervals count the write twice:
        // that is what attribute_io is for.
        assert_eq!(
            own_io(&attribute(&two(1 << 20, 0), "io"), "", "8:0"),
            (0, 1 << 20)
        );
    }

    /// A debt larger than the next interval's own bytes is paid off over
    /// several, and only bytes are ever owed.
    #[test]
    fn a_debt_is_paid_over_several_intervals_and_in_bytes_only() {
        let tree = |root_w: u64, child_w: u64| {
            let mut root = node("", &["io"]);
            root.io = Some(io("8:0", 0, root_w));
            let mut child = node("example-db.service", &["io"]);
            child.io = Some(io("8:0", 0, child_w));
            Tree {
                nodes: vec![root, child],
            }
        };
        let mut carry = Carry::default();
        attribute_io(&tree(0, 300), &mut carry);
        let root_own =
            |carry: &mut Carry, w: u64| own_io(&attribute_io(&tree(w, 0), carry), "", "8:0");
        assert_eq!(root_own(&mut carry, 100), (0, 0));
        assert_eq!(root_own(&mut carry, 150), (0, 0));
        assert_eq!(root_own(&mut carry, 100), (0, 50));
        assert!(carry.is_empty());
        // Only bytes are owed: the root's ios still read as they are.
        let mut root = node("", &["io"]);
        root.io = Some(vec![IoStat {
            dev: "8:0".into(),
            wbytes: 0,
            wios: 2,
            ..IoStat::default()
        }]);
        let mut child = node("example-db.service", &["io"]);
        child.io = Some(vec![IoStat {
            dev: "8:0".into(),
            wbytes: 10,
            wios: 9,
            ..IoStat::default()
        }]);
        attribute_io(
            &Tree {
                nodes: vec![root, child],
            },
            &mut carry,
        );
        let Carry(owed) = &carry;
        assert_eq!(owed[""]["8:0"].wbytes, 10);
        assert_eq!(owed[""]["8:0"].wios, 0);
    }

    fn host_like() -> Tree {
        let all = ["cpu", "io", "memory"];
        Tree {
            nodes: vec![
                node("", &all),
                node("init.scope", &all),
                node("system.slice", &all),
                node("system.slice/example-db.service", &all),
                node("system.slice/example-web.service", &all),
                node("system.slice/example-web.service/worker", &[]),
                node("user.slice", &all),
                node("user.slice/user-1000.slice", &[]),
                node("user.slice/user-1000.slice/session-1.scope", &[]),
            ],
        }
    }

    /// user.slice carries cpu but its sessions do not: they roll up, and
    /// the sentence says so.
    #[test]
    fn depth_sentence_names_the_roll_up() {
        let tree = host_like();
        assert_eq!(
            depth_sentence(&tree, "cpu"),
            "cpu accounting down to: services, scopes; user sessions roll up to user.slice"
        );
        // A controller not enabled below the service: its sub-cgroup is
        // the service's, and needs no clause of its own.
        assert_eq!(
            depth_sentence(&tree, "io"),
            "io accounting down to: services, scopes; user sessions roll up to user.slice"
        );
    }

    #[test]
    fn depth_sentence_at_the_root_and_nowhere() {
        let tree = Tree {
            nodes: vec![node("", &["io"]), node("system.slice", &[])],
        };
        assert_eq!(
            depth_sentence(&tree, "io"),
            "io accounting at the root only; everything rolls up to it"
        );
        assert_eq!(
            depth_sentence(&tree, "cpu"),
            "cpu accounting is not available here"
        );
    }

    #[test]
    fn remedy_is_text_for_the_consumer() {
        assert_eq!(
            State::NotEnabled("io".into()).remedy().as_deref(),
            Some("systemctl set-property <unit> IOAccounting=yes")
        );
        assert_eq!(
            State::NotEnabled("cpu".into()).remedy().as_deref(),
            Some("systemctl set-property <unit> CPUAccounting=yes")
        );
        assert_eq!(State::V1.remedy(), None);
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        crate::statefile::tests::temp_dir(&format!("cgroup-{tag}"))
    }

    fn write(dir: &Path, rel: &str, text: &str) {
        let path = dir.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, text).unwrap();
    }

    #[test]
    fn walk_on_a_v1_root() {
        let dir = temp_dir("v1");
        write(&dir, "cpu/tasks", "1\n");
        assert_eq!(walk(&dir, &["io"], false), Err(State::V1));
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn walk_on_a_missing_root_is_v1_not_a_panic() {
        let dir = temp_dir("gone").join("nothing-here");
        assert_eq!(walk(&dir, &["io"], false), Err(State::V1));
    }

    #[test]
    fn walk_on_a_v2_tree() {
        let dir = temp_dir("v2");
        write(&dir, "cgroup.controllers", "cpu io memory pids\n");
        write(&dir, "cgroup.subtree_control", "cpu io memory\n");
        write(
            &dir,
            "io.stat",
            "8:0 rbytes=100 wbytes=900 rios=1 wios=9 dbytes=0 dios=0\n",
        );
        write(
            &dir,
            "cpu.stat",
            "usage_usec 1000\nuser_usec 600\nsystem_usec 400\n",
        );
        write(&dir, "cgroup.procs", "1\n");
        write(&dir, "system.slice/cgroup.subtree_control", "io\n");
        write(
            &dir,
            "system.slice/io.stat",
            "8:0 rbytes=60 wbytes=500 rios=1 wios=5 dbytes=0 dios=0\n",
        );
        write(&dir, "system.slice/cpu.stat", "usage_usec 700\n");
        write(&dir, "system.slice/memory.current", "8192\n");
        write(&dir, "system.slice/cgroup.procs", "");
        write(
            &dir,
            "system.slice/example-db.service/cgroup.subtree_control",
            "",
        );
        write(
            &dir,
            "system.slice/example-db.service/io.stat",
            "8:0 rbytes=60 wbytes=400 rios=1 wios=4 dbytes=0 dios=0\n",
        );
        write(
            &dir,
            "system.slice/example-db.service/cpu.stat",
            "usage_usec 650\n",
        );
        write(
            &dir,
            "system.slice/example-db.service/cgroup.procs",
            "200\n201\n",
        );

        let tree = walk(&dir, &["io"], false).expect("v2 tree");
        let paths: Vec<&str> = tree.nodes.iter().map(|n| n.path.as_str()).collect();
        assert_eq!(
            paths,
            ["", "system.slice", "system.slice/example-db.service"]
        );
        // Only what io needs was read.
        assert!(
            tree.nodes
                .iter()
                .all(|n| n.cpu.is_none() && n.procs.is_none())
        );
        assert_eq!(tree.nodes[2].enabled, vec!["io"]);
        let rows = attribute(&tree, "io");
        assert_eq!(own_io(&rows, "", "8:0"), (40, 400));
        assert_eq!(own_io(&rows, "system.slice", "8:0"), (0, 100));
        assert_eq!(
            own_io(&rows, "system.slice/example-db.service", "8:0"),
            (60, 400)
        );

        // cpu stops at system.slice: the service is not enabled for it.
        let tree = walk(&dir, &["cpu", "memory"], true).expect("v2 tree");
        assert_eq!(tree.nodes[0].memory, None);
        assert_eq!(tree.nodes[1].memory, Some(8192));
        assert_eq!(tree.nodes[2].cpu, None);
        let cpu = attribute(&tree, "cpu");
        assert_eq!(cpu.len(), 2);
        assert_eq!(cpu[1].own, Reading::Cpu(700));
        assert_eq!(cpu[1].procs, 2);

        // pids is enabled at the root only.
        assert_eq!(
            walk(&dir, &["pids"], false),
            Err(State::NotEnabled("pids".into()))
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
