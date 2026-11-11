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
        // The candidates are the first processes below the shells; the
        // agent is looked for below those too, since a launcher (`npm exec
        // claude`) is not the agent but runs it.
        let mut found = Vec::new();
        let mut named = None;
        let mut queue = vec![(root, true)];
        let mut seen = 0;
        while let Some((pid, under_shells)) = queue.pop() {
            seen += 1;
            if seen > 256 {
                break;
            }
            for &kid in self.kids(pid) {
                let Some(k) = self.get(kid) else { continue };
                let shell = is_shell(&k.comm, &k.args);
                if !shell && named.is_none_or(|n| kid < n) && is_named(&k.comm, &k.args, name) {
                    named = Some(kid);
                }
                if under_shells && !shell {
                    found.push(kid);
                }
                queue.push((kid, under_shells && shell));
            }
        }
        match (named, found.as_slice()) {
            (Some(pid), _) => Some(self.innermost(pid, name)),
            (None, [only]) => Some(*only),
            _ => None,
        }
    }

    /// The agent `name` that a multiplexer's foreground `pid` belongs to.
    ///
    /// Herdr reports a pane's foreground process group, and the first in it
    /// need not be the agent: a launcher (`npx`) runs it as a child, and a
    /// job the agent ran in the foreground sits below it, often behind its
    /// shell. So the agent is looked for at `pid`, then among its ancestors,
    /// then below it; where no process carries the name, `pid` is taken.
    pub fn agent_at(&self, pid: u32, name: &str) -> u32 {
        let named = |pid: u32| self.get(pid).is_some_and(|p| is_named(&p.comm, &p.args, name));
        let mut up = pid;
        for _ in 0..8 {
            if named(up) {
                return self.innermost(up, name);
            }
            match self.get(up) {
                // Shells are walked through too: a foreground job runs in
                // the agent's own shell, and only a named process is taken,
                // so walking past the pane's shell finds nothing wrong.
                Some(p) if p.ppid > 1 => up = p.ppid,
                _ => break,
            }
        }
        let mut queue = self.kids(pid).to_vec();
        let mut seen = 0;
        while let Some(kid) = queue.pop() {
            seen += 1;
            if seen > 64 {
                break;
            }
            if named(kid) {
                return self.innermost(kid, name);
            }
            queue.extend_from_slice(self.kids(kid));
        }
        pid
    }

    /// The innermost process of that name under `pid`: a wrapper often
    /// runs the real agent under the same name (`node …/codex.js` runs the
    /// native `codex`), and the agent's shells hang off the inner one.
    fn innermost(&self, pid: u32, name: &str) -> u32 {
        let mut at = pid;
        for _ in 0..8 {
            let inner = self.kids(at).iter().copied().find(|&kid| {
                self.get(kid).is_some_and(|k| is_named(&k.comm, &k.args, name))
            });
            match inner {
                Some(kid) => at = kid,
                None => break,
            }
        }
        at
    }

    /// The shells `agent` started that are still running something.
    ///
    /// Only shells count. An agent's MCP servers, language servers and its
    /// own helpers (`rg`) are its direct children too, but they are started
    /// as themselves; a command the agent ran for you goes through a shell.
    /// A shell with nothing under it is a job that is ending or a builtin,
    /// and it is left out rather than drawn as `bash -c …`.
    ///
    /// A shell that execs a lone command leaves only the command, a direct
    /// child the table cannot tell from an MCP server. It is left out: a
    /// missed job is a gap, but every server drawn as running work would be
    /// wrong on every agent. The agent's own records and logs fill the gap.
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
    let name = product(&name);
    let matches = |word: &str| {
        let word = base_name(word).to_ascii_lowercase();
        let word = word.strip_suffix(".js").unwrap_or(&word);
        product(word) == name
    };
    let mut words = args.split_whitespace();
    let program = words.next().unwrap_or("");
    // The script is the name only under an interpreter: `node …/claude.js`
    // is Claude, but `tail /tmp/claude` and `rg claude` are not.
    let script = words.next().filter(|_| INTERPRETERS.contains(&base_name(program)));
    matches(comm) || matches(program) || script.is_some_and(matches)
}

/// A name with the suffix products and their commands differ by taken
/// off, so `cursor` is `cursor-agent`, `claude-code` is `claude` and
/// `gemini-cli` is `gemini`, whichever side a board reports.
fn product(name: &str) -> &str {
    ["-agent", "-code", "-cli"]
        .iter()
        .find_map(|s| name.strip_suffix(s).filter(|n| !n.is_empty()))
        .unwrap_or(name)
}

/// Programs that run a script, so the script names what is running.
const INTERPRETERS: &[&str] = &["node", "python", "python3", "ruby", "perl", "bun", "deno"];

fn base_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

/// Whether a process is a shell, read off `comm` and the first word of the
/// command line, with a login shell's leading `-` dropped. Rex's prompt
/// detection asks the same question through this list.
pub fn is_shell(comm: &str, args: &str) -> bool {
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
///
/// The pane gets screenshotted, so anything that reads as a credential is
/// drawn as `***`, script arguments included: see [`redact`].
pub fn label(args: &str, comm: &str) -> String {
    let words: Vec<&str> = args.split_whitespace().collect();
    let Some(first) = words.first() else {
        return base_name(comm).to_string();
    };
    let program = base_name(first);
    if INTERPRETERS.contains(&program) {
        if let Some(script) = words.get(1).filter(|w| !w.starts_with('-')) {
            let mut out = vec![base_name(script)];
            out.extend_from_slice(&words[2..]);
            return redact(&out).join(" ");
        }
    }
    let mut out = vec![program];
    out.extend_from_slice(&words[1..]);
    redact(&out).join(" ")
}

/// A command line's words with every credential in them replaced by `***`:
/// the login in a URL (`https://***@host`) and a query parameter named for
/// a secret (`?token=***`), the value of a flag or a
/// variable whose name says secret (`--token ***`, `GH_TOKEN=***`), curl's
/// `-u ***`, and everything after a header named for one
/// (`'Authorization: ***`). Words are what `ps` hands over, quotes and all,
/// so this reads names rather than parsing a shell; it would rather hide a
/// file name after `--key` than show a token after a flag it did not know.
fn redact(words: &[&str]) -> Vec<String> {
    const HIDDEN: &str = "***";
    let secret = |name: &str| {
        let name = name.trim_matches(|c: char| c == '-' || c == '\'' || c == '"').to_ascii_lowercase();
        ["token", "secret", "passw", "key", "auth", "credential", "cookie"]
            .iter()
            .any(|w| name.contains(w))
    };
    let mut out = Vec::with_capacity(words.len());
    let mut hide_next = false;
    let mut hide_rest = false;
    for word in words {
        if hide_rest || hide_next {
            hide_next = false;
            out.push(HIDDEN.to_string());
            continue;
        }
        // Whichever of `=` and `:` comes first says what the word is: an
        // assignment (`KEY=a:b`) or a header (`X-Api-Key:abc==`).
        let assigned = word
            .split_once('=')
            .map(|(name, _)| name)
            .filter(|n| !n.contains(':') && secret(n));
        if let Some(name) = assigned {
            out.push(format!("{name}={HIDDEN}"));
        } else if let Some(at) = word.find("://") {
            let rest = &word[at + 3..];
            let host_end = rest.find(['/', '?', '#']).unwrap_or(rest.len());
            let url = match rest[..host_end].rfind('@') {
                Some(login) => format!("{}{HIDDEN}{}", &word[..at + 3], &rest[login..]),
                None => word.to_string(),
            };
            // A query parameter named for a secret (`?token=…`) is one too.
            let url = match url.split_once('?') {
                Some((base, query)) => {
                    let query: Vec<String> = query
                        .split('&')
                        .map(|pair| match pair.split_once('=') {
                            Some((name, _)) if secret(name) => format!("{name}={HIDDEN}"),
                            _ => pair.to_string(),
                        })
                        .collect();
                    format!("{base}?{}", query.join("&"))
                }
                None => url,
            };
            out.push(url);
        } else if let Some((name, value)) = word.split_once(':').filter(|(n, _)| !n.contains('=') && secret(n)) {
            // A header, `Authorization: Bearer x` or `X-Api-Key:x`. Its value
            // can run on past this word (`Authorization:Bearer x`), so the
            // words after it go too.
            if value.trim_matches(['\'', '"']).is_empty() {
                out.push(word.to_string());
            } else {
                out.push(format!("{name}:{HIDDEN}"));
            }
            hide_rest = true;
        } else {
            hide_next = word.starts_with('-') && (secret(word) || *word == "-u" || *word == "--user");
            out.push(word.to_string());
        }
    }
    out
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
    fn the_agent_is_found_from_a_launcher_or_a_job_in_the_foreground() {
        let ps = "100 1 zsh -zsh\n110 100 npm npm exec claude\n120 110 2.1.317 claude\n\
                  130 120 cargo cargo build\n140 120 bash bash -c x\n141 140 sleep sleep 9\n";
        let table = parse_process_table(ps).expect("table");
        // The launcher is in the foreground: the agent is below it.
        assert_eq!(table.agent_at(110, "claude"), 120);
        // A job the agent runs in the foreground: the agent is above it.
        assert_eq!(table.agent_at(130, "claude"), 120);
        assert_eq!(table.agent_at(120, "claude"), 120);
        // A job behind the agent's shell: the shell is walked through.
        assert_eq!(table.agent_at(141, "claude"), 120);
        // Nothing named: the reported pid stands.
        assert_eq!(table.agent_at(130, "codex"), 130);
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
        // A command that only mentions the agent is not the agent.
        let ps = "100 1 zsh -zsh\n110 100 tail tail /tmp/claude\n120 100 2.1.317 claude\n";
        let mention = parse_process_table(ps).expect("table");
        assert_eq!(mention.agent_below(100, "claude"), Some(120));
        // A launcher between the shell and the agent is looked through.
        let ps = "100 1 zsh -zsh\n110 100 npm npm exec claude\n120 110 2.1.317 claude\n\
                  130 120 bash bash -c x\n131 130 cargo cargo build\n";
        let launched = parse_process_table(ps).expect("table");
        assert_eq!(launched.agent_below(100, "claude"), Some(120));
        // A wrapper of the same name runs the native agent: the inner one
        // owns the shells.
        let ps = "100 1 zsh -zsh\n110 100 node node /opt/codex/bin/codex.js\n\
                  120 110 codex /opt/codex/bin/codex\n130 120 bash bash -c x\n131 130 cargo cargo build\n";
        let wrapped = parse_process_table(ps).expect("table");
        assert_eq!(wrapped.agent_below(100, "codex"), Some(120));
        assert_eq!(wrapped.agent_at(110, "codex"), 120);
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

    // Herdr calls Cursor's agent `cursor` and its process is `cursor-agent`;
    // a product name and its command are the same agent either way round.
    #[test]
    fn a_product_name_finds_its_command_and_a_command_its_product() {
        assert!(is_named("cursor-agent", "cursor-agent", "cursor"));
        assert!(is_named("cursor", "/usr/bin/cursor", "cursor-agent"));
        assert!(is_named("2.1.317", "claude", "claude-code"));
        assert!(is_named("gemini", "node /x/bin/gemini.js", "gemini-cli"));
        assert!(!is_named("cursor", "cursor .", "claude"));
    }

    // The pane gets screenshotted: a credential on a job's command line is
    // drawn as `***`, and the rest of the line is left as it was.
    #[test]
    fn a_credential_on_a_job_command_line_is_never_drawn() {
        let cases = [
            ("git clone https://me:hunter2@example.com/r.git", "git clone https://***@example.com/r.git"),
            ("deploy --token abc123 --prod", "deploy --token *** --prod"),
            ("deploy --api-key=abc123", "deploy --api-key=***"),
            ("env GH_TOKEN=abc123 gh pr list", "env GH_TOKEN=*** gh pr list"),
            ("curl -u me:hunter2 https://example.com", "curl -u *** https://example.com"),
            ("curl -H 'Authorization: Bearer abc123' https://x", "curl -H 'Authorization: *** *** ***"),
            ("node /opt/bin/deploy.js --token abc123", "deploy.js --token ***"),
            ("curl https://example.com/x?page=2&access_token=abc123", "curl https://example.com/x?page=2&access_token=***"),
            ("curl -H Authorization:Bearer abc123 https://x", "curl -H Authorization:*** *** ***"),
            ("curl -H X-Api-Key:abc123 https://x", "curl -H X-Api-Key:*** ***"),
            ("curl -H X-Api-Key:YWJj== https://x", "curl -H X-Api-Key:*** ***"),
            ("env API_KEY=a:b run", "env API_KEY=*** run"),
            ("npm test -- --watch", "npm test -- --watch"),
            ("vite --port 5173 http://localhost:5173/", "vite --port 5173 http://localhost:5173/"),
        ];
        for (args, want) in cases {
            assert_eq!(label(args, ""), want, "{args}");
        }
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
