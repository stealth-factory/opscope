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

//! Antigravity: its conversation databases and the Code Assist quota.
//!
//! Nothing here is a token count, because Antigravity records none. The
//! conversation stores hold steps the agent took, the history file holds
//! prompts typed, and the only percentages on the tab come from a language
//! server that exists in memory while the agent runs and is gone after.

use std::collections::HashSet;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags};
use opscope_core as tc;

use crate::shared::*;
use crate::*;

/// Where the CLI keeps everything: its token, its conversations and the
/// prompt history. Also the only proof the agent is installed, since it is
/// launched by the IDE and puts no binary on PATH.
fn antigravity_dir() -> String {
    under_home(".gemini/antigravity-cli")
}

fn token_path() -> String {
    format!("{}/antigravity-oauth-token", antigravity_dir())
}

const CODE_ASSIST_API: &str = "https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist";

/// How long to wait for a started `agy` to serve its quota.
///
/// Measured at a few seconds on this machine: the ports open immediately
/// and answer nothing, and the quota service comes up behind them. So the
/// wait is on an endpoint that parses, never on a port that exists.
const AGY_READY: f64 = 25.0;
/// A started `agy` is expensive next to reading a socket, so its answer is
/// held far longer than the local probe's.
const AGY_TTL: f64 = 900.0;

/// Where the CLI might be, in the order CodexBar looks.
fn agy_path() -> Option<String> {
    if let Ok(named) = std::env::var("ANTIGRAVITY_CLI_PATH") {
        if !named.is_empty() && std::path::Path::new(&named).exists() {
            return Some(named);
        }
    }
    let mut seen = Vec::new();
    if let Ok(path) = std::env::var("PATH") {
        seen.extend(path.split(':').map(|dir| format!("{}/agy", dir)));
    }
    seen.push(under_home(".local/bin/agy"));
    seen.push("/opt/homebrew/bin/agy".into());
    seen.push("/usr/local/bin/agy".into());
    seen.into_iter().find(|p| std::path::Path::new(p).exists())
}

/// Every listening TCP port belonging to `pid` or anything it started.
///
/// Descendants matter: the port that answers is not always the process that
/// was launched. On this machine the quota came from a child, so a search
/// scoped to the pid alone finds two ports that answer nothing and gives up.
fn ports_under(pid: i32) -> Vec<u16> {
    let mut family = vec![pid.to_string()];
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let child = entry.file_name().to_string_lossy().to_string();
        if !child.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", child)) else {
            continue;
        };
        // The command name sits in brackets and may itself contain spaces,
        // so the fields after it are found from the last ')' rather than by
        // splitting the whole line.
        let Some(after) = stat.rsplit_once(')') else { continue };
        if after.1.split_whitespace().nth(1) == Some(&pid.to_string()) {
            family.push(child);
        }
    }
    let mut inodes = std::collections::HashSet::new();
    for who in &family {
        for fd in std::fs::read_dir(format!("/proc/{}/fd", who))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if let Some(rest) = target.to_string_lossy().strip_prefix("socket:[") {
                inodes.insert(rest.trim_end_matches(']').to_string());
            }
        }
    }
    let mut ports = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(body) = std::fs::read_to_string(table) else {
            continue;
        };
        ports.extend(listening_ports(&body, &inodes));
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Start `agy`, read the quota it serves, and stop it again.
///
/// A pseudo-terminal is not decoration. Started with its input on
/// /dev/null the CLI opens ports that answer nothing for as long as you
/// wait; given a pty it serves the quota within seconds. Both were measured
/// here before this was written.
///
/// The child is ours alone: it is killed by the pid we were handed and
/// nothing is matched by name, so a CLI the reader started themselves can
/// never be shut by this. That one is found by the ordinary local probe
/// long before this runs.
fn agy_quota() -> Result<Vec<serde_json::Value>, String> {
    let Some(path) = agy_path() else {
        return Err("no `agy` on this machine - install the Antigravity CLI, \
                    or set ANTIGRAVITY_CLI_PATH"
            .into());
    };
    let mut master: libc::c_int = 0;
    // SAFETY: forkpty writes the master fd through the pointer and returns
    // in both processes, 0 in the child. The child immediately execs and
    // never returns, so nothing here runs twice.
    let pid = unsafe {
        libc::forkpty(
            &mut master,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    if pid < 0 {
        return Err("could not start `agy`: no pseudo-terminal available".into());
    }
    if pid == 0 {
        // SAFETY: child side. Replace this process with the CLI; if that
        // fails there is nothing to return to, so it exits.
        unsafe {
            let c = std::ffi::CString::new(path.clone()).unwrap_or_default();
            let argv = [c.as_ptr(), std::ptr::null()];
            libc::execv(c.as_ptr(), argv.as_ptr());
            libc::_exit(127);
        }
    }
    let out = agy_wait(pid, master);
    // SAFETY: our own child, by pid. Reaped so it cannot be left a zombie
    // for as long as the widget runs.
    unsafe {
        libc::kill(pid, libc::SIGKILL);
        libc::waitpid(pid, std::ptr::null_mut(), 0);
        libc::close(master);
    }
    out
}

/// Poll a started CLI until its quota endpoint parses, or time runs out.
fn agy_wait(pid: i32, master: libc::c_int) -> Result<Vec<serde_json::Value>, String> {
    // The pty has to be drained or the child blocks writing into a full
    // buffer and never finishes starting. Nothing it prints is read for
    // meaning - this widget does not scrape terminal output.
    // SAFETY: setting O_NONBLOCK on a descriptor we own.
    unsafe {
        let flags = libc::fcntl(master, libc::F_GETFL);
        libc::fcntl(master, libc::F_SETFL, flags | libc::O_NONBLOCK);
    }
    let mut scratch = [0u8; 8192];
    let deadline = now() + AGY_READY;
    let mut ports_seen = false;
    while now() < deadline {
        // SAFETY: reading into our own buffer from our own descriptor.
        loop {
            let got = unsafe {
                libc::read(master, scratch.as_mut_ptr() as *mut libc::c_void, scratch.len())
            };
            if got <= 0 {
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(700));
        for port in ports_under(pid) {
            ports_seen = true;
            let url = format!("http://127.0.0.1:{}{}", port, ANTIGRAVITY_RPC);
            let Some(got) = post_json(&url, &[("Content-Type", "application/json")], "{}", 4)
            else {
                continue;
            };
            let body = if got["response"].as_object().is_some_and(|o| !o.is_empty()) {
                &got["response"]
            } else {
                &got
            };
            if let Some(groups) = body["groups"].as_array() {
                if !groups.is_empty() {
                    return Ok(groups.clone());
                }
            }
        }
    }
    Err(if ports_seen {
        "`agy` started but served no quota - it may not be signed in".into()
    } else {
        "`agy` started and opened no port".into()
    })
}

/// The same quota summary the language server answers, from Google.
///
/// This was thought not to exist. The tab said so, and said it in the
/// strongest terms - that with the app closed there was nothing to ask, on
/// any machine, for anybody - because the only quota this widget knew about
/// came out of a process that dies with the IDE.
///
/// It does exist, it is on the endpoint the tier already comes from, and the
/// body is empty rather than the `pluginType` metadata `loadCodeAssist`
/// wants: sending that here is a 400 naming `metadata` as an unknown field,
/// which reads exactly like an endpoint that is not there.
///
/// What comes back is the same shape the language server sends, group for
/// group and bucket for bucket, so it needs no parser of its own.
const REMOTE_QUOTA_API: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuotaSummary";

/// The Connect method the language server answers the quota on.
const ANTIGRAVITY_RPC: &str =
    "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary";

/// How long each window the server names actually is. It reports the name
/// and the fraction left but not the span, and without the span there is no
/// pace to compare a fill against.
const ANTIGRAVITY_WINDOWS: &[(&str, f64)] = &[("weekly", 7.0 * 86400.0), ("5h", 5.0 * 3600.0)];

fn window_secs(name: &str) -> Option<f64> {
    ANTIGRAVITY_WINDOWS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, s)| *s)
}

/// What Antigravity has recorded here, and what its server says is left.
#[derive(Clone, Default)]
pub struct Data {
    /// Which Code Assist tier the account is on. `None` means the call did
    /// not happen or did not answer, which the tab says out loud.
    live: Option<serde_json::Value>,
    /// Why there is no tier, when there is none. The three reasons want
    /// three different things from the reader - sign in, run the CLI, or
    /// wait - and the old line offered "expired or the call failed", which
    /// is the widget admitting it never looked.
    tier_why: Missing,
    /// What the endpoint said, when it said anything. Empty otherwise.
    tier_said: String,
    /// The quota groups, empty when neither source answered.
    quota: Vec<serde_json::Value>,
    /// Why there is no quota, when there is none. Empty when there is.
    quota_why: String,
    /// Which of the three answered. They agree in shape and very nearly in
    /// value, so nothing else can tell them apart - and a reader wondering
    /// why there are numbers with the app shut deserves the answer.
    quota_from: From,
    /// How the CLI authenticated. Read once here rather than per frame:
    /// the tab is redrawn on every keypress and this is a file on disk.
    auth: String,
    /// Conversation databases found.
    sessions: usize,
    /// How many of them answered. A database a running agent has locked is
    /// not a database with no steps in it, so the two are counted apart.
    counted: usize,
    steps: f64,
    prompts: usize,
    last: f64,
}

/// Whether a command line belongs to Antigravity's language server.
///
/// The same match the Python makes with a regex, written out: `agy` or
/// `antigravity` at the very start of the line or straight after a path
/// separator, or `language_server` anywhere. Anchoring to the executable
/// keeps a process that merely names Antigravity in an argument - a grep,
/// an editor with the file open - from having its sockets probed.
fn is_language_server(cmd: &str) -> bool {
    if cmd.contains("language_server") {
        return true;
    }
    let bytes = cmd.as_bytes();
    for name in ["agy", "antigravity"] {
        let mut at = 0usize;
        while let Some(found) = cmd[at..].find(name) {
            let start = at + found;
            let end = start + name.len();
            let anchored = start == 0 || bytes[start - 1] == b'/';
            let bounded = match bytes.get(end) {
                None => true,
                Some(c) => !c.is_ascii_alphanumeric() && *c != b'_',
            };
            if anchored && bounded {
                return true;
            }
            at = start + 1;
        }
    }
    false
}

/// Listening sockets in a /proc/net/tcp table that belong to us.
///
/// State 0A is LISTEN, column 9 is the socket inode, and the local address
/// carries the port as hex after the colon. Split out from the reading so
/// the parse can be tested against a table nobody's machine has to have.
fn listening_ports(table: &str, inodes: &HashSet<String>) -> Vec<u16> {
    let mut out = Vec::new();
    for row in table.lines().skip(1) {
        let cols: Vec<&str> = row.split_whitespace().collect();
        if cols.len() <= 9 || cols[3] != "0A" || !inodes.contains(cols[9]) {
            continue;
        }
        if let Some(hex) = cols[1].split(':').nth(1) {
            if let Ok(port) = u16::from_str_radix(hex, 16) {
                out.push(port);
            }
        }
    }
    out
}

/// TCP ports the running Antigravity language server listens on.
///
/// The quota never reaches disk, but the process holding it serves an RPC
/// on localhost, so the port is the way in. Found by matching the process
/// then reading its listening sockets out of /proc - no lsof, no guessing
/// at a range, and nothing touched but our own machine.
fn antigravity_ports() -> Vec<u16> {
    let mut inodes: HashSet<String> = HashSet::new();
    for entry in std::fs::read_dir("/proc").into_iter().flatten().flatten() {
        let pid = entry.file_name().to_string_lossy().to_string();
        if pid.is_empty() || !pid.chars().all(|c| c.is_ascii_digit()) {
            continue;
        }
        let Ok(raw) = std::fs::read(format!("/proc/{}/cmdline", pid)) else {
            continue;
        };
        let cmd = String::from_utf8_lossy(&raw).replace('\0', " ");
        if !is_language_server(&cmd) {
            continue;
        }
        // Another user's file descriptors are not readable, and that is not
        // an error worth reporting - it is simply not our process.
        for fd in std::fs::read_dir(format!("/proc/{}/fd", pid))
            .into_iter()
            .flatten()
            .flatten()
        {
            let Ok(target) = std::fs::read_link(fd.path()) else {
                continue;
            };
            if let Some(rest) = target.to_string_lossy().strip_prefix("socket:[") {
                inodes.insert(rest.trim_end_matches(']').to_string());
            }
        }
    }
    if inodes.is_empty() {
        return Vec::new();
    }
    let mut ports: Vec<u16> = Vec::new();
    for table in ["/proc/net/tcp", "/proc/net/tcp6"] {
        let Ok(body) = std::fs::read_to_string(table) else {
            continue;
        };
        ports.extend(listening_ports(&body, &inodes));
    }
    ports.sort_unstable();
    ports.dedup();
    ports
}

/// Weekly and five-hour limits, from the language server on localhost.
///
/// The same figures Antigravity's own TUI prints. It speaks Connect over
/// plain HTTP on a loopback port - its TLS listener answers with a wrong
/// version number, so http is not a downgrade here, it is the protocol -
/// and the call leaves this machine no more than reading a file would.
///
/// Found by reading how CodexBar does it (github.com/steipete/CodexBar),
/// after chasing the Google endpoint in the binary to a 404: the quota was
/// never a remote call to make, it was a local one.
fn antigravity_quota() -> Option<serde_json::Value> {
    for port in antigravity_ports() {
        let url = format!("http://127.0.0.1:{}{}", port, ANTIGRAVITY_RPC);
        let Some(got) = post_json(&url, &[("Content-Type", "application/json")], "{}", 5) else {
            continue;
        };
        // Some builds wrap the payload and some do not, so an envelope is
        // only unwrapped when it actually holds something.
        let body = if got["response"].as_object().is_some_and(|o| !o.is_empty()) {
            &got["response"]
        } else {
            &got
        };
        // Anything else answering on a matched port is not this server, so
        // the loop keeps going rather than settling for an empty reply.
        if let Some(groups) = body["groups"].as_array() {
            if !groups.is_empty() {
                return Some(serde_json::Value::Array(groups.clone()));
            }
        }
    }
    None
}

/// Which Code Assist tier the account is on.
///
impl Data {
    /// The reason the tier is missing, for callers outside this module -
    /// the summary says it too, under a heading of Antigravity's own.
    pub fn why_no_tier(&self) -> Missing {
        if self.live.is_some() { Missing::Nothing } else { self.tier_why }
    }
}

/// Which source the quota on screen came from.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum From {
    /// A language server already running - the app's, or a CLI the reader
    /// started. Nothing was launched and nothing left the machine.
    #[default]
    Local,
    /// Google, on the endpoint the tier comes from. Works for as long as
    /// the token lasts, which is an hour past the last run.
    Google,
    /// A CLI this widget started, read, and stopped again.
    Agy,
}

/// Why the tier is missing, in the three ways it can be.
#[derive(Clone, Copy, Default, PartialEq)]
pub enum Missing {
    /// There is a tier; nothing is missing.
    #[default]
    Nothing,
    /// No token file, or one with no access token in it.
    NotSignedIn,
    /// A token that has lapsed, with how long ago in seconds.
    Expired(f64),
    /// A good token and no answer. Google's, not ours.
    NoAnswer,
}


/// The missing tier in one sentence, ending in what to do about it.
///
/// The line this replaces read "no tier: the CLI's access token has expired
/// or the call failed", which names two causes, commits to neither, and asks
/// nothing of the reader. Each of these commits, because the widget can tell
/// them apart - it just was not looking.
pub fn tier_note(why: Missing) -> String {
    match why {
        Missing::Nothing => String::new(),
        Missing::NotSignedIn => {
            "no tier · Antigravity has not signed in on this machine yet. \
             Open it once and the tier appears here."
                .into()
        }
        Missing::Expired(ago) => format!(
            "no tier · Antigravity's access token expired {} ago. \
             It refreshes them itself - open it once and this fills in.",
            left_span(ago)
        ),
        Missing::NoAnswer => {
            "no tier · the token is good, but Google's Code Assist API did not \
             answer. Nothing to do here; it is retried every hour."
                .into()
        }
    }
}

/// Why Antigravity publishes no bar, for the summary screen.
///
/// A missing tier is one reason and was the only one this said out loud,
/// so an account whose tier reads perfectly well produced an empty note
/// and "No quota published by: antigravity" with nothing after it. The
/// commoner reason by far is the other one: unlike every other agent here
/// Antigravity has no account-wide quota endpoint at all, and its
/// percentages live in a language server that exists only while the app is
/// running. Its own tab has always said so; the summary had not.
pub fn why_no_lane(d: &Data) -> String {
    let tier = tier_note(d.why_no_tier());
    if !tier.is_empty() {
        return tier;
    }
    // The remote attempt knows exactly which step failed, and that is more
    // use than any sentence written in advance.
    if !d.quota_why.is_empty() {
        return format!("no quota · {}", d.quota_why);
    }
    "no quota · neither the language server inside the app nor Google answered.".into()
}

/// The same, with the server's own words when there are any.
pub fn tier_note_said(why: Missing, said: &str) -> String {
    let base = tier_note(why);
    match (base.is_empty(), said.is_empty()) {
        (true, _) => String::new(),
        (false, true) => base,
        (false, false) => format!("{} It said: {}.", base, said),
    }
}

/// What is wrong with the credential, read from the same file the call uses.
///
/// Cheap enough to do every frame - it is a small JSON file - and it must
/// not be cached with the call: the call is held for an hour, and a token
/// that lapses inside that hour would otherwise be reported as an endpoint
/// that would not answer.
fn why_no_tier() -> Missing {
    let Some(file) = read_json(&token_path()) else {
        return Missing::NotSignedIn;
    };
    let tok = &file["token"];
    if text(tok, "access_token").is_empty() {
        return Missing::NotSignedIn;
    }
    match iso_epoch(&text(tok, "expiry")) {
        Some(at) if at <= now() => Missing::Expired(now() - at),
        _ => Missing::NoAnswer,
    }
}

fn antigravity_said() -> Result<serde_json::Value, String> {
    let Some(file) = read_json(&token_path()) else {
        return Err(String::new());
    };
    let tok = &file["token"];
    let access = text(tok, "access_token");
    let expiry = iso_epoch(&text(tok, "expiry"));
    if access.is_empty() || expiry.is_some_and(|at| at <= now()) {
        return Err(String::new());
    }
    post_try(CODE_ASSIST_API, &access)
}

/// The same call, keeping why it failed.
fn post_try(url: &str, access: &str) -> Result<serde_json::Value, String> {
    post_body(url, access, "{\"metadata\":{\"pluginType\":\"GEMINI\"}}")
}

/// The quota summary Google holds, when nothing local is serving one.
///
/// Preferred second, not first: the language server is the app's own answer
/// and moves as it is used, while this is what Google has recorded. They
/// agree in shape and very nearly in value, so the tab cannot tell them
/// apart - which is the point, and why the row says which it read.
/// Everything that can be decided without leaving the machine, decided
/// before anything is cached.
///
/// The reason has to survive a cache hit. `cached` holds a failure for a
/// while and does not re-run the closure that produced it, so a reason
/// captured inside that closure is gone by the next frame and the row falls
/// back to whatever generic sentence is left - which is how this said
/// "Google did not answer" about a token that had expired an hour earlier
/// and was never sent.
fn remote_token(allowed: bool) -> Result<String, String> {
    if !allowed {
        return Err("asking Google is off - set usage.antigravity_remote to true".into());
    }
    let Some(file) = read_json(&token_path()) else {
        return Err(
            "Antigravity has not signed in on this machine - run `agy` once and sign in".into(),
        );
    };
    let tok = &file["token"];
    let access = text(tok, "access_token");
    if access.is_empty() {
        return Err("the Antigravity token file holds no session - sign in again".into());
    }
    // A lapsed token is not sent. It would be refused, and the refusal would
    // then be the only thing reported, which says less than the expiry does:
    // this one names how long ago and what refreshes it.
    if let Some(expiry) = iso_epoch(&text(tok, "expires")) {
        if expiry <= now() {
            return Err(expired_note(now() - expiry));
        }
    }
    if let Some(expiry) = iso_epoch(&text(tok, "expiry")) {
        if expiry <= now() {
            return Err(expired_note(now() - expiry));
        }
    }
    Ok(access)
}

fn expired_note(ago: f64) -> String {
    format!(
        "Antigravity's token expired {} ago - it refreshes them itself, so open it \
         or run `agy` once and sign in",
        left_span(ago)
    )
}

/// The ask itself, once the credential is known to be worth sending.
fn remote_quota(access: &str) -> Result<Vec<serde_json::Value>, String> {
    let got = post_body(REMOTE_QUOTA_API, access, "{}")
        .map_err(|said| format!("Google refused the Antigravity token: {}", said))?;
    match got["groups"].as_array() {
        Some(groups) if !groups.is_empty() => Ok(groups.clone()),
        // A 200 with nothing in it. Not a failure to report as one, but not
        // a reading either, and the difference matters when the reader is
        // deciding whether to go and open something.
        _ => Err("Google answered with no quota groups for this account".into()),
    }
}

fn post_body(url: &str, access: &str, body: &str) -> Result<serde_json::Value, String> {
    post_json_said(
        url,
        &[
            ("Authorization", &format!("Bearer {}", access)),
            ("Content-Type", "application/json"),
            // Google gates this response on the client string. Sent as
            // plain opscope it answers UNSUPPORTED_CLIENT and returns
            // no tier at all; the parenthesised form is the conventional way
            // to name the client being spoken for while still saying who is
            // actually calling.
            ("User-Agent", "opscope (antigravity-cli)"),
        ],
        body,
        20,
    )
}

/// How many steps one conversation store records, or nothing if it would
/// not open.
///
/// Read-only on purpose: these are a live agent's working state, and this
/// widget has no business being able to write to them. A busy database
/// waits briefly and then gives up - the pane refreshes on a clock, so
/// blocking a poll for seconds to spare one number would cost every other
/// number on the wall.
fn conversation_steps(path: &str) -> Option<f64> {
    let con = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY).ok()?;
    con.busy_timeout(Duration::from_millis(250)).ok()?;
    let count: i64 = con
        .query_row("select count(*) from steps", [], |row| row.get(0))
        .ok()?;
    Some(count as f64)
}

/// What the conversation stores record, which is activity and not cost.
///
/// Each conversation is its own SQLite file with a `steps` table - one row
/// per step the agent took - so the counts are real work done. No table
/// anywhere carries a token count.
pub fn read(caches: &mut Caches, cfg: &Config) -> Data {
    use std::os::unix::fs::MetadataExt;
    // The refusal is cached with the failure, so it is shown for as long as
    // the failure lasts rather than only on the frame the call was made.
    let live = cached(caches, "antigravity", PLAN_TTL, || {
        match antigravity_said() {
            Ok(v) => Some(v),
            Err(why) => Some(serde_json::json!({ "opscope_refusal": why })),
        }
    });
    let said = live
        .as_ref()
        .map(|v| text(v, "opscope_refusal"))
        .unwrap_or_default();
    let live = if said.is_empty() { live } else { None };
    let mut d = Data {
        tier_why: if live.is_some() { Missing::Nothing } else { why_no_tier() },
        tier_said: said,
        live,
        quota: cached(caches, "antigravity-quota", LIVE_TTL, antigravity_quota)
            .and_then(|got| got.as_array().cloned())
            .unwrap_or_default(),
        auth: read_json(&token_path())
            .map(|file| text(&file, "auth_method"))
            .unwrap_or_default(),
        ..Data::default()
    };
    // Local first: it is the app's own answer and moves as the app is used.
    // Google's is asked only when there is no server to ask, and is held far
    // longer - it is a record rather than a live meter, and unlike the local
    // one it is a request that leaves this machine.
    // Three sources, cheapest first, and every one of them optional.
    //
    // A language server already running costs a socket read, so it is
    // always tried. Google costs one request and works for as long as the
    // token lasts. Starting the CLI costs a process and several seconds, so
    // it is last however preferred it is - being last is not being
    // disfavoured, it is being expensive.
    if d.quota.is_empty() {
        let mut why = String::new();
        match remote_token(cfg.antigravity_remote) {
            // Decided before the cache, so it survives a held failure and
            // is the same sentence on every frame until it changes.
            Err(said) => why = said,
            Ok(access) => {
                let mut asked = String::new();
                let got = cached(caches, "antigravity-remote", PLAN_TTL, || {
                    match remote_quota(&access) {
                        Ok(groups) => Some(serde_json::Value::Array(groups)),
                        Err(said) => {
                            asked = said;
                            None
                        }
                    }
                })
                .and_then(|got| got.as_array().cloned());
                match got {
                    Some(groups) => {
                        d.quota = groups;
                        d.quota_from = From::Google;
                    }
                    None => {
                        why = if asked.is_empty() {
                            "Google did not answer, and the refusal is still held".to_string()
                        } else {
                            asked
                        }
                    }
                }
            }
        }
        if d.quota.is_empty() && cfg.antigravity_start {
            let mut asked = String::new();
            let got = cached(caches, "antigravity-agy", AGY_TTL, || match agy_quota() {
                Ok(groups) => Some(serde_json::Value::Array(groups)),
                Err(said) => {
                    asked = said;
                    None
                }
            })
            .and_then(|got| got.as_array().cloned());
            match got {
                Some(groups) => {
                    d.quota = groups;
                    d.quota_from = From::Agy;
                    why.clear();
                }
                // Both failed. The CLI's reason is the later and more
                // specific one, so it wins - "Google refused the token" and
                // "`agy` is not signed in" are the same fault said twice,
                // and the second names what to do.
                None => {
                    if !asked.is_empty() {
                        why = asked;
                    }
                }
            }
        }
        if d.quota.is_empty() {
            d.quota_why = why;
        }
    }
    let mut files: Vec<String> = std::fs::read_dir(format!("{}/conversations", antigravity_dir()))
        .into_iter()
        .flatten()
        .flatten()
        .map(|entry| entry.path().to_string_lossy().to_string())
        .filter(|path| path.ends_with(".db"))
        .collect();
    files.sort();
    d.sessions = files.len();
    for path in &files {
        let Ok(meta) = std::fs::metadata(path) else {
            continue;
        };
        // The timestamp is taken before the query, so a conversation too
        // busy to count still proves the agent ran.
        d.last = d.last.max(meta.mtime() as f64);
        if let Some(steps) = conversation_steps(path) {
            d.steps += steps;
            d.counted += 1;
        }
    }
    let history = format!("{}/history.jsonl", antigravity_dir());
    if let Ok(body) = std::fs::read_to_string(&history) {
        d.prompts = body.lines().filter(|line| !line.trim().is_empty()).count();
        if let Ok(meta) = std::fs::metadata(&history) {
            d.last = d.last.max(meta.mtime() as f64);
        }
    }
    d
}

/// Every quota this agent publishes, for the summary screen.
///
/// Grouped by model family on the tab, flattened here: the summary compares
/// lanes across agents, and "gemini weekly" says which family it is without
/// the group heading the tab can afford.
///
/// Never marked stale. The language server answers only while Antigravity
/// is running and this reading is at most one refresh old, so unlike a
/// file-backed cache there is no old number here to warn about.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let mut out = Vec::new();
    for group in &d.quota {
        let display = text(group, "displayName");
        let short = display
            .split_whitespace()
            .next()
            .unwrap_or("?")
            .to_lowercase();
        for bucket in group["buckets"].as_array().into_iter().flatten() {
            let Some(left) = bucket["remainingFraction"].as_f64() else {
                continue;
            };
            let window = match text(bucket, "window") {
                s if s.is_empty() => "?".to_string(),
                s => s,
            };
            out.push(Lane {
                label: format!("{} {}", short, window),
                pct: 100.0 * (1.0 - left),
                window_secs: window_secs(&window),
                reset: iso_epoch(&text(bucket, "resetTime")),
                stale: false,
                projected: false,
                            apart: false,
            });
        }
    }
    out
}

/// One bar per limit, grouped by the model family it covers.
///
/// Shown as spent rather than the remaining fraction the RPC returns, so
/// red means the same here as on every other tab. Every plan reports every
/// family it covers, so a Gemini-only account still gets a Claude/GPT pair
/// sitting at 0% - they are real limits, not padding, and are left in.
fn antigravity_quota_rows(
    groups: &[serde_json::Value],
    from: From,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    // The long form names where the number comes from, which matters here
    // more than elsewhere; the short one still says it is not this machine's
    // own tally. Shortened before it can clip, as the other headers are.
    let (mut note, short, tiny) = match from {
        From::Google => (
            " · account-wide, from Google - the app is not running",
            " · from Google",
            " · remote",
        ),
        From::Agy => (
            " · account-wide, from `agy`, started for this reading",
            " · from `agy`",
            " · agy",
        ),
        From::Local => (
            " · account-wide, from the local language server",
            " · from the local server",
            " · local",
        ),
    };
    for shorter in [short, tiny] {
        if 13 + "live".len() + note.chars().count() <= w.saturating_sub(1) {
            break;
        }
        note = shorter;
    }
    let hue = agent_hue("antigravity");
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), note.into()),
        ],
        w - 1,
    )];
    for group in groups {
        let buckets: Vec<&serde_json::Value> = group["buckets"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|b| b["remainingFraction"].as_f64().is_some())
            .collect();
        if buckets.is_empty() {
            continue;
        }
        let name = match text(group, "displayName") {
            s if s.is_empty() => "?".to_string(),
            s => s,
        };
        rows.push(tc::seg(&[(p.txt.as_str(), format!("  {}", name))], w - 1));
        let windows: Vec<String> = buckets
            .iter()
            .map(|b| match text(b, "window") {
                s if s.is_empty() => "?".to_string(),
                s => s,
            })
            .collect();
        let label_w = windows.iter().map(|s| s.chars().count()).max().unwrap_or(1);
        for (bucket, window) in buckets.iter().zip(&windows) {
            let pct = 100.0 * (1.0 - bucket["remainingFraction"].as_f64().unwrap_or(0.0));
            let used = (pct / 100.0).clamp(0.0, 1.0);
            let secs = window_secs(window);
            let reset = iso_epoch(&text(bucket, "resetTime"));
            let mut when = match reset {
                None => String::new(),
                Some(at) if at - now() > 0.0 => format!("resets in {}", left_span(at - now())),
                Some(_) => "resetting".to_string(),
            };
            let mut room = w as i64 - 36 - label_w as i64;
            if room < 8 {
                // Below the bar's floor the row cannot shrink further, so the
                // reset stands down rather than being cut in half.
                when = String::new();
                room = w as i64 - 20 - label_w as i64;
            }
            let mut line: Vec<(String, String)> = vec![(
                p.dim.clone(),
                format!("   {} ", tc::pad(window, label_w)),
            )];
            line.extend(paced_bar(
                used,
                elapsed_of(secs, reset),
                room.max(8) as usize,
                hue,
                p,
            ));
            line.push((pct_colour(pct, hue, p), pct_text(pct)));
            let (pace_colour, pace_txt) = pace_cell(lead(pct, secs, reset), p);
            line.push((pace_colour, pace_txt));
            line.push((p.dim.clone(), format!("  {}", when)));
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
    }
    rows.push(String::new());
    rows
}

fn antigravity_plan_rows(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let live = d.live.clone().unwrap_or(serde_json::Value::Null);
    let filled = |v: &serde_json::Value| v.as_object().is_some_and(|o| !o.is_empty());
    if !filled(&live["currentTier"]) && !filled(&live["paidTier"]) {
        return Vec::new();
    }
    let (cur, paid) = (&live["currentTier"], &live["paidTier"]);
    let mut pairs: Vec<(String, String)> = Vec::new();
    for (label, value) in [
        ("code assist tier", text(cur, "id")),
        // paidTier is the Google AI subscription behind the account, which
        // is a different thing from the Code Assist tier and can disagree
        // with it - free-tier here, while the account is on Ultra. Both are
        // stated.
        ("google ai plan", text(paid, "name")),
        ("project", text(&live, "cloudaicompanionProject")),
        ("auth", d.auth.clone()),
    ] {
        if !value.is_empty() {
            pairs.push((label.into(), value));
        }
    }
    // The two tiers can disagree - free-tier beside Google AI Ultra is
    // normal, since one is GCP licensing and the other a consumer plan - but
    // that is a paragraph the docs can carry, not four lines on every frame.
    let headline = match text(cur, "name") {
        s if s.is_empty() => text(paid, "name"),
        s => s,
    };
    plan_rows(&headline, &pairs, w, "", None, "", p)
}

/// The caveat on the step count, in the longest form that fits.
///
/// Shortened rather than clipped, for the reason the reset above it is:
/// "from 7 of 9 conversa" is not a shorter sentence, it is a broken one,
/// and this qualifies the one number on the tab that can be incomplete.
fn steps_note(d: &Data, room: i64) -> String {
    if d.sessions == d.counted {
        return String::new();
    }
    let forms = if d.counted == 0 {
        [
            format!("  {} conversations, none readable just now", d.sessions),
            format!("  none of {} readable", d.sessions),
            "  unreadable".to_string(),
        ]
    } else {
        [
            format!("  from {} of {} conversations", d.counted, d.sessions),
            format!("  {} of {} readable", d.counted, d.sessions),
            format!("  {}/{}", d.counted, d.sessions),
        ]
    };
    let fits = forms
        .iter()
        .find(|form| form.chars().count() as i64 <= room)
        .unwrap_or(&forms[2]);
    fits.clone()
}

/// What this machine recorded: conversations, steps and prompts.
///
/// Steps are the one number here that can go missing. A conversation the
/// running agent has locked is unreadable rather than empty, so a partial
/// sum says how partial it is and a sum of nothing says nothing - a zero
/// would read as an idle agent, which is the opposite of the truth.
fn antigravity_activity(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── ACTIVITY ── ".into()),
            (
                p.dim.as_str(),
                format!(
                    "local · {}",
                    if d.last > 0.0 {
                        format!("last {} ago", ago(d.last))
                    } else {
                        "never run here".to_string()
                    }
                ),
            ),
        ],
        w - 1,
    )];
    let steps = if d.counted == 0 && d.sessions > 0 {
        "—".to_string()
    } else {
        format!("{}", d.steps as i64)
    };
    let cells = [
        ("conversations", format!("{}", d.sessions), p.txt.as_str()),
        ("agent steps", steps, p.agent.as_str()),
        ("prompts", format!("{}", d.prompts), p.txt.as_str()),
    ];
    let label_w = cells.iter().map(|c| c.0.len()).max().unwrap_or(0);
    for (label, value, colour) in &cells {
        let lead = format!("  {}  ", tc::pad(label, label_w));
        let mut line = vec![(p.dim.as_str(), lead.clone()), (*colour, value.clone())];
        // Only the step count can be short of the truth, and it says so
        // beside the number rather than in a footnote further down.
        let note = match *label {
            "agent steps" => steps_note(
                d,
                w as i64 - 1 - lead.chars().count() as i64 - value.chars().count() as i64,
            ),
            _ => String::new(),
        };
        if !note.is_empty() {
            line.push((p.warn.as_str(), note));
        }
        rows.push(tc::seg(&line, w - 1));
    }
    rows.push(String::new());
    rows
}

fn antigravity_body(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    let mut rows = antigravity_quota_rows(&d.quota, d.quota_from, w, p);
    if d.live.is_none() {
        rows.extend(
            wrap_text(&tier_note_said(d.tier_why, &d.tier_said), w.saturating_sub(4).max(20))
                .into_iter()
                .map(|line| tc::seg(&[(p.warn.as_str(), format!("  {}", line))], w - 1)),
        );
        rows.push(String::new());
    }
    rows.extend(antigravity_activity(d, w, p));
    // The absence is only worth explaining while it is one. With the quota
    // drawn above, a paragraph about why there is no quota contradicts the
    // screen.
    //
    // When there is no quota, the sentence carries the step that failed
    // rather than the shape of the problem. "It comes from the language
    // server while Antigravity is running" is true and was all this said,
    // and it sends a reader to open an app when the actual fault can be a
    // session signed out, a token an hour old, or a setting turned off -
    // none of which opening the app on its own would settle.
    let absent = if d.quota_why.is_empty() {
        "No tokens are recorded locally, and no quota either: it comes \
         from the language server while Antigravity is running, so start \
         it and this fills in."
            .to_string()
    } else {
        format!(
            "No tokens are recorded locally, and no quota either: {}",
            d.quota_why
        )
    };
    rows.extend(no_local(
        if d.quota.is_empty() {
            absent.as_str()
        } else {
            "No per-token usage is recorded locally - the conversations and \
             steps above are what there is."
        },
        "",
        w,
        p,
    ));
    rows
}

/// The whole tab: what is left of the limits, what this machine did, and
/// which subscription the percentages are percentages of.
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    add_section(antigravity_body(d, w, p), antigravity_plan_rows(d, w, p))
}

#[cfg(test)]
mod tests {

    #[test]
    fn the_note_says_which_step_failed_not_that_something_did() {
        // "No quota" covered four situations wanting different things from
        // the reader: never signed in, signed out since, an hour-old token,
        // and a Google that said no. Only one of them is fixed by opening
        // the app, so the note carries whichever actually happened.
        let with = |why: &str| Data {
            live: Some(serde_json::json!({"currentTier": {"id": "free"}})),
            quota: Vec::new(),
            quota_why: why.to_string(),
            ..Data::default()
        };

        let d = with("Antigravity's token expired 2h ago - it refreshes them itself");
        assert!(lanes(&d).is_empty(), "no groups, no lanes");
        let note = why_no_lane(&d);
        assert!(note.starts_with("no quota · "), "{}", note);
        assert!(note.contains("expired 2h ago"), "the step that failed was dropped: {}", note);

        // Off by configuration is a reason too, and names the key.
        let note = why_no_lane(&with(
            "asking Google is off - set usage.antigravity_remote to true",
        ));
        assert!(note.contains("usage.antigravity_remote"), "{}", note);

        // A tier that cannot be read still wins: it is the older problem and
        // opening the app is what fixes it.
        let lapsed = Data {
            live: None,
            tier_why: Missing::Expired(3600.0),
            quota_why: "Google refused the Antigravity token".into(),
            ..Data::default()
        };
        let note = why_no_lane(&lapsed);
        assert!(note.contains("expired"), "tier reason lost: {}", note);
        assert!(!note.contains("Google refused"), "two reasons at once: {}", note);
    }

    #[test]
    fn the_remote_ask_refuses_before_it_leaves_the_machine() {
        // Turned off, and there is no request to make - the reason names the
        // key rather than blaming the network.
        let off = remote_token(false).unwrap_err();
        assert!(off.contains("usage.antigravity_remote"), "{}", off);
        // It may name Google - that is where the request would have gone -
        // but it must not report a refusal or a silence that never happened.
        for blame in ["refused", "did not answer", "no quota groups"] {
            assert!(!off.contains(blame), "blamed the server for a setting: {}", off);
        }
    }
    use super::*;

    /// A conversation store built here rather than found on this machine:
    /// the schema is the whole assertion, and a real one belongs to a live
    /// agent.
    fn temp_db(tag: &str, steps: usize, table: &str) -> String {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let path = std::env::temp_dir().join(format!(
            "toys-antigravity-{}-{}-{}.db",
            tag,
            std::process::id(),
            unique
        ));
        let name = path.to_string_lossy().to_string();
        let con = Connection::open(&name).expect("a temp database");
        con.execute(&format!("create table {} (id integer primary key)", table), [])
            .expect("the schema");
        for i in 0..steps {
            con.execute(&format!("insert into {} (id) values (?1)", table), [i as i64])
                .expect("a row");
        }
        drop(con);
        name
    }

    fn groups(fractions: &[(&str, &str, f64)]) -> Vec<serde_json::Value> {
        let mut out: Vec<serde_json::Value> = Vec::new();
        for (family, window, left) in fractions {
            let bucket = serde_json::json!({
                "window": window,
                "remainingFraction": left,
                "resetTime": "2099-01-01T00:00:00Z",
            });
            match out
                .iter_mut()
                .find(|g| text(g, "displayName") == *family)
            {
                Some(g) => g["buckets"].as_array_mut().unwrap().push(bucket),
                None => out.push(serde_json::json!({
                    "displayName": family,
                    "buckets": [bucket],
                })),
            }
        }
        out
    }

    #[test]
    fn the_steps_table_is_counted_from_a_database_we_built() {
        let path = temp_db("counts", 7, "steps");
        assert_eq!(conversation_steps(&path), Some(7.0));
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_database_that_is_not_there_is_unknown_rather_than_created() {
        // The point of opening read-only: a missing path must not become an
        // empty database that then reports zero steps for ever after.
        let missing = std::env::temp_dir().join(format!(
            "toys-antigravity-absent-{}.db",
            std::process::id()
        ));
        let name = missing.to_string_lossy().to_string();
        assert_eq!(conversation_steps(&name), None);
        assert!(!missing.exists());
    }

    #[test]
    fn a_store_without_a_steps_table_counts_as_unreadable_not_as_zero() {
        let path = temp_db("noschema", 3, "notes");
        assert_eq!(conversation_steps(&path), None);
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn a_conversation_that_would_not_open_leaves_the_step_count_unknown() {
        let p = palette();
        let d = Data {
            sessions: 3,
            counted: 0,
            steps: 0.0,
            last: now() - 60.0,
            ..Data::default()
        };
        let shown = antigravity_activity(&d, 90, &p).join(" ");
        assert!(shown.contains("—"), "an unknown count is not a zero");
        assert!(shown.contains("3 conversations, none readable just now"));
    }

    #[test]
    fn the_caveat_shortens_rather_than_being_cut_in_half() {
        let d = Data {
            sessions: 9,
            counted: 7,
            ..Data::default()
        };
        assert_eq!(steps_note(&d, 80), "  from 7 of 9 conversations");
        assert_eq!(steps_note(&d, 20), "  7 of 9 readable");
        assert_eq!(steps_note(&d, 8), "  7/9");
        // Narrower than any form: the shortest still says 7 of 9, which is
        // the part that must survive.
        assert_eq!(steps_note(&d, 1), "  7/9");
        let whole = Data {
            sessions: 9,
            counted: 9,
            ..Data::default()
        };
        assert_eq!(steps_note(&whole, 4), "");
        // Nothing found is not something unreadable.
        assert_eq!(steps_note(&Data::default(), 80), "");
    }

    #[test]
    fn a_narrow_pane_keeps_the_caveat_whole() {
        let p = palette();
        let d = Data {
            sessions: 9,
            counted: 7,
            steps: 1234.0,
            ..Data::default()
        };
        let shown = antigravity_activity(&d, 44, &p).join(" ");
        assert!(!shown.contains("conversa "), "a clipped word is a broken one");
        assert!(shown.contains("7 of 9 readable"));
    }

    #[test]
    fn a_partial_step_count_says_how_partial_it_is() {
        let p = palette();
        let d = Data {
            sessions: 9,
            counted: 7,
            steps: 1234.0,
            ..Data::default()
        };
        let shown = antigravity_activity(&d, 90, &p).join(" ");
        assert!(shown.contains("1234"));
        assert!(shown.contains("from 7 of 9 conversations"));
        assert!(shown.contains("never run here"));
    }

    #[test]
    fn a_complete_step_count_carries_no_caveat() {
        let p = palette();
        let d = Data {
            sessions: 2,
            counted: 2,
            steps: 40.0,
            prompts: 5,
            ..Data::default()
        };
        let shown = antigravity_activity(&d, 90, &p).join(" ");
        assert!(shown.contains("40"));
        assert!(!shown.contains("of 2 conversations"));
    }

    #[test]
    fn every_bucket_of_every_group_becomes_one_lane() {
        let d = Data {
            quota: groups(&[
                ("Gemini 3 Pro", "weekly", 0.25),
                ("Gemini 3 Pro", "5h", 1.0),
                ("Claude Sonnet 4.5", "weekly", 0.5),
            ]),
            ..Data::default()
        };
        let found = lanes(&d);
        assert_eq!(found.len(), 3);
        assert_eq!(found[0].label, "gemini weekly");
        assert!((found[0].pct - 75.0).abs() < 1e-9);
        assert_eq!(found[0].window_secs, Some(7.0 * 86400.0));
        assert_eq!(found[1].label, "gemini 5h");
        assert_eq!(found[1].pct, 0.0);
        assert_eq!(found[2].label, "claude weekly");
        assert!(found.iter().all(|l| !l.stale));
        assert!(found.iter().all(|l| l.reset.is_some()));
    }

    #[test]
    fn a_bucket_with_no_fraction_is_left_out_rather_than_read_as_full() {
        let d = Data {
            quota: vec![serde_json::json!({
                "displayName": "Gemini 3 Pro",
                "buckets": [
                    {"window": "weekly"},
                    {"window": "5h", "remainingFraction": 0.9},
                ],
            })],
            ..Data::default()
        };
        let found = lanes(&d);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].label, "gemini 5h");
    }

    #[test]
    fn a_group_names_itself_once_and_its_windows_underneath() {
        let p = palette();
        let rows = antigravity_quota_rows(
            &groups(&[
                ("Gemini 3 Pro", "weekly", 0.25),
                ("Gemini 3 Pro", "5h", 0.996),
                ("Claude Sonnet 4.5", "weekly", 1.0),
            ]),
            From::Local,
            96,
            &p,
        );
        let shown = rows.join("\n");
        assert_eq!(shown.matches("Gemini 3 Pro").count(), 1);
        assert!(shown.contains("Claude Sonnet 4.5"));
        assert!(shown.contains("QUOTA"));
        // Spent, not remaining: a quarter left is three quarters gone.
        assert!(shown.contains("75%"));
        // A real small number and no number at all have to be tellable apart.
        assert!(shown.contains("0.40%"));
    }

    #[test]
    fn a_group_with_nothing_measurable_in_it_is_skipped() {
        let p = palette();
        let rows = antigravity_quota_rows(
            &[serde_json::json!({"displayName": "GPT", "buckets": []})],
            From::Local,
            96,
            &p,
        );
        // The header and its blank line, and no group heading between them.
        assert_eq!(rows.len(), 2);
        assert!(!rows[0].contains("GPT"));
    }

    #[test]
    fn the_source_note_shortens_before_it_can_clip() {
        let p = palette();
        let quota = groups(&[("Gemini 3 Pro", "weekly", 0.5)]);
        let wide = antigravity_quota_rows(&quota, From::Local, 120, &p)[0].clone();
        let narrow = antigravity_quota_rows(&quota, From::Local, 60, &p)[0].clone();
        let tight = antigravity_quota_rows(&quota, From::Local, 30, &p)[0].clone();
        assert!(wide.contains("from the local language server"));
        assert!(narrow.contains("from the local server"));
        assert!(tight.contains("local"));
        assert!(!tight.contains("from the local server"));
    }

    #[test]
    fn no_quota_is_explained_and_a_quota_is_not_contradicted() {
        let p = palette();
        // A reason has to be named: the tab says which of the three it is,
        // so a fixture that leaves it at "nothing is missing" is asking for
        // a sentence there is no cause to print.
        let empty = antigravity_body(
            &Data {
                tier_why: Missing::NoAnswer,
                ..Data::default()
            },
            90,
            &p,
        )
        .join(" ");
        assert!(empty.contains("no quota either"));
        assert!(empty.contains("no tier"));
        let with = antigravity_body(
            &Data {
                quota: groups(&[("Gemini 3 Pro", "weekly", 0.5)]),
                live: Some(serde_json::json!({"currentTier": {"id": "free-tier"}})),
                ..Data::default()
            },
            90,
            &p,
        )
        .join(" ");
        assert!(with.contains("No per-token usage is recorded locally"));
        assert!(!with.contains("no quota either"));
        assert!(!with.contains("no tier"));
    }

    #[test]
    fn each_missing_tier_asks_for_something_different() {
        // The line this replaced offered "expired or the call failed" for
        // all three, which is two causes, no commitment and nothing to do.
        let signed_out = tier_note(Missing::NotSignedIn);
        let lapsed = tier_note(Missing::Expired(3.0 * 3600.0));
        let silent = tier_note(Missing::NoAnswer);
        for note in [&signed_out, &lapsed, &silent] {
            assert!(note.starts_with("no tier · "), "{:?}", note);
            assert!(note.trim_end().ends_with('.'), "not a sentence: {:?}", note);
        }
        assert!(signed_out.contains("not signed in"), "{}", signed_out);
        assert!(lapsed.contains("3h"), "the age is the useful part: {}", lapsed);
        assert!(silent.contains("Google"), "{}", silent);
        // and each is a different sentence, which is the whole point
        assert_ne!(signed_out, lapsed);
        assert_ne!(lapsed, silent);
        assert_ne!(signed_out, silent);
        // nothing missing says nothing
        assert!(tier_note(Missing::Nothing).is_empty());
    }

    #[test]
    fn the_subscription_states_both_tiers_when_they_disagree() {
        let p = palette();
        let d = Data {
            live: Some(serde_json::json!({
                "currentTier": {"id": "free-tier", "name": "Gemini Code Assist for individuals"},
                "paidTier": {"name": "Google AI Ultra"},
                "cloudaicompanionProject": "example-project",
            })),
            auth: "oauth-personal".into(),
            ..Data::default()
        };
        let shown = antigravity_plan_rows(&d, 90, &p).join(" ");
        assert!(shown.contains("Gemini Code Assist for individuals"));
        assert!(shown.contains("free-tier"));
        assert!(shown.contains("Google AI Ultra"));
        assert!(shown.contains("example-project"));
        assert!(shown.contains("oauth-personal"));
    }

    #[test]
    fn no_tier_at_all_draws_no_subscription_block() {
        let p = palette();
        assert!(antigravity_plan_rows(&Data::default(), 90, &p).is_empty());
        let hollow = Data {
            live: Some(serde_json::json!({"currentTier": {}, "paidTier": {}})),
            ..Data::default()
        };
        assert!(antigravity_plan_rows(&hollow, 90, &p).is_empty());
    }

    #[test]
    fn only_the_executable_counts_as_the_language_server() {
        assert!(is_language_server("/opt/example/bin/antigravity --serve"));
        assert!(is_language_server("agy chat"));
        assert!(is_language_server("/opt/example/language_server_linux_x64"));
        // A word boundary, so the hyphenated CLI still matches.
        assert!(is_language_server("/opt/example/antigravity-cli"));
        // Named in an argument rather than run: not this process.
        assert!(!is_language_server("grep -rn antigravity /tmp/notes"));
        assert!(!is_language_server("vim antigravity.md"));
        assert!(!is_language_server("/opt/example/bin/antigravityish"));
        assert!(!is_language_server("/opt/example/bin/agy_helper"));
        assert!(!is_language_server(""));
    }

    #[test]
    fn only_listening_sockets_we_own_yield_a_port() {
        // An invented /proc/net/tcp: two listeners and one established
        // connection, of which one listener is ours.
        let table = "\
  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode
   0: 0100007F:9C40 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 4242
   1: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 9999
   2: 0100007F:9C41 0100007F:D431 01 00000000:00000000 00:00000000 00000000  1000        0 4243
";
        let mine: HashSet<String> = ["4242", "4243"].iter().map(|s| s.to_string()).collect();
        assert_eq!(listening_ports(table, &mine), vec![40000]);
        assert!(listening_ports(table, &HashSet::new()).is_empty());
    }

    #[test]
    fn every_section_draws_at_every_pane_width() {
        // Two of the three widths here subtract a constant from the pane
        // before handing the remainder to a bar, and a pane narrower than
        // the constant is exactly how a widget ends up on the floor.
        let p = palette();
        let cfg = Config::default();
        let d = Data {
            quota_from: From::Local,
            quota_why: String::new(),
            quota: groups(&[
                ("Gemini 3 Pro", "weekly", 0.25),
                ("Gemini 3 Pro", "5h", 0.996),
            ]),
            live: Some(serde_json::json!({
                "currentTier": {"id": "free-tier", "name": "Gemini Code Assist for individuals"},
                "paidTier": {"name": "Google AI Ultra"},
            })),
            auth: "oauth-personal".into(),
            sessions: 9,
            counted: 7,
            steps: 1234.0,
            prompts: 42,
            last: now() - 3600.0,
            tier_why: Missing::Nothing,
            tier_said: String::new(),
        };
        for w in [20usize, 40, 80, 200] {
            let plain = tab(&d, w, 40, &cfg, &p).join("\n");
            for want in ["QUOTA", "ACTIVITY", "SUBSCRIPTION"] {
                assert!(plain.contains(want), "{} missing at width {}", want, w);
            }
        }
    }

    #[test]
    fn a_window_the_server_does_not_name_has_no_pace() {
        assert_eq!(window_secs("weekly"), Some(7.0 * 86400.0));
        assert_eq!(window_secs("5h"), Some(5.0 * 3600.0));
        assert_eq!(window_secs("monthly"), None);
        assert_eq!(window_secs("?"), None);
    }
}
