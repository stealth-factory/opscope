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

//! Work a coding agent left running, for any widget that lists agents.
//!
//! An agent that hands a job to a background shell and comes back to its
//! prompt reports `idle` or `done`, and so does the multiplexer, while the
//! job is still running. The best source for that work is the agent itself,
//! through OSC 7501 child records; a widget that has those (Rex carries
//! them) draws them and does not ask here. Where an agent reports none, the
//! next source is the process table: a shell the agent started that is
//! still running something.
//!
//! What the process table cannot see is a subagent. Those run inside the
//! agent's own process, so they leave no process behind; they are left to
//! the agent's own records and, later, its session logs (OPS-271).
//!
//! The parser reads `ps -axww -o pid=,ppid=,comm=,args=` and compiles and is
//! tested on every target; [`read_process_table`] is the one call that
//! spawns it. Linux and macOS both ship `ps`.

use std::collections::HashMap;

/// The `ps` invocation whose output [`parse_process_table`] reads.
pub const PS_ARGS: &[&str] = &["ps", "-axww", "-o", "pid=,ppid=,comm=,args="];

/// Read the machine's process table, once, for every agent a frame shows.
pub fn read_process_table() -> Result<Table, String> {
    parse_process_table(&crate::run(PS_ARGS, 5)?)
}

/// One process, as `ps` named it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Proc {
    pub ppid: u32,
    /// The executable's name: `comm`, which macOS gives as a full path.
    pub comm: String,
    /// The whole command line.
    pub args: String,
}

/// Every process on the machine, by pid, with its children indexed.
#[derive(Clone, Debug, Default)]
pub struct Table {
    procs: HashMap<u32, Proc>,
    children: HashMap<u32, Vec<u32>>,
}

/// Where a row under an agent came from, said on the row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Via {
    /// A shell the agent started, found in the process table.
    Process,
}

impl Via {
    pub fn words(self) -> &'static str {
        match self {
            Via::Process => "via process",
        }
    }
}

/// One piece of work running under an agent.
#[derive(Clone, Debug, PartialEq)]
pub struct Task {
    pub pid: u32,
    /// What is running, as a person would name it: `npm test`.
    pub label: String,
    pub via: Via,
}

impl Task {
    /// The rows that draw this job under its agent in `width` columns:
    /// `   ├ npm test  running · via process`, wrapped rather than clipped
    /// so a long command keeps the arguments that tell it apart.
    pub fn rows(&self, width: usize) -> Vec<String> {
        branch(&format!("{}  running · {}", self.label, self.via.words()), width)
    }
}

/// What an agent left running, or why that could not be read: the rows a
/// widget draws under the agent. An unread table is said there, beside the
/// agent, so an agent with nothing under it is never mistaken for one whose
/// work could not be looked at.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Work {
    pub tasks: Vec<Task>,
    /// Why the process table could not be read, when it could not.
    pub unread: Option<String>,
}

impl Work {
    /// The work under `agent`, read from a table that may have failed.
    pub fn of(table: &Result<Table, String>, agent: Option<u32>) -> Self {
        match table {
            Ok(table) => Work {
                tasks: agent.map(|pid| table.background_shells(pid)).unwrap_or_default(),
                unread: None,
            },
            Err(why) => Work {
                tasks: Vec::new(),
                unread: Some(why.clone()),
            },
        }
    }

    /// The rows under the agent in `width` columns, each with whether it is
    /// the unread explanation (drawn dim) rather than a job.
    pub fn rows(&self, width: usize) -> Vec<(bool, String)> {
        let mut out: Vec<(bool, String)> = self
            .tasks
            .iter()
            .flat_map(|job| job.rows(width))
            .map(|row| (false, row))
            .collect();
        if let Some(why) = &self.unread {
            let text = format!("background work could not be read: {why}");
            out.extend(branch(&text, width).into_iter().map(|row| (true, row)));
        }
        out
    }
}

/// `text` wrapped under a `├` branch in `width` columns.
fn branch(text: &str, width: usize) -> Vec<String> {
    crate::wrap_words(text, width.saturating_sub(5).max(1))
        .into_iter()
        .enumerate()
        .map(|(i, line)| format!("   {} {line}", if i == 0 { "├" } else { " " }))
        .collect()
}

/// Read `ps -axww -o pid=,ppid=,comm=,args=`.
///
/// `comm` is taken as one field, so a name with a space in it shifts the
/// command line by a word; the label is read from `args`, which keeps the
/// words in order whatever `comm` did.
pub fn parse_process_table(text: &str) -> Result<Table, String> {
    let mut table = Table::default();
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let mut fields = line.split_whitespace();
        let pid = fields
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .filter(|&pid| pid > 0)
            .ok_or("process table has an invalid pid")?;
        let ppid = fields
            .next()
            .and_then(|s| s.parse::<u32>().ok())
            .ok_or("process table has an invalid parent pid")?;
        let comm = fields
            .next()
            .ok_or("process table has a process with no name")?
            .to_string();
        let args = fields.collect::<Vec<_>>().join(" ");
        table.children.entry(ppid).or_default().push(pid);
        table.procs.insert(pid, Proc { ppid, comm, args });
    }
    if table.procs.is_empty() {
        return Err("process table is empty".into());
    }
    for kids in table.children.values_mut() {
        kids.sort_unstable();
    }
    Ok(table)
}

impl Table {
    pub fn get(&self, pid: u32) -> Option<&Proc> {
        self.procs.get(&pid)
    }

    fn kids(&self, pid: u32) -> &[u32] {
        self.children.get(&pid).map(Vec::as_slice).unwrap_or(&[])
    }

    /// The agent under a pane's root process: the root itself when it is not
    /// a shell, otherwise the process below it that is the agent `name`
    /// (`claude`, `codex`).
    ///
    /// Luvus names only the pane's root process, which is the shell the
    /// pane was opened with; the agent is somewhere below it, and so may be
    /// a job the shell started earlier (`sleep 600 &`). The agent is told
    /// from those by its name. Where no process carries the name, a lone
    /// process that is not a shell is taken; more than one is a guess, and
    /// nothing is taken rather than the wrong one.
    pub fn agent_below(&self, root: u32, name: &str) -> Option<u32> {
        let p = self.get(root)?;
        if !is_shell(&p.comm, &p.args) {
            return Some(root);
        }
        let mut found = Vec::new();
        let mut queue = vec![root];
        let mut seen = 0;
        while let Some(pid) = queue.pop() {
            seen += 1;
            if seen > 256 {
                break;
            }
            for &kid in self.kids(pid) {
                let Some(k) = self.get(kid) else { continue };
                if is_shell(&k.comm, &k.args) {
                    queue.push(kid);
                } else {
                    found.push(kid);
                }
            }
        }
        found.sort_unstable();
        let named = found.iter().copied().find(|&pid| {
            let p = &self.procs[&pid];
            is_named(&p.comm, &p.args, name)
        });
        match (named, found.as_slice()) {
            (Some(pid), _) => Some(pid),
            (None, [only]) => Some(*only),
            _ => None,
        }
    }

    /// The shells `agent` started that are still running something.
    ///
    /// Only shells count. An agent's MCP servers, language servers and its
    /// own helpers (`rg`) are its direct children too, but they are started
    /// as themselves; a command the agent ran for you goes through a shell.
    /// A shell with nothing under it is a job that is ending or a builtin,
    /// and it is left out rather than drawn as `bash -c …`.
    pub fn background_shells(&self, agent: u32) -> Vec<Task> {
        let mut tasks = Vec::new();
        for &shell in self.kids(agent) {
            let Some(p) = self.get(shell) else { continue };
            if !is_shell(&p.comm, &p.args) {
                continue;
            }
            if let Some(job) = self.first_job(shell) {
                tasks.push(Task {
                    pid: job,
                    label: label(&self.procs[&job].args, &self.procs[&job].comm),
                    via: Via::Process,
                });
            }
        }
        tasks
    }

    /// The first process under `shell` that is not itself a shell.
    fn first_job(&self, shell: u32) -> Option<u32> {
        let mut queue = self.kids(shell).to_vec();
        let mut seen = 0;
        while let Some(pid) = queue.first().copied() {
            queue.remove(0);
            seen += 1;
            if seen > 64 {
                return None;
            }
            let p = self.get(pid)?;
            if !is_shell(&p.comm, &p.args) {
                return Some(pid);
            }
            queue.extend_from_slice(self.kids(pid));
        }
        None
    }
}

/// Whether a process is the program `name`: by `comm`, by the program on
/// its command line, or by the script an interpreter runs. Claude Code's
/// `comm` is its version (`2.1.317`), but its command line starts `claude`.
fn is_named(comm: &str, args: &str, name: &str) -> bool {
    let name = name.trim().to_ascii_lowercase();
    if name.is_empty() {
        return false;
    }
    let matches = |word: &str| {
        let word = base_name(word).to_ascii_lowercase();
        word == name || word.strip_suffix(".js").is_some_and(|w| w == name)
    };
    let mut words = args.split_whitespace();
    matches(comm) || words.next().is_some_and(matches) || words.next().is_some_and(matches)
}

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// The same shells `rex_parse::is_shell` knows, read off `comm` and the
/// first word of the command line.
fn is_shell(comm: &str, args: &str) -> bool {
    const SHELLS: &[&str] = &[
        "sh", "bash", "zsh", "fish", "dash", "ksh", "mksh", "tcsh", "csh", "nu", "elvish", "xonsh",
        "pwsh", "ash", "yash", "oksh",
    ];
    let first = args.split_whitespace().next().unwrap_or("");
    let first = base_name(first.trim_start_matches('-'));
    let comm = base_name(comm.trim_start_matches('-'));
    SHELLS.contains(&comm) || SHELLS.contains(&first)
}

/// What a person would call a job: its command line with the program's
/// directory dropped, so `/usr/bin/tail -f x.log` reads `tail -f x.log`.
///
/// An interpreter running a script is named by the script, the way the
/// pane rows name one: `node /…/bin/npm test` reads `npm test`.
pub fn label(args: &str, comm: &str) -> String {
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(first) = words.first() else {
        return base_name(comm).to_string();
    };
    let program = base_name(first);
    const INTERPRETERS: &[&str] = &["node", "python", "python3", "ruby", "perl", "bun", "deno"];
    if INTERPRETERS.contains(&program) {
        if let Some(script) = words.get(1).filter(|w| !w.starts_with('-')) {
            let mut out = vec![base_name(script)];
            out.extend_from_slice(&words[2..]);
            return out.join(" ");
        }
    }
    let mut out = vec![program];
    out.extend_from_slice(&words[1..]);
    out.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    // An agent (claude) with an MCP server, a bundled rg, a background
    // `npm test` behind Claude's shell wrapper, and a shell whose job has
    // already gone; beside it an unrelated login shell.
    const PS: &str = "\
    1     0 launchd /sbin/launchd
  100     1 -zsh -zsh
  200   100 2.1.317 claude
  210   200 node node /home/u/.npm/_npx/x/bin/mcp-server-github
  211   200 rg /home/u/.local/share/claude/rg --files
  220   200 bash /bin/bash -c -l eval 'npm test' < /dev/null
  221   220 node node /usr/lib/node_modules/npm/bin/npm-cli.js test
  230   200 zsh /bin/zsh -c sleep-wrapper
  300     1 bash -bash
  301   300 vim vim notes.md
";

    #[test]
    fn a_shell_the_agent_started_is_its_background_work_and_its_servers_are_not() {
        let table = parse_process_table(PS).expect("table");
        let tasks = table.background_shells(200);
        assert_eq!(
            tasks,
            vec![Task {
                pid: 221,
                label: "npm-cli.js test".into(),
                via: Via::Process
            }]
        );
    }

    #[test]
    fn the_agent_is_found_under_the_shell_a_pane_was_opened_with() {
        let table = parse_process_table(PS).expect("table");
        assert_eq!(table.agent_below(100, "claude"), Some(200));
        // A root that is not a shell is the agent itself.
        assert_eq!(table.agent_below(200, "claude"), Some(200));
        // A shell with nothing under it has no agent.
        assert_eq!(table.agent_below(230, "claude"), None);
    }

    #[test]
    fn a_job_the_pane_shell_started_first_is_not_taken_for_the_agent() {
        // `sleep 600 &` typed before `claude`: the sleep has the lower pid.
        let ps = "100 1 zsh -zsh\n110 100 sleep sleep 600\n120 100 2.1.317 claude\n";
        let table = parse_process_table(ps).expect("table");
        assert_eq!(table.agent_below(100, "claude"), Some(120));
        // An agent run by node is named by its script.
        let ps = "100 1 zsh -zsh\n110 100 sleep sleep 600\n120 100 node node /x/bin/codex.js\n";
        let table = parse_process_table(ps).expect("table");
        assert_eq!(table.agent_below(100, "codex"), Some(120));
        // Two candidates and neither named: no guess.
        assert_eq!(table.agent_below(100, "opencode"), None);
        // One candidate is the agent whatever it is called.
        let ps = "100 1 zsh -zsh\n120 100 2.1.317 claude\n";
        let table = parse_process_table(ps).expect("table");
        assert_eq!(table.agent_below(100, "Claude Code"), Some(120));
    }

    #[test]
    fn a_job_is_named_without_its_directory_and_a_script_by_its_script() {
        assert_eq!(label("/usr/bin/tail -f /var/log/x.log", "tail"), "tail -f /var/log/x.log");
        assert_eq!(label("node /opt/bin/vite --port 5173", "node"), "vite --port 5173");
        assert_eq!(label("python3 -m http.server", "python3"), "python3 -m http.server");
        assert_eq!(label("", "/usr/bin/sleep"), "sleep");
    }

    #[test]
    fn an_unreadable_table_is_an_error_and_not_an_agent_with_nothing_running() {
        assert!(parse_process_table("").is_err());
        assert!(parse_process_table("abc 1 x x").is_err());
        assert!(parse_process_table("10 zz x x").is_err());
    }

    #[test]
    fn a_long_job_wraps_under_its_branch_instead_of_being_cut() {
        let job = Task {
            pid: 1,
            label: "cargo test --workspace --all-features".into(),
            via: Via::Process,
        };
        let rows = job.rows(30);
        assert!(rows.len() > 1);
        assert!(rows[0].starts_with("   ├ cargo"));
        assert!(rows[1..].iter().all(|r| r.starts_with("     ")));
        assert!(rows.iter().all(|r| crate::display_width(r) <= 30));
        assert!(rows.last().unwrap().ends_with("via process"));
    }

    #[test]
    fn an_unread_table_is_said_under_the_agent_and_not_drawn_as_no_work() {
        let work = Work::of(&Err("ps timed out".into()), Some(200));
        let rows = work.rows(60);
        assert_eq!(rows.len(), 1);
        assert!(rows[0].0);
        assert!(rows[0].1.contains("could not be read: ps timed out"));
        let table = parse_process_table(PS);
        assert_eq!(Work::of(&table, Some(200)).rows(60).len(), 1);
        assert!(Work::of(&table, None).rows(60).is_empty());
    }

    #[test]
    fn a_job_nested_under_two_shells_is_still_found() {
        let ps = "200 1 claude claude\n220 200 bash bash -c x\n221 220 sh sh -c y\n222 221 cargo cargo build\n";
        let table = parse_process_table(ps).expect("table");
        assert_eq!(table.background_shells(200)[0].label, "cargo build");
    }
}
