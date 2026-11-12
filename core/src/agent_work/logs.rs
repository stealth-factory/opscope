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

//! Local session acquisition, cached by file identity/length and read incrementally.
use super::{
    branch,
    parse::{self, Transcript},
    Via,
};
use std::{
    collections::{HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Seek, SeekFrom},
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    sync::Arc,
    time::SystemTime,
};

/// The `CLAUDE_CONFIG_DIR` a process was started with, from its own
/// environment. `None` where it names none or that cannot be read (no
/// `/proc`, or another user's process).
fn claude_config_dir_of(pid: u32) -> Option<PathBuf> {
    let env = crate::procfs::read_environ(pid as i32).ok()?;
    crate::procfs::parse_environ_var(&env, "CLAUDE_CONFIG_DIR").map(PathBuf::from)
}

/// [`claude_config_dir_of`], or `$HOME/.claude` when the process names none.
fn claude_root_of(pid: u32) -> Option<PathBuf> {
    let env = crate::procfs::read_environ(pid as i32).ok()?;
    crate::procfs::parse_environ_var(&env, "CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| crate::procfs::parse_environ_var(&env, "HOME").map(|home| Path::new(home).join(".claude")))
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Activity {
    #[default]
    Unknown,
    Working,
    Waiting,
    Done,
    Error,
}
impl Activity {
    pub fn finished(self) -> bool {
        matches!(self, Self::Done | Self::Error)
    }
    pub fn words(self) -> &'static str {
        match self {
            Self::Unknown => "state unknown",
            Self::Working => "working",
            Self::Waiting => "waiting on background work",
            Self::Done => "done",
            Self::Error => "stopped",
        }
    }
}
#[derive(Clone, Debug, PartialEq)]
pub struct LogTask {
    pub id: String,
    pub label: String,
    pub state: Activity,
    pub updated: String,
}
#[derive(Clone, Debug, Default, PartialEq)]
pub struct LogWork {
    pub session_id: String,
    /// No multiplexer-provided session ID: newest root transcript for this cwd.
    pub guessed: bool,
    /// The session came from the agent's own pid file (`sessions/<pid>.json`).
    pub bound: bool,
    /// Archive records that could not be indexed; known tasks remain usable.
    pub warnings: Vec<String>,
    /// Includes finished items for consumers that need history. Mux draws active work only.
    pub tasks: Vec<LogTask>,
}
impl LogWork {
    pub fn explanation(&self) -> Option<String> {
        (!self.session_id.is_empty()).then(|| {
            format!(
                "session {} · {}",
                self.session_id,
                if self.guessed {
                    "guess: newest session for this cwd; concurrent sessions may differ"
                } else if self.bound {
                    "session id from the agent's own pid file"
                } else {
                    "session id supplied by the multiplexer"
                }
            )
        })
    }
    /// Each unfinished child as one group, so a consumer never splits a task.
    pub(super) fn task_rows(&self, width: usize) -> Vec<Vec<(bool, String)>> {
        let mut rows = Vec::new();
        for task in self.tasks.iter().filter(|t| !t.state.finished()) {
            let suffix = if self.guessed {
                " · session guessed"
            } else {
                ""
            };
            rows.push(
                branch(
                    &format!(
                        "{}  {} · {}{suffix}",
                        task.label,
                        task.state.words(),
                        Via::Log.words()
                    ),
                    width,
                )
                .into_iter()
                .map(|r| (task.state == Activity::Unknown, r))
                .collect(),
            );
        }
        rows
    }
    /// Wrapped unfinished child rows, in source order.
    pub fn rows(&self, width: usize) -> Vec<(bool, String)> {
        self.task_rows(width).into_iter().flatten().collect()
    }
}
#[derive(Debug, Default)]
struct Cached {
    identity: Option<(u64, u64)>,
    offset: u64,
    size: u64,
    modified: Option<SystemTime>,
    transcript: Transcript,
    error: Option<String>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Stamp {
    identity: (u64, u64),
    size: u64,
    modified: SystemTime,
}
impl Stamp {
    fn of(metadata: &fs::Metadata) -> Result<Self, String> {
        Ok(Self {
            identity: (metadata.dev(), metadata.ino()),
            size: metadata.len(),
            modified: metadata
                .modified()
                .map_err(|e| format!("rollout modification time: {e}"))?,
        })
    }
}
#[derive(Debug)]
struct Header {
    stamp: Stamp,
    parsed: Result<Option<Transcript>, String>,
}
#[derive(Debug)]
struct Indexed {
    path: PathBuf,
    header: Transcript,
    modified: SystemTime,
}
#[derive(Debug, Default)]
struct CodexIndex {
    entries: HashMap<String, Indexed>,
    roots: HashMap<String, String>,
    children: HashMap<String, Vec<String>>,
    warnings: Vec<String>,
}
#[derive(Debug)]
pub struct LogReader {
    claude: PathBuf,
    /// Every Claude config dir seen: the default, then any an agent process
    /// was found running with. Background agents are looked for in each.
    claude_roots: Vec<PathBuf>,
    codex: PathBuf,
    opencode: PathBuf,
    files: HashMap<PathBuf, Cached>,
    headers: HashMap<PathBuf, Header>,
    codex_index: Arc<CodexIndex>,
    scanned: Option<SystemTime>,
}
impl Default for LogReader {
    fn default() -> Self {
        let home = PathBuf::from(std::env::var_os("HOME").unwrap_or_default());
        let claude = std::env::var_os("CLAUDE_CONFIG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".claude"));
        let codex = std::env::var_os("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".codex"));
        let data = std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"));
        Self::at(claude, codex, data.join("opencode/opencode.db"))
    }
}
impl LogReader {
    pub fn at(claude: PathBuf, codex: PathBuf, opencode: PathBuf) -> Self {
        Self {
            claude_roots: vec![claude.clone()],
            claude,
            codex,
            opencode,
            files: HashMap::new(),
            headers: HashMap::new(),
            codex_index: Arc::new(CodexIndex::default()),
            scanned: None,
        }
    }
    /// Missing stores are not installed sources; unreadable/malformed stores are errors.
    /// An explicit ID is never replaced with a guessed one.
    pub fn read(
        &mut self,
        agent: &str,
        cwd: &str,
        session: Option<&str>,
    ) -> Result<LogWork, String> {
        let session = session.filter(|s| !s.is_empty());
        match agent {
            "claude" | "claude-code" => self.claude(cwd, session),
            "codex" => self.codex(cwd, session),
            "opencode" => self.opencode(cwd, session),
            _ => Ok(LogWork::default()),
        }
    }
    /// [`read`](Self::read) for an agent whose pid is known. A Claude agent
    /// is bound to its session through its own pid file, in the config dir
    /// the process itself runs with; the cwd guess is only the fallback,
    /// and a pid file that cannot be read says so beside the guess.
    pub fn read_agent(
        &mut self,
        agent: &str,
        pid: Option<u32>,
        cwd: &str,
        session: Option<&str>,
    ) -> Result<LogWork, String> {
        let claude = matches!(agent, "claude" | "claude-code");
        let session = session.filter(|s| !s.is_empty());
        let (Some(pid), true) = (pid, claude) else {
            return self.read(agent, cwd, session);
        };
        if let Some(id) = session {
            // The id the multiplexer reported stays the identity; the pid
            // only says which config dir its transcript is in.
            return match self.claude_binding(pid) {
                Ok(Some((root, _))) => self.claude_in(&root, &[cwd], Some(id), false),
                _ => self.read(agent, cwd, Some(id)),
            };
        }
        match self.claude_binding(pid) {
            Ok(Some((root, bound))) => {
                let cwds = [bound.cwd.as_str(), cwd];
                self.claude_in(&root, &cwds, Some(&bound.session_id), true)
            }
            // No pid file: guess by cwd, but in the config dir the process
            // itself was started with, where its environment names one.
            Ok(None) => match claude_config_dir_of(pid).filter(|root| root.is_dir()) {
                Some(root) => {
                    self.add_root(root.clone());
                    self.claude_in(&root, &[cwd], None, false)
                }
                None => self.read(agent, cwd, None),
            },
            Err(why) => self.read(agent, cwd, None).map(|mut work| {
                work.warnings.push(why);
                work
            }),
        }
    }
    /// The config dir a Claude process runs with, and its pid file there.
    /// The process's own `CLAUDE_CONFIG_DIR` where its environment can be
    /// read (Linux, same user); otherwise whichever known config dir holds
    /// a pid file for it, which is how a Mac finds it.
    fn claude_binding(&mut self, pid: u32) -> Result<Option<(PathBuf, super::ClaudeSession)>, String> {
        let mut candidates: Vec<PathBuf> = claude_root_of(pid).into_iter().collect();
        candidates.extend(self.claude_roots.iter().cloned());
        for root in candidates {
            let path = root.join("sessions").join(format!("{pid}.json"));
            let text = match fs::read_to_string(&path) {
                Ok(text) => text,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => return Err(format!("Claude pid file: {e}")),
            };
            let session = super::parse_claude_session(&text)?;
            if session.pid != pid || !super::jobs::same_process(&session) {
                // Left behind by an earlier process with this pid.
                continue;
            }
            self.add_root(root.clone());
            return Ok(Some((root, session)));
        }
        Ok(None)
    }
    /// Remember a config dir, once: a `CLAUDE_CONFIG_DIR` that is a symlink
    /// to a dir already known is the same dir, or its jobs would be drawn
    /// and counted twice.
    fn add_root(&mut self, root: PathBuf) {
        let real = |p: &Path| fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
        let new = real(&root);
        if !self.claude_roots.iter().any(|known| real(known) == new) {
            self.claude_roots.push(root);
        }
    }
    /// Every Claude config dir background agents are looked for in.
    pub fn claude_roots(&self) -> &[PathBuf] {
        &self.claude_roots
    }
    /// Claude background agents under every known config dir, plus the
    /// config dir of every Claude process in `table`: a background agent
    /// started with its own `CLAUDE_CONFIG_DIR` holds no pane that would
    /// have found that dir.
    pub fn read_jobs(&mut self, table: Option<&Result<super::Table, String>>) -> super::Jobs {
        if let Some(Ok(table)) = table {
            for (pid, _) in table.agents(&["claude"]) {
                if let Some(root) = claude_root_of(pid) {
                    if root.join("jobs").is_dir() {
                        self.add_root(root);
                    }
                }
            }
        }
        super::read_jobs(&self.claude_roots, table)
    }
    /// Incrementally parse complete records; partial bytes never become errors.
    fn transcript(&mut self, path: &Path, claude: bool) -> Result<Transcript, String> {
        let m = fs::metadata(path).map_err(|e| format!("session log metadata: {e}"))?;
        let c = self.files.entry(path.into()).or_default();
        let modified = m.modified().ok();
        let identity = Some((m.dev(), m.ino()));
        if c.identity != identity
            || m.len() < c.size
            || (m.len() == c.size && modified != c.modified)
            || (c.error.is_some() && (m.len() != c.size || modified != c.modified))
        {
            *c = Cached::default();
        }
        // Unchanged corrupt files must not replay their parsed prefix on
        // every poll. A changed file may have been repaired in place.
        if let Some(error) = &c.error {
            c.size = m.len();
            c.modified = modified;
            return Err(error.clone());
        }
        if c.size == m.len() && c.modified == modified {
            return Ok(c.transcript.clone());
        }
        let mut f = File::open(path).map_err(|e| format!("session log open: {e}"))?;
        f.seek(SeekFrom::Start(c.offset))
            .map_err(|e| format!("session log seek: {e}"))?;
        let mut reader = BufReader::new(f);
        let mut bytes = Vec::new();
        let result = loop {
            bytes.clear();
            match reader.read_until(b'\n', &mut bytes) {
                Err(e) => return Err(format!("session log read: {e}")),
                Ok(0) => break Ok(()),
                Ok(_) if bytes.last() != Some(&b'\n') => break Ok(()),
                Ok(_) => (),
            }
            let line = match std::str::from_utf8(&bytes) {
                Ok(line) => line,
                Err(e) => break Err(format!("invalid complete session log UTF-8: {e}")),
            };
            if !line.trim().is_empty() {
                let parsed = if claude {
                    parse::parse_claude_record(&mut c.transcript, line)
                } else {
                    parse::parse_codex_record(&mut c.transcript, line)
                };
                if let Err(e) = parsed {
                    break Err(e);
                }
            }
            c.offset += bytes.len() as u64;
        };
        c.identity = identity;
        c.size = m.len();
        c.modified = modified;
        if let Err(error) = result {
            c.error = Some(error.clone());
            return Err(error);
        }
        Ok(c.transcript.clone())
    }
    fn claude(&mut self, cwd: &str, session: Option<&str>) -> Result<LogWork, String> {
        let root = self.claude.clone();
        self.claude_in(&root, &[cwd], session, false)
    }
    /// The Claude session under `root`, in the first of `cwds` whose project
    /// folder holds it. A session bound from a pid file may have no
    /// transcript yet - nothing has been said in it - which is no work
    /// rather than an error.
    fn claude_in(
        &mut self,
        root: &Path,
        cwds: &[&str],
        session: Option<&str>,
        bound: bool,
    ) -> Result<LogWork, String> {
        let cwds: Vec<&str> = cwds.iter().copied().filter(|c| !c.is_empty()).collect();
        let Some(&cwd) = cwds.first() else {
            return if session.is_some() {
                Err("Claude session lookup needs the pane cwd".into())
            } else {
                Ok(LogWork::default())
            };
        };
        let encode = |cwd: &str| -> String {
            cwd.chars()
                .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
                .collect()
        };
        let mut project = root.join("projects").join(encode(cwd));
        let path = if let Some(id) = session {
            if !safe_id(id) {
                return Err("invalid Claude session id".into());
            }
            let found = cwds.iter().map(|c| root.join("projects").join(encode(c))).find(|p| {
                p.join(format!("{id}.jsonl")).exists()
            });
            match found {
                Some(p) => {
                    project = p;
                    Some(project.join(format!("{id}.jsonl")))
                }
                None if bound => {
                    return Ok(LogWork {
                        session_id: id.to_string(),
                        bound: true,
                        ..Default::default()
                    })
                }
                None => return Err("the supplied Claude session log was not found".into()),
            }
        } else {
            let paths = list(&project, false)?;
            let mut dated = Vec::new();
            for path in paths
                .into_iter()
                .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
            {
                let stamp = fs::metadata(&path)
                    .and_then(|m| m.modified())
                    .map_err(|e| format!("Claude transcript modification time: {e}"))?;
                dated.push((stamp, path));
            }
            dated.into_iter().max().map(|(_, path)| path)
        };
        let Some(path) = path else {
            return Ok(LogWork::default());
        };
        let parent = self.transcript(&path, true)?;
        let id = path
            .file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_string();
        let mut tasks = parent.agents;
        for child in list(&project.join(&id).join("subagents"), false)?
            .into_iter()
            .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
        {
            let child_id = child
                .file_stem()
                .and_then(|s| s.to_str())
                .unwrap_or("")
                .strip_prefix("agent-")
                .unwrap_or("")
                .to_string();
            if child_id.is_empty() {
                continue;
            }
            let data = self.transcript(&child, true)?;
            let task = tasks.entry(child_id.clone()).or_insert_with(|| LogTask {
                id: child_id,
                label: "subagent".into(),
                state: Activity::Unknown,
                updated: String::new(),
            });
            // A resumed child can run again after a prior completion notice.
            if data.updated > task.updated {
                task.state = data.state;
                task.updated = data.updated;
            }
        }
        Ok(LogWork {
            session_id: id,
            guessed: session.is_none(),
            bound,
            tasks: tasks.into_values().collect(),
            ..Default::default()
        })
    }
    /// Publish one archive index per interval, shared by every pane. Only
    /// the selected descendants' bodies are checked again on each read.
    fn refresh_codex_index(&mut self) -> Result<(), String> {
        let paths: Vec<_> = list(&self.codex.join("sessions"), true)?
            .into_iter()
            .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
            .collect();
        let live: HashSet<_> = paths.iter().cloned().collect();
        self.headers.retain(|p, _| live.contains(p));
        let codex = self.codex.clone();
        self.files
            .retain(|p, _| !p.starts_with(&codex) || live.contains(p));
        let mut index = CodexIndex::default();
        for path in paths {
            let metadata = match fs::metadata(&path) {
                Ok(m) => m,
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                Err(e) => {
                    index.warnings.push(format!("rollout metadata: {e}"));
                    continue;
                }
            };
            let stamp = match Stamp::of(&metadata) {
                Ok(stamp) => stamp,
                Err(error) => {
                    index.warnings.push(error);
                    continue;
                }
            };
            let cached = self.headers.get(&path).is_some_and(|h| {
                h.stamp == stamp
                    || (h.parsed.as_ref().is_ok_and(|t| t.is_some())
                        && h.stamp.identity == stamp.identity
                        && stamp.size > h.stamp.size)
            });
            if !cached {
                let file = match File::open(&path) {
                    Ok(f) => f,
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(e) => {
                        index.warnings.push(format!("rollout header open: {e}"));
                        continue;
                    }
                };
                let mut bytes = Vec::new();
                let parsed = BufReader::new(file)
                    .read_until(b'\n', &mut bytes)
                    .map_err(|e| format!("rollout header read: {e}"))
                    .and_then(|_| {
                        if bytes.last() != Some(&b'\n') {
                            return Ok(None);
                        }
                        let line = std::str::from_utf8(&bytes)
                            .map_err(|e| format!("rollout header UTF-8: {e}"))?;
                        let mut header = Transcript::default();
                        parse::parse_codex_record(&mut header, line)?;
                        if header.id.is_empty() {
                            return Err("rollout header has no session id".into());
                        }
                        Ok(Some(header))
                    });
                self.headers.insert(path.clone(), Header { stamp, parsed });
            }
            let header = match &self.headers[&path].parsed {
                Ok(Some(header)) => header.clone(),
                Ok(None) => continue, // the writer has not finished the header
                Err(error) => {
                    index.warnings.push(error.clone());
                    continue;
                }
            };
            // A reverted rollout shares the stable thread ID; retain only
            // its newest body, including deterministic modification-time ties.
            if index
                .entries
                .get(&header.id)
                .is_none_or(|old| (stamp.modified, &path) > (old.modified, &old.path))
            {
                index.entries.insert(
                    header.id.clone(),
                    Indexed {
                        path,
                        header,
                        modified: stamp.modified,
                    },
                );
            }
        }
        for (id, entry) in &index.entries {
            if let Some(parent) = &entry.header.parent {
                index
                    .children
                    .entry(parent.clone())
                    .or_default()
                    .push(id.clone());
            } else if !entry.header.cwd.is_empty() {
                let newer = index.roots.get(&entry.header.cwd).is_none_or(|old_id| {
                    let old = &index.entries[old_id];
                    (entry.modified, &entry.path) > (old.modified, &old.path)
                });
                if newer {
                    index.roots.insert(entry.header.cwd.clone(), id.clone());
                }
            }
        }
        for children in index.children.values_mut() {
            children.sort();
        }
        self.codex_index = Arc::new(index);
        self.scanned = Some(SystemTime::now());
        Ok(())
    }

    fn codex(&mut self, cwd: &str, session: Option<&str>) -> Result<LogWork, String> {
        if self
            .scanned
            .is_none_or(|s| s.elapsed().map_or(true, |d| d.as_secs() >= 2))
        {
            self.refresh_codex_index()?;
        }
        let index = Arc::clone(&self.codex_index);
        let id = session.or_else(|| index.roots.get(cwd).map(String::as_str));
        let parent = id
            .and_then(|id| index.entries.get(id))
            .filter(|e| e.header.parent.is_none());
        let Some(parent) = parent else {
            return if session.is_some() {
                Err("the supplied Codex session log was not found".into())
            } else if !index.warnings.is_empty() {
                Err(format!(
                    "{} rollout headers could not be indexed; session lookup is incomplete",
                    index.warnings.len()
                ))
            } else {
                Ok(LogWork::default())
            };
        };
        let parent_id = parent.header.id.clone();
        let mut seen = HashSet::from([parent_id.clone()]);
        let mut pending = vec![parent_id.clone()];
        let mut tasks = Vec::new();
        while let Some(id) = pending.pop() {
            for child in index.children.get(&id).into_iter().flatten() {
                if !seen.insert(child.clone()) {
                    continue;
                }
                let entry = &index.entries[child];
                let t = self.transcript(&entry.path, false)?;
                tasks.push(LogTask {
                    id: child.clone(),
                    label: t.label,
                    state: t.state,
                    updated: t.updated,
                });
                pending.push(child.clone());
            }
        }
        tasks.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(LogWork {
            session_id: parent_id,
            guessed: session.is_none(),
            bound: false,
            tasks,
            warnings: index.warnings.clone(),
        })
    }
    fn opencode(&self, cwd: &str, session: Option<&str>) -> Result<LogWork, String> {
        use rusqlite::{Connection, OpenFlags, OptionalExtension};
        match fs::metadata(&self.opencode) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                return if session.is_some() {
                    Err("the supplied OpenCode session store was not found".into())
                } else {
                    Ok(LogWork::default())
                };
            }
            Err(e) => return Err(format!("OpenCode store metadata: {e}")),
            Ok(_) => (),
        }
        let sql_error = |e| format!("OpenCode session store: {e}");
        let mut c = Connection::open_with_flags(&self.opencode, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(sql_error)?;
        c.busy_timeout(std::time::Duration::from_millis(100))
            .map_err(sql_error)?;
        let tx = c.transaction().map_err(sql_error)?;
        let v2: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE name='session_v2')",
                [],
                |r| r.get(0),
            )
            .map_err(sql_error)?;
        if !v2 {
            return Err(
                "OpenCode session store has no persisted session liveness (requires session_v2)"
                    .into(),
            );
        }
        let read = || -> rusqlite::Result<LogWork> {
            let id: Option<String> = if let Some(id) = session {
                tx.query_row(
                    "SELECT id FROM session_v2 WHERE id=?1 AND parent_id IS NULL",
                    [id],
                    |r| r.get(0),
                )
                .optional()?
            } else {
                tx.query_row("SELECT id FROM session_v2 WHERE directory=?1 AND parent_id IS NULL AND time_archived IS NULL ORDER BY time_updated DESC,id LIMIT 1",[cwd],|r|r.get(0)).optional()?
            };
            let Some(id) = id else {
                return Ok(LogWork::default());
            };
            let mut q=tx.prepare("WITH RECURSIVE children(id) AS (SELECT id FROM session_v2 WHERE parent_id=?1 UNION SELECT s.id FROM session_v2 s JOIN children c ON s.parent_id=c.id) SELECT id,coalesce(agent,'subagent'),time_busy,time_idle,time_updated,idle_outcome FROM session_v2 WHERE id IN (SELECT id FROM children)")?;
            let tasks = q
                .query_map([&id], |r| {
                    let busy: Option<i64> = r.get(2)?;
                    let idle: Option<i64> = r.get(3)?;
                    let state = if busy.is_some_and(|b| idle.is_none_or(|i| b > i)) {
                        Activity::Working
                    } else if idle.is_some() {
                        match r.get::<_, Option<String>>(5)?.as_deref() {
                            Some("failed" | "interrupted") => Activity::Error,
                            _ => Activity::Done,
                        }
                    } else {
                        Activity::Unknown
                    };
                    Ok(LogTask {
                        id: r.get(0)?,
                        label: r.get(1)?,
                        state,
                        updated: r.get::<_, i64>(4)?.to_string(),
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(LogWork {
                session_id: id,
                guessed: session.is_none(),
                tasks,
                ..Default::default()
            })
        };
        let work = read().map_err(|e| format!("OpenCode session store: {e}"))?;
        if session.is_some() && work.session_id.is_empty() {
            Err("the supplied OpenCode session was not found".into())
        } else {
            Ok(work)
        }
    }
}
fn safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}
fn list(path: &Path, recursive: bool) -> Result<Vec<PathBuf>, String> {
    let entries = match fs::read_dir(path) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(format!("session log directory: {e}")),
    };
    let mut paths = Vec::new();
    for entry in entries {
        let e = entry.map_err(|e| format!("session log entry: {e}"))?;
        let ty = e
            .file_type()
            .map_err(|e| format!("session log type: {e}"))?;
        if ty.is_dir() && recursive {
            paths.extend(list(&e.path(), true)?);
        } else if ty.is_file() {
            paths.push(e.path());
        }
    }
    Ok(paths)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            // macOS SystemTime has microsecond resolution: parallel tests can
            // otherwise share a path and overwrite/remove each other's logs.
            static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!(
                "opscope-work-{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(SystemTime::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }
        fn write(&self, path: &str, body: &str) -> PathBuf {
            let p = self.0.join(path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(&p, body).unwrap();
            p
        }
        fn reader(&self) -> LogReader {
            LogReader::at(
                self.0.join("claude"),
                self.0.join("codex"),
                self.0.join("opencode.db"),
            )
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    const LAUNCH: &str = "{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:00:01Z\",\"toolUseResult\":{\"status\":\"async_launched\",\"agentId\":\"child\"}}\n";
    #[test]
    fn claude_reads_live_child_append_partial_record_and_completion() {
        let f = Fixture::new();
        f.write("claude/projects/-srv-repo/parent.jsonl", LAUNCH);
        let child = f.write(
            "claude/projects/-srv-repo/parent/subagents/agent-child.jsonl",
            "{\"type\":\"user\",\"timestamp\":\"2026-01-01T00:00:02Z\"}\n",
        );
        let mut reader = f.reader();
        let first = reader.read("claude", "/srv/repo", Some("parent")).unwrap();
        assert!(!first.guessed);
        assert_eq!(first.tasks[0].state, Activity::Working);
        let mut out = fs::OpenOptions::new().append(true).open(child).unwrap();
        write!(
            out,
            "{{\"type\":\"assistant\",\"timestamp\":\"2026-01-01T00:00:03Z\",\"message\":"
        )
        .unwrap();
        assert_eq!(
            reader.read("claude", "/srv/repo", Some("parent")).unwrap(),
            first
        );
        writeln!(out, "{{\"stop_reason\":\"end_turn\"}}}}").unwrap();
        let done = reader.read("claude", "/srv/repo", Some("parent")).unwrap();
        assert_eq!(done.tasks[0].state, Activity::Done);
        assert!(done.rows(80).is_empty()); // shared API keeps the history, mux draws active work
        assert!(reader.read("claude", "/srv/repo", None).unwrap().guessed);
        assert!(reader.read("claude", "/srv/repo", Some("absent")).is_err());
        assert!(reader
            .read("claude", "/srv/repo", Some("../parent"))
            .is_err());
    }
    fn pid_file(pid: u32, session: &str, cwd: &str, start: &str) -> String {
        format!(
            "{{\"pid\":{pid},\"sessionId\":\"{session}\",\"cwd\":\"{cwd}\",\"procStart\":\"{start}\",\"kind\":\"interactive\",\"status\":\"busy\"}}"
        )
    }
    fn launch(id: &str) -> String {
        LAUNCH.replace("\"child\"", &format!("\"{id}\""))
    }
    #[test]
    fn two_claude_panes_in_one_cwd_are_bound_to_their_own_sessions_by_pid() {
        let f = Fixture::new();
        f.write("claude/projects/-srv-repo/one.jsonl", &launch("first"));
        f.write("claude/projects/-srv-repo/two.jsonl", &launch("second"));
        f.write("claude/sessions/900001.json", &pid_file(900001, "one", "/srv/repo", ""));
        f.write("claude/sessions/900002.json", &pid_file(900002, "two", "/srv/repo", ""));
        let mut reader = f.reader();
        let one = reader.read_agent("claude", Some(900001), "/srv/repo", None).unwrap();
        let two = reader.read_agent("claude", Some(900002), "/srv/repo", None).unwrap();
        assert_eq!((one.session_id.as_str(), one.tasks[0].id.as_str()), ("one", "first"));
        assert_eq!((two.session_id.as_str(), two.tasks[0].id.as_str()), ("two", "second"));
        for work in [&one, &two] {
            assert!(!work.guessed && work.bound);
            assert!(work.rows(80).iter().all(|(_, r)| !r.contains("guessed")));
            assert!(work.explanation().unwrap().contains("pid file"));
        }
        // No pid file: the newest transcript for the cwd, labelled a guess.
        let guess = reader.read_agent("claude", Some(900003), "/srv/repo", None).unwrap();
        assert!(guess.guessed && !guess.bound);
        // Not Claude: the pid is not consulted.
        assert!(reader.read_agent("codex", Some(900001), "/srv/repo", None).unwrap().tasks.is_empty());
    }
    #[test]
    fn a_bound_session_with_no_transcript_yet_is_no_work_not_an_error() {
        let f = Fixture::new();
        f.write("claude/sessions/900004.json", &pid_file(900004, "fresh", "/srv/repo", ""));
        let work = f.reader().read_agent("claude", Some(900004), "/srv/repo", None).unwrap();
        assert_eq!(work.session_id, "fresh");
        assert!(work.bound && work.tasks.is_empty());
    }
    #[test]
    fn a_session_in_a_worktree_is_found_from_the_pid_files_cwd() {
        let f = Fixture::new();
        f.write("claude/projects/-srv-repo-wt/s.jsonl", &launch("deep"));
        f.write("claude/sessions/900005.json", &pid_file(900005, "s", "/srv/repo/wt", ""));
        let work = f.reader().read_agent("claude", Some(900005), "/srv/repo", None).unwrap();
        assert_eq!(work.tasks[0].id, "deep");
    }
    #[test]
    fn a_corrupt_pid_file_falls_back_to_the_guess_and_says_why() {
        let f = Fixture::new();
        f.write("claude/projects/-srv-repo/one.jsonl", &launch("first"));
        f.write("claude/sessions/900006.json", "{\"pid\":");
        let work = f.reader().read_agent("claude", Some(900006), "/srv/repo", None).unwrap();
        assert!(work.guessed);
        assert!(work.warnings[0].contains("pid file"), "{:?}", work.warnings);
    }
    #[test]
    fn a_pid_file_left_by_an_earlier_process_with_that_pid_is_not_believed() {
        let f = Fixture::new();
        let me = std::process::id();
        f.write("claude/projects/-srv-repo/one.jsonl", &launch("first"));
        f.write(&format!("claude/sessions/{me}.json"), &pid_file(me, "stale", "/srv/repo", "1"));
        let work = f.reader().read_agent("claude", Some(me), "/srv/repo", None).unwrap();
        // Only where /proc gives a start time can the reuse be seen.
        if crate::procfs::read_stat(me as i32).is_ok() {
            assert!(work.guessed, "{work:?}");
        }
    }
    #[test]
    fn an_agent_with_its_own_config_dir_is_read_there() {
        let f = Fixture::new();
        let custom = f.0.join("elsewhere");
        f.write("elsewhere/projects/-srv-repo/s.jsonl", &launch("custom"));
        let mut child = std::process::Command::new("/bin/sleep")
            .arg("30")
            .env("CLAUDE_CONFIG_DIR", &custom)
            .spawn()
            .unwrap();
        let pid = child.id();
        f.write(&format!("elsewhere/sessions/{pid}.json"), &pid_file(pid, "s", "/srv/repo", ""));
        let mut reader = f.reader();
        // The environment of another process is readable only where /proc
        // is, and holds the new variable only once the child has exec'd.
        let mut readable = false;
        for _ in 0..200 {
            match crate::procfs::read_environ(pid as i32) {
                Ok(env) if env.contains("CLAUDE_CONFIG_DIR=") => {
                    readable = true;
                    break;
                }
                Ok(_) => std::thread::sleep(std::time::Duration::from_millis(10)),
                Err(_) => break,
            }
        }
        let work = reader.read_agent("claude", Some(pid), "/srv/repo", None);
        // A pane whose multiplexer reported the session id still reads it
        // in the agent's own config dir.
        let mut fresh = f.reader();
        let reported = fresh.read_agent("claude", Some(pid), "/srv/repo", Some("s"));
        // A background agent with its own config dir and no pane at all is
        // found through the process table alone.
        f.write("elsewhere/jobs/aaaa1111/state.json", include_str!("fixtures/jobs/working.json"));
        let table = Ok(super::super::parse_process_table(&format!("{pid} 1 claude claude --bg\n")).unwrap());
        let jobs = f.reader().read_jobs(Some(&table));
        // With no pid file at all, the cwd guess is made in its own dir.
        std::fs::remove_file(f.0.join(format!("elsewhere/sessions/{pid}.json"))).unwrap();
        let guessed = f.reader().read_agent("claude", Some(pid), "/srv/repo", None);
        let _ = child.kill();
        let _ = child.wait();
        if readable {
            let work = work.unwrap();
            assert_eq!(work.tasks[0].id, "custom");
            assert!(reader.claude_roots().contains(&custom));
            assert_eq!(reported.unwrap().tasks[0].id, "custom");
            assert_eq!(jobs.shown.len(), 1);
            assert_eq!(jobs.shown[0].short, "aaaa1111");
            let guessed = guessed.unwrap();
            assert_eq!(guessed.tasks[0].id, "custom");
            assert!(guessed.guessed);
        }
    }
    #[test]
    fn an_atomic_log_replacement_does_not_keep_the_previous_launch() {
        let f = Fixture::new();
        let p = f.write("claude/projects/-srv-repo/parent.jsonl", LAUNCH);
        let mut reader = f.reader();
        assert_eq!(
            reader
                .read("claude", "/srv/repo", None)
                .unwrap()
                .tasks
                .len(),
            1
        );
        let other=f.write("replacement", &("{\"type\":\"user\",\"message\":{\"content\":\"replacement with a longer body that has no subagent launches at all\"}}\n".repeat(3)));
        fs::rename(other, p).unwrap();
        assert!(reader
            .read("claude", "/srv/repo", None)
            .unwrap()
            .tasks
            .is_empty());
    }

    #[test]
    fn log_rows_fit_tiny_panes_and_a_log_error_keeps_process_work() {
        let mut work = super::super::Work::default();
        work.logs = LogWork {
            session_id: "parent".into(),
            guessed: true,
            bound: false,
            warnings: Vec::new(),
            tasks: vec![LogTask {
                id: "one".into(),
                label: "Explore".into(),
                state: Activity::Working,
                updated: String::new(),
            }],
        };
        for width in 1..=100 {
            for (_, row) in work.rows(width) {
                assert!(crate::display_width(&row) <= width, "width {width}: {row}");
            }
        }
        work.tasks.push(super::super::Task {
            pid: 7,
            label: "cargo test".into(),
            via: Via::Process,
        });
        let work = work.with_logs(Err("source unreadable".into()));
        assert!(work.rows(80).iter().any(|(_, r)| r.contains("cargo test")));
        assert!(work
            .rows(80)
            .iter()
            .any(|(_, r)| r.contains("source unreadable")));
    }

    #[test]
    fn a_partial_multibyte_character_waits_for_the_rest_of_the_record() {
        let f = Fixture::new();
        let path = f.write("claude/projects/-srv-repo/parent.jsonl", LAUNCH);
        let mut out = fs::OpenOptions::new().append(true).open(path).unwrap();
        out.write_all(b"{\"type\":\"user\",\"message\":{\"content\":\"\xf0\x9f")
            .unwrap();
        let mut reader = f.reader();
        assert_eq!(
            reader
                .read("claude", "/srv/repo", Some("parent"))
                .unwrap()
                .tasks
                .len(),
            1
        );
        out.write_all(b"\x98\x80\"}}\n").unwrap();
        assert_eq!(
            reader
                .read("claude", "/srv/repo", Some("parent"))
                .unwrap()
                .tasks
                .len(),
            1
        );
    }

    #[test]
    fn a_bad_complete_record_keeps_its_cached_failure_and_position() {
        let f = Fixture::new();
        let path = f.write(
            "claude/projects/-srv-repo/parent.jsonl",
            &(LAUNCH.to_string() + "invalid\n"),
        );
        let mut reader = f.reader();
        let first = reader
            .read("claude", "/srv/repo", Some("parent"))
            .unwrap_err();
        assert!(
            reader.files.contains_key(&path),
            "a stable error must not evict the parsed prefix"
        );
        assert_eq!(reader.files[&path].offset, LAUNCH.len() as u64);
        assert_eq!(
            reader
                .read("claude", "/srv/repo", Some("parent"))
                .unwrap_err(),
            first
        );
    }

    #[test]
    fn unrelated_bad_headers_do_not_hide_a_known_codex_child() {
        let f = Fixture::new();
        f.write("codex/sessions/root.jsonl","{\"type\":\"session_meta\",\"payload\":{\"id\":\"root\",\"cwd\":\"/srv/repo\",\"source\":\"cli\"}}\n");
        f.write("codex/sessions/child.jsonl","{\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"source\":{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"root\"}}}}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n");
        f.write("codex/sessions/unrelated.jsonl", "invalid header\n");
        let work = f.reader().read("codex", "/srv/repo", Some("root")).unwrap();
        assert_eq!(work.tasks.len(), 1);
        assert_eq!(work.tasks[0].state, Activity::Working);
    }

    #[test]
    fn codex_shares_the_archive_index_but_reads_selected_child_appends() {
        let f = Fixture::new();
        f.write("codex/sessions/root.jsonl","{\"type\":\"session_meta\",\"payload\":{\"id\":\"root\",\"cwd\":\"/srv/repo\",\"source\":\"cli\"}}\n");
        let child=f.write("codex/sessions/child.jsonl","{\"type\":\"session_meta\",\"payload\":{\"id\":\"child\",\"source\":{\"subagent\":{\"thread_spawn\":{\"parent_thread_id\":\"root\"}}}}}\n{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n");
        let bad = f.write("codex/sessions/unrelated.jsonl", "invalid header\n");
        let mut reader = f.reader();
        let first = reader.read("codex", "/srv/repo", Some("root")).unwrap();
        assert_eq!(first.warnings.len(), 1);
        assert!(super::super::Work::default()
            .with_logs(Ok(first))
            .rows(80)
            .iter()
            .any(|(warn, row)| *warn && row.contains("incomplete")));
        fs::remove_file(bad).unwrap();
        fs::OpenOptions::new()
            .append(true)
            .open(child)
            .unwrap()
            .write_all(b"{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
            .unwrap();
        // Another pane uses the same completed index, even after an
        // unrelated archive change. Its selected child is current now.
        reader.scanned = Some(SystemTime::now());
        let second = reader.read("codex", "/srv/repo", Some("root")).unwrap();
        assert_eq!(second.warnings.len(), 1);
        assert_eq!(second.tasks[0].state, Activity::Done);
        reader.scanned = None;
        assert!(reader
            .read("codex", "/srv/repo", Some("root"))
            .unwrap()
            .warnings
            .is_empty());
    }

    #[test]
    fn unsupported_opencode_schema_is_explained_without_a_sql_parameter_error() {
        let f = Fixture::new();
        rusqlite::Connection::open(f.0.join("opencode.db"))
            .unwrap()
            .execute_batch("CREATE TABLE session(id TEXT);")
            .unwrap();
        let why = f.reader().read("opencode", "/srv/repo", None).unwrap_err();
        assert!(why.contains("requires session_v2"), "{why}");
        assert!(!why.contains("Invalid parameter"), "{why}");
    }

    #[test]
    fn arbitrary_numeric_report_ids_do_not_identify_process_jobs() {
        let mut work = super::super::Work::default();
        work.tasks.push(super::super::Task {
            pid: 73,
            label: "cargo test".into(),
            via: Via::Process,
        });
        work.covered_by(&["build/73".into(), "73".into()]);
        assert_eq!(work.tasks.len(), 1);
    }

    #[test]
    fn claude_corrupt_record_is_visible_and_rewrite_recovers() {
        let f = Fixture::new();
        let path = f.write("claude/projects/-srv-repo/parent.jsonl", "invalid\n");
        let mut reader = f.reader();
        assert!(reader.read("claude", "/srv/repo", None).is_err());
        fs::write(path, LAUNCH).unwrap();
        assert_eq!(
            reader
                .read("claude", "/srv/repo", None)
                .unwrap()
                .tasks
                .len(),
            1
        );
    }
    #[test]
    fn codex_only_reads_bodies_belonging_to_the_selected_root_and_follows_descendants() {
        let f = Fixture::new();
        let meta = |id: &str, parent: Option<&str>| {
            serde_json::json!({"type":"session_meta","payload":{"id":id,"cwd":"/srv/repo","source":parent.map(|p|serde_json::json!({"subagent":{"thread_spawn":{"parent_thread_id":p,"depth":1}}})).unwrap_or(serde_json::json!("cli"))}}).to_string()+"\n"
        };
        f.write("codex/sessions/2026/01/01/root.jsonl", &meta("root", None));
        f.write(
            "codex/sessions/2026/01/01/child.jsonl",
            &(meta("child", Some("root"))
                + "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n"),
        );
        f.write(
            "codex/sessions/2026/01/01/grandchild.jsonl",
            &(meta("grandchild", Some("child"))
                + "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n"),
        );
        f.write(
            "codex/sessions/2026/01/01/unrelated.jsonl",
            &(meta("unrelated", Some("elsewhere")) + "a corrupt unrelated body is never read\n"),
        );
        let mut reader = f.reader();
        let work = reader.read("codex", "/srv/repo", Some("root")).unwrap();
        assert_eq!(work.tasks.len(), 2);
        assert!(work
            .tasks
            .iter()
            .any(|t| t.id == "child" && t.state == Activity::Working));
        assert!(work
            .tasks
            .iter()
            .any(|t| t.id == "grandchild" && t.state == Activity::Done));
        assert_eq!(reader.files.len(), 2);
        assert!(reader.read("codex", "/srv/repo", Some("absent")).is_err());
        let replacement = f.write(
            "codex/sessions/2026/01/02/child-reverted.jsonl",
            &(meta("child", Some("root"))
                + "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n"),
        );
        File::open(replacement)
            .unwrap()
            .set_times(
                std::fs::FileTimes::new()
                    .set_modified(SystemTime::now() + std::time::Duration::from_secs(2)),
            )
            .unwrap();
        reader.scanned = None;
        let reverted = reader.read("codex", "/srv/repo", Some("root")).unwrap();
        assert_eq!(reverted.tasks.len(), 2);
        assert!(reverted
            .tasks
            .iter()
            .any(|t| t.id == "child" && t.state == Activity::Done));
    }
    #[test]
    fn opencode_reads_busy_children_including_claimed_work_and_grandchildren() {
        let f = Fixture::new();
        let db = rusqlite::Connection::open(f.0.join("opencode.db")).unwrap();
        db.execute_batch("CREATE TABLE session_v2(id TEXT PRIMARY KEY,parent_id TEXT,directory TEXT,agent TEXT,time_busy INTEGER,time_idle INTEGER,time_suspended INTEGER,time_updated INTEGER,time_archived INTEGER,idle_outcome TEXT);
        INSERT INTO session_v2 VALUES('root',NULL,'/srv/repo','build',1,2,NULL,1,NULL,'succeeded'),('child','root','/srv/repo','explore',3,2,3,3,NULL,NULL),('nested','child','/srv/repo','general',1,2,NULL,2,NULL,'succeeded'),('other','elsewhere','/srv/repo','general',3,2,NULL,3,NULL,NULL);").unwrap();
        let mut reader = f.reader();
        let work = reader.read("opencode", "/srv/repo", Some("root")).unwrap();
        assert_eq!(work.tasks.len(), 2);
        assert!(work
            .tasks
            .iter()
            .any(|t| t.id == "child" && t.state == Activity::Working));
        assert!(work
            .tasks
            .iter()
            .any(|t| t.id == "nested" && t.state == Activity::Done));
        db.execute(
            "UPDATE session_v2 SET time_idle=4,idle_outcome='failed' WHERE id='child'",
            [],
        )
        .unwrap();
        assert!(reader
            .read("opencode", "/srv/repo", Some("root"))
            .unwrap()
            .tasks
            .iter()
            .all(|t| t.state.finished()));
        assert!(reader
            .read("opencode", "/srv/repo", Some("root"))
            .unwrap()
            .tasks
            .iter()
            .any(|t| t.id == "child" && t.state == Activity::Error));
        assert!(reader
            .read("opencode", "/srv/repo", Some("absent"))
            .is_err());
    }
    #[test]
    fn missing_is_distinct_from_an_unreadable_or_unsupported_store() {
        let f = Fixture::new();
        let mut reader = f.reader();
        assert_eq!(
            reader.read("opencode", "/srv/repo", None).unwrap(),
            LogWork::default()
        );
        f.write("opencode.db", "not sqlite");
        assert!(reader.read("opencode", "/srv/repo", None).is_err());
    }
    #[test]
    fn reports_remove_only_identified_work_not_other_agents_with_the_same_label() {
        let mut work = super::super::Work::default();
        work.logs.tasks = vec![
            LogTask {
                id: "one".into(),
                label: "Explore".into(),
                state: Activity::Working,
                updated: String::new(),
            },
            LogTask {
                id: "two".into(),
                label: "Explore".into(),
                state: Activity::Working,
                updated: String::new(),
            },
        ];
        work.covered_by(&["agent/one".into()]);
        assert_eq!(work.logs.tasks.len(), 1);
        assert_eq!(work.logs.tasks[0].id, "two");
        let rows = work.logs.rows(80);
        assert!(rows[0].1.contains("via log"));
    }
}
