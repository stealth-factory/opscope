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

/// Each model's own allowance, on the same host and the same credential.
///
/// Asked only when the summary carries no Gemini group - which is every
/// time Google's summary is the one read, since Google's has only ever held
/// the Claude and GPT-OSS group. The grouped Gemini limit came from the
/// local language server alone, and `agy` stopped serving that to outside
/// callers. This is the Gemini quota Google still publishes, per model.
const REMOTE_MODEL_QUOTA_API: &str =
    "https://cloudcode-pa.googleapis.com/v1internal:retrieveUserQuota";

/// The Connect method the language server answers the quota on.
const ANTIGRAVITY_RPC: &str =
    "/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary";

/// How long each window the server names actually is. It reports the name
/// and the fraction left but not the span, and without the span there is no
/// pace to compare a fill against.
const ANTIGRAVITY_WINDOWS: &[(&str, f64)] = &[("5h", 5.0 * 3600.0), ("weekly", 7.0 * 86400.0)];

fn window_secs(name: &str) -> Option<f64> {
    ANTIGRAVITY_WINDOWS
        .iter()
        .find(|(k, _)| *k == name)
        .map(|(_, s)| *s)
}

fn ordered_buckets(group: &serde_json::Value) -> Vec<&serde_json::Value> {
    let mut buckets: Vec<_> = group["buckets"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|b| b["remainingFraction"].as_f64().is_some())
        .collect();
    buckets.sort_by_key(|bucket| {
        ANTIGRAVITY_WINDOWS
            .iter()
            .position(|(name, _)| *name == text(bucket, "window"))
            .unwrap_or(ANTIGRAVITY_WINDOWS.len())
    });
    buckets
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
    /// True when a language server was running and refused for want of its
    /// CSRF token. The app is open, so "the app is not running" would be a
    /// false reason for reading Google's summary instead.
    local_refused: bool,
    /// Each Gemini model's own allowance, read only when the summary has no
    /// Gemini group. Per model, never a group limit.
    gemini: Vec<crate::parse::ModelQuota>,
    /// Why there are no Gemini readings when the summary needed them.
    /// Empty when there are, or when the summary had its own Gemini group.
    gemini_why: String,
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
/// `agy` or `antigravity` at the very start of the line or straight after a
/// path separator, or `language_server` anywhere. Anchoring to the executable
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
///
/// A server that refuses for want of its CSRF token is recorded rather than
/// skipped, as `{"opscope_refusal": "csrf"}`: `agy` builds from October
/// 2026 refuse every outside caller that way, and passing over it silently
/// left the Google summary on screen with nothing to say why the app's own
/// Gemini group had gone. A real answer on another port still wins.
fn antigravity_quota() -> Option<serde_json::Value> {
    let mut refused = false;
    for port in antigravity_ports() {
        let url = format!("http://127.0.0.1:{}{}", port, ANTIGRAVITY_RPC);
        let Ok((status, text, _)) =
            tc::post_answer(&url, &[("Content-Type", "application/json")], "{}", 5)
        else {
            continue;
        };
        if !(200..300).contains(&status) {
            refused |= crate::parse::parse_antigravity_csrf_refusal(&text);
            continue;
        }
        let Ok(got) = serde_json::from_str::<serde_json::Value>(&text) else {
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
    refused.then(|| serde_json::json!({ "opscope_refusal": "csrf" }))
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
        return Err(tc::missing_config(
            "asking Google is off - set agent_usage.antigravity_remote to true",
        ));
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
    let local = cached(caches, "antigravity-quota", LIVE_TTL, antigravity_quota);
    let mut d = Data {
        tier_why: if live.is_some() { Missing::Nothing } else { why_no_tier() },
        tier_said: said,
        live,
        local_refused: local
            .as_ref()
            .is_some_and(|v| text(v, "opscope_refusal") == "csrf"),
        quota: local
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
    // A summary with no Gemini group is Google's - the local server is the
    // only source that ever carried one - so the per-model readings are the
    // Gemini quota there is. Asked on the same terms as the summary: never
    // when asking Google is off, never with a lapsed token.
    //
    // A failure goes back to the cache as a failure, so it keeps the short
    // backoff rather than being held for the hour a reading is; its reason
    // is written into the slot afterwards so it lasts as long as the failure.
    if !d.quota.is_empty() && !has_gemini_group(&d.quota) {
        match remote_token(cfg.antigravity_remote) {
            Err(said) => d.gemini_why = said,
            Ok(access) => {
                const KEY: &str = "antigravity-models";
                let mut asked = String::new();
                let got = cached(caches, KEY, PLAN_TTL, || {
                    match post_body(REMOTE_MODEL_QUOTA_API, &access, "{}") {
                        Ok(v) => Some(v),
                        Err(said) => {
                            asked = said;
                            None
                        }
                    }
                });
                remember_refusal(caches, KEY, &asked);
                let got = got.unwrap_or_default();
                let refused = match text(&got, "why") {
                    held if held.is_empty() => asked,
                    held => held,
                };
                match crate::parse::parse_antigravity_model_quota(&got.to_string()) {
                    _ if !refused.is_empty() => {
                        d.gemini_why = format!("Google refused the per-model quota: {}", refused)
                    }
                    Some(models) if !models.is_empty() => d.gemini = models,
                    Some(_) => d.gemini_why = "Google lists no Gemini models for this account".into(),
                    None => d.gemini_why = "Google's per-model quota came back unreadable".into(),
                }
            }
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
        let group_start = out.len();
        for bucket in ordered_buckets(group) {
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
                apart: group_start > 0 && out.len() == group_start,
            });
        }
    }
    // Per-model Gemini readings stand in for a Gemini group only when there
    // is none, and the summary gets one lane of them: the busiest model,
    // named as that. Every model would be a dozen lanes on a screen meant
    // for comparing agents, and an average would be a number Google never
    // published.
    if let Some(top) = d
        .gemini
        .iter()
        .min_by(|a, b| a.left.partial_cmp(&b.left).unwrap_or(std::cmp::Ordering::Equal))
    {
        out.push(Lane {
            label: "gemini busiest".into(),
            pct: 100.0 * (1.0 - top.left),
            window_secs: None,
            reset: top.reset,
            stale: false,
            projected: false,
            apart: !out.is_empty(),
        });
    }
    out
}

/// Whether the summary carries a Gemini group of its own. The local server's
/// does; Google's never has.
fn has_gemini_group(groups: &[serde_json::Value]) -> bool {
    groups
        .iter()
        .any(|g| text(g, "displayName").to_lowercase().starts_with("gemini"))
}

/// One bar per limit, grouped by the model family it covers.
///
/// Shown as spent rather than the remaining fraction the RPC returns, so
/// red means the same here as on every other tab. Which groups there are is
/// whatever the source sends - the local server has sent Gemini beside
/// Claude and GPT-OSS, Google sends only the latter - and each is named
/// with the models Google says it covers, since a heading like "Claude and
/// GPT models" reads wrongly to anyone who does not know the GPT is GPT-OSS.
fn antigravity_quota_rows_with_plots(
    groups: &[serde_json::Value],
    from: From,
    local_refused: bool,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> Vec<String> {
    if groups.is_empty() {
        return Vec::new();
    }
    // The long form names where the number comes from, which matters here
    // more than elsewhere; the short one still says it is not this machine's
    // own tally. Shortened before it can clip, as the other headers are.
    let (mut note, short, tiny) = match from {
        // Running and refusing is not the same as shut, and saying the app
        // is not running while it is open beside the pane is a false reason.
        From::Google if local_refused => (
            " · account-wide, from Google - the app's server refused",
            " · from Google",
            " · remote",
        ),
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
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (p.ok.as_str(), "live".into()),
            (p.dim.as_str(), note.into()),
        ],
        w - 1,
    )];
    if local_refused {
        let mut why = "Antigravity's language server now refuses outside readers: it \
                       wants a CSRF token it gives only to processes it starts itself. \
                       This is Google's summary instead."
            .to_string();
        if !has_gemini_group(groups) {
            why.push_str(" Google publishes no grouped Gemini limit.");
        }
        rows.extend(
            wrap_text(&why, w.saturating_sub(4).max(1))
                .into_iter()
                .map(|line| tc::seg(&[(p.warn.as_str(), format!("  {}", line))], w - 1)),
        );
    }
    for group in groups {
        let buckets = ordered_buckets(group);
        if buckets.is_empty() {
            continue;
        }
        let name = match text(group, "displayName") {
            s if s.is_empty() => "?".to_string(),
            s => s,
        };
        rows.push(tc::seg(&[(p.txt.as_str(), format!("  {}", name))], w - 1));
        // Google's own list of what the group covers - "Claude Opus, Claude
        // Sonnet, GPT-OSS" - which is what settles the heading.
        let covers = crate::parse::strip_controls(&text(group, "description"));
        if !covers.is_empty() {
            rows.extend(
                wrap_text(&covers, w.saturating_sub(6).max(1))
                    .into_iter()
                    .map(|line| tc::seg(&[(p.dim.as_str(), format!("    {}", line))], w - 1)),
            );
        }
        let windows: Vec<String> = buckets
            .iter()
            .map(|b| match text(b, "window") {
                s if s.is_empty() => "?".to_string(),
                s => s,
            })
            .collect();
        let label_w = windows.iter().map(|s| s.chars().count()).max().unwrap_or(1);
        for (bucket, window) in buckets.iter().zip(&windows) {
            let left = bucket["remainingFraction"].as_f64().unwrap_or(0.0);
            let reset = iso_epoch(&text(bucket, "resetTime"));
            let row = quota_bar_row(rows.len(), window, label_w, left, window_secs(window), reset, w, p, plots);
            rows.push(row);
        }
    }
    rows.push(String::new());
    rows
}

/// A model id cut into lines of at most `budget` characters, after its
/// hyphens where it can be and mid-word only where a piece is longer than
/// the line. `gemini-3.5-flash-extra-low` does not fit a 20-column pane even
/// on a row of its own, and a clipped id names a model that does not exist.
fn break_model_id(id: &str, budget: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut line = String::new();
    for piece in id.split_inclusive('-') {
        if !line.is_empty() && line.chars().count() + piece.chars().count() > budget {
            lines.push(std::mem::take(&mut line));
        }
        line.push_str(piece);
        while line.chars().count() > budget {
            let head: String = line.chars().take(budget).collect();
            line = line.chars().skip(budget).collect();
            lines.push(head);
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// What a pace-less bar row needs besides its label: three cells of indent,
/// a space, the bar's eight-cell floor, the six-cell percentage, two cells
/// before the reset, and the column the frame keeps free.
const QUOTA_ROW_FIXED: usize = 21;

/// One limit as a bar: its label, the bar, % spent, pace, and the reset.
#[allow(clippy::too_many_arguments)]
fn quota_bar_row(
    at: usize,
    label: &str,
    label_w: usize,
    left: f64,
    secs: Option<f64>,
    reset: Option<f64>,
    w: usize,
    p: &Palette,
    plots: &mut Vec<tc::Plot>,
) -> String {
    let pct = 100.0 * (1.0 - left);
    let used = (pct / 100.0).clamp(0.0, 1.0);
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
    let hue = agent_hue("antigravity");
    let mut line: Vec<(String, String)> = vec![(
        p.dim.clone(),
        format!("   {} ", tc::pad(label, label_w)),
    )];
    line.extend(paced_bar_with_plot(plots, at, &line,
        used,
        elapsed_of(secs, reset),
        room.max(8) as usize,
        hue,
        p,
    ));
    line.push((pct_colour(pct, hue, p), pct_text(pct)));
    let (pace_colour, pace_txt) =
        pace_cell(lead(pct, secs, reset), pace_guessed(false, false, reset), p);
    line.push((pace_colour, pace_txt));
    line.push((p.dim.clone(), format!("  {}", when)));
    let refs: Vec<(&str, String)> =
        line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    tc::seg(&refs, w - 1)
}

/// Gemini's quota as Google publishes it when there is no Gemini group:
/// one bar per model, each that model's own allowance.
///
/// Headed so it cannot be read as a shared limit, because it is not one -
/// Google states no window for these, so they carry no pace either. When
/// the readings are missing the reason is drawn where they would be, since
/// a summary with no Gemini in it looks like an account that has none.
fn gemini_model_rows(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>, rows: &mut Vec<String>) {
    if d.gemini.is_empty() && d.gemini_why.is_empty() {
        return;
    }
    rows.push(tc::seg(&[(p.txt.as_str(), "  Gemini, per model".to_string())], w - 1));
    let (note, colour) = if d.gemini.is_empty() {
        (format!("No Gemini readings: {}.", d.gemini_why), p.warn.as_str())
    } else {
        (
            "Each model's own allowance, from Google - not slices of one shared limit."
                .to_string(),
            p.dim.as_str(),
        )
    };
    rows.extend(
        wrap_text(&note, w.saturating_sub(6).max(1))
            .into_iter()
            .map(|line| tc::seg(&[(colour, format!("    {}", line))], w - 1)),
    );
    // Model ids run to 26 characters, and a label that wide in a narrow pane
    // pushes the bar and the percentage past the edge, where `seg` clips
    // them - the row fits and the reading is gone. So below the width a bar
    // needs beside its label, each name takes a row of its own and the bar
    // goes underneath it.
    let label_w = d.gemini.iter().map(|m| m.model.chars().count()).max().unwrap_or(1);
    let stacked = label_w + QUOTA_ROW_FIXED > w;
    for m in &d.gemini {
        if stacked {
            for piece in break_model_id(&m.model, w.saturating_sub(4).max(1)) {
                rows.push(tc::seg(&[(p.dim.as_str(), format!("   {}", piece))], w - 1));
            }
            let row = quota_bar_row(rows.len(), "", 0, m.left, None, m.reset, w, p, plots);
            rows.push(row);
        } else {
            let row = quota_bar_row(rows.len(), &m.model, label_w, m.left, None, m.reset, w, p, plots);
            rows.push(row);
        }
    }
    rows.push(String::new());
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

fn antigravity_body_with_plots(d: &Data, w: usize, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    let mut rows =
        antigravity_quota_rows_with_plots(&d.quota, d.quota_from, d.local_refused, w, p, plots);
    gemini_model_rows(d, w, p, plots, &mut rows);
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
pub fn tab_with_plots(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette, plots: &mut Vec<tc::Plot>) -> Vec<String> {
    add_section(antigravity_body_with_plots(d, w, p, plots), antigravity_plan_rows(d, w, p))
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
            "asking Google is off - set agent_usage.antigravity_remote to true",
        ));
        assert!(note.contains("agent_usage.antigravity_remote"), "{}", note);

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
        assert!(off.contains("agent_usage.antigravity_remote"), "{}", off);
        assert!(off.contains(tc::SET_IN_SETTINGS), "{}", off);
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
        assert_eq!(found[0].label, "gemini 5h");
        assert_eq!(found[0].pct, 0.0);
        assert_eq!(found[0].window_secs, Some(5.0 * 3600.0));
        assert_eq!(found[1].label, "gemini weekly");
        assert!((found[1].pct - 75.0).abs() < 1e-9);
        assert_eq!(found[1].window_secs, Some(7.0 * 86400.0));
        assert_eq!(found[2].label, "claude weekly");
        assert_eq!(
            found.iter().map(|l| l.apart).collect::<Vec<_>>(),
            [false, false, true]
        );
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
    fn summary_keeps_model_groups_and_short_windows_first() {
        let state = crate::vendors::State {
            antigravity: Data {
                quota: groups(&[
                    ("Gemini", "weekly", 0.25),
                    ("Gemini", "5h", 1.0),
                    ("Claude", "weekly", 0.1),
                    ("Claude", "5h", 0.5),
                    ("GPT", "weekly", 0.2),
                    ("GPT", "5h", 0.75),
                ]),
                ..Data::default()
            },
            ..crate::vendors::State::default()
        };
        let mut plots = Vec::new();
        let rows = crate::vendors::tab_body_with_plots(
            SUMMARY_TAB,
            &state,
            120,
            40,
            &Config::default(),
            &palette(),
            &["antigravity".into()],
            &mut plots,
        );
        let labels = [
            "gemini 5h",
            "gemini weekly",
            "claude 5h",
            "claude weekly",
            "gpt 5h",
            "gpt weekly",
        ];
        let positions: Vec<_> = labels
            .iter()
            .map(|label| rows.iter().position(|row| row.contains(label)).unwrap())
            .collect();
        assert_eq!(
            positions,
            [
                positions[0],
                positions[0] + 1,
                positions[0] + 3,
                positions[0] + 4,
                positions[0] + 6,
                positions[0] + 7,
            ]
        );
        assert!(rows[positions[2] - 1].is_empty());
        assert!(rows[positions[4] - 1].is_empty());
        assert_eq!(
            plots.iter().map(|plot| plot.y).collect::<Vec<_>>(),
            positions
        );
    }

    /// Google's summary as it actually arrives: the one group, with the
    /// models it covers.
    fn google_summary() -> Vec<serde_json::Value> {
        vec![serde_json::json!({
            "displayName": "Claude and GPT models",
            "description": "Models within this group: Claude Opus, Claude Sonnet, GPT-OSS",
            "buckets": [
                {"bucketId": "3p-weekly", "window": "weekly",
                 "resetTime": "2099-01-01T00:00:00Z", "remainingFraction": 1},
                {"bucketId": "3p-5h", "window": "5h",
                 "resetTime": "2099-01-01T00:00:00Z", "remainingFraction": 1}
            ]
        })]
    }

    fn model(name: &str, left: f64) -> crate::parse::ModelQuota {
        crate::parse::ModelQuota { model: name.into(), left, reset: iso_epoch("2099-01-01T00:00:00Z") }
    }

    #[test]
    fn a_refusing_local_server_is_not_reported_as_an_app_that_is_shut() {
        let p = palette();
        let shown = antigravity_quota_rows_with_plots(
            &google_summary(), From::Google, true, 120, &p, &mut Vec::new(),
        )
        .join("\n");
        assert!(!shown.contains("not running"), "the app is open: {}", shown);
        assert!(shown.contains("refused"), "{}", shown);
        assert!(shown.contains("CSRF token"), "{}", shown);
        assert!(shown.contains("no grouped Gemini limit"), "{}", shown);
        // Without a refusal, the old reason stands.
        let shut = antigravity_quota_rows_with_plots(
            &google_summary(), From::Google, false, 120, &p, &mut Vec::new(),
        )
        .join("\n");
        assert!(shut.contains("not running"), "{}", shut);
        assert!(!shut.contains("CSRF"), "{}", shut);
    }

    #[test]
    fn a_group_says_which_models_it_covers() {
        // "Claude and GPT models" reads as a mistake until it says the GPT
        // is GPT-OSS - and Google says so, in the group's description.
        let shown = antigravity_quota_rows(&google_summary(), From::Google, 120, &palette()).join("\n");
        assert!(shown.contains("Claude and GPT models"));
        assert!(shown.contains("Claude Opus, Claude Sonnet, GPT-OSS"), "{}", shown);
    }

    #[test]
    fn gemini_per_model_readings_are_drawn_and_headed_as_per_model() {
        let d = Data {
            live: Some(serde_json::json!({"currentTier": {"id": "free-tier"}})),
            quota: google_summary(),
            quota_from: From::Google,
            local_refused: true,
            gemini: vec![model("gemini-3-flash", 0.75), model("gemini-2.5-pro", 1.0)],
            ..Data::default()
        };
        let mut plots = Vec::new();
        let rows = antigravity_body_with_plots(&d, 120, &palette(), &mut plots);
        let shown = rows.join("\n");
        assert!(shown.contains("Gemini, per model"), "{}", shown);
        assert!(shown.contains("not slices of one shared limit"), "{}", shown);
        let flash = rows.iter().position(|r| r.contains("gemini-3-flash")).unwrap();
        assert!(rows[flash].contains("25%"), "spent, not left: {}", rows[flash]);
        assert!(rows.iter().any(|r| r.contains("gemini-2.5-pro")));
        // Two windows of the group and two models, each with its bar on its
        // own row.
        assert_eq!(plots.len(), 4);
        assert!(plots.iter().any(|plot| plot.y == flash));
    }

    #[test]
    fn missing_gemini_readings_say_why_where_they_would_be() {
        let d = Data {
            live: Some(serde_json::json!({"currentTier": {"id": "free-tier"}})),
            quota: google_summary(),
            quota_from: From::Google,
            gemini_why: "Google refused the per-model quota: the server answered 500".into(),
            ..Data::default()
        };
        let shown = antigravity_body(&d, 120, &palette()).join("\n");
        assert!(shown.contains("Gemini, per model"), "{}", shown);
        assert!(shown.contains("No Gemini readings: Google refused"), "{}", shown);
        // Nothing to say when the summary carried its own Gemini group.
        let grouped = Data { quota: groups(&[("Gemini", "5h", 0.5)]), ..Data::default() };
        assert!(!antigravity_body(&grouped, 120, &palette()).join("\n").contains("per model"));
    }

    #[test]
    fn the_summary_gets_one_gemini_lane_for_the_busiest_model() {
        let d = Data {
            quota: google_summary(),
            gemini: vec![
                model("gemini-2.5-pro", 1.0),
                model("gemini-3-flash", 0.4),
                model("gemini-3.5-flash-low", 0.9),
            ],
            ..Data::default()
        };
        let found = lanes(&d);
        let gemini: Vec<_> = found.iter().filter(|l| l.label.starts_with("gemini")).collect();
        assert_eq!(gemini.len(), 1, "one lane, not one per model");
        assert_eq!(gemini[0].label, "gemini busiest");
        assert!((gemini[0].pct - 60.0).abs() < 1e-9);
        assert!(gemini[0].apart, "not a third window of the Claude group");
        assert!(gemini[0].label.chars().count() <= 16, "the summary's label column");
        // No readings, no lane - never a 0% standing in for a missing one.
        let none = Data { quota: google_summary(), ..Data::default() };
        assert!(lanes(&none).iter().all(|l| !l.label.starts_with("gemini")));
    }

    #[test]
    fn a_model_id_breaks_after_its_hyphens_and_loses_nothing() {
        let id = "gemini-3.5-flash-extra-low";
        assert_eq!(break_model_id(id, 40), [id]);
        assert_eq!(break_model_id(id, 16), ["gemini-3.5-", "flash-extra-low"]);
        // A piece longer than the line is cut mid-word, not dropped.
        let tight = break_model_id(id, 5);
        assert!(tight.iter().all(|l| l.chars().count() <= 5), "{:?}", tight);
        assert_eq!(tight.concat(), id);
    }

    #[test]
    fn only_a_summary_without_gemini_asks_for_the_per_model_readings() {
        assert!(!has_gemini_group(&google_summary()));
        assert!(has_gemini_group(&groups(&[("Gemini Models", "5h", 1.0)])));
    }

    #[test]
    fn quota_charts_put_five_hours_above_weekly_for_every_model() {
        let quota = groups(&[
            ("Gemini", "weekly", 0.25),
            ("Gemini", "5h", 1.0),
            ("Claude", "weekly", 0.1),
            ("Claude", "5h", 0.5),
            ("GPT", "weekly", 0.2),
            ("GPT", "5h", 0.75),
        ]);
        let mut plots = Vec::new();
        let rows =
            antigravity_quota_rows_with_plots(&quota, From::Local, false, 96, &palette(), &mut plots);
        assert_eq!(plots.len(), 6);
        for pair in plots.chunks_exact(2) {
            assert!(rows[pair[0].y].contains("5h"));
            assert!(rows[pair[1].y].contains("weekly"));
            assert_eq!(pair[1].y, pair[0].y + 1);
        }
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
            local_refused: false,
            gemini: Vec::new(),
            gemini_why: String::new(),
        };
        for w in [20usize, 40, 80, 200] {
            let plain = tab(&d, w, 40, &cfg, &p).join("\n");
            for want in ["QUOTA", "ACTIVITY", "SUBSCRIPTION"] {
                assert!(plain.contains(want), "{} missing at width {}", want, w);
            }
        }
        // Google's summary behind a refusing server, with the per-model
        // Gemini rows: the longest model id is wider than a narrow pane's
        // whole bar, and the two notes are prose. No row may outgrow the
        // pane, or the terminal wraps it and scrolls the title away.
        let google = Data {
            quota: google_summary(),
            quota_from: From::Google,
            local_refused: true,
            gemini: vec![model("gemini-3.5-flash-extra-low", 0.5), model("gemini-3-flash", 1.0)],
            ..d.clone()
        };
        for w in [20usize, 26, 40, 80, 200] {
            let rows = tab(&google, w, 40, &cfg, &p);
            let plain = rows.join("\n");
            // Fitting is not enough: a row can fit by having its reading
            // clipped off the end. Every model's name and its percentage
            // must survive at every width.
            for want in ["QUOTA", "Gemini, per model", "ACTIVITY", "50%"] {
                assert!(plain.contains(want), "{} missing at width {}", want, w);
            }
            // A name may be broken across rows below its width, but every
            // character of it must be on screen.
            let visible = |s: &str| {
                let mut out = String::new();
                let mut rest = s.chars();
                while let Some(c) = rest.next() {
                    if c == '\u{1b}' {
                        for c in rest.by_ref() {
                            if c == 'm' {
                                break;
                            }
                        }
                    } else {
                        out.push(c);
                    }
                }
                out
            };
            let joined: String = rows.iter().map(|r| visible(r).trim().to_string()).collect();
            for name in ["gemini-3.5-flash-extra-low", "gemini-3-flash"] {
                assert!(joined.contains(name), "{} cut at width {}: {}", name, w, plain);
            }
            let section: Vec<&String> = rows
                .iter()
                .skip_while(|r| !r.contains("Gemini, per model"))
                .take_while(|r| !r.is_empty())
                .collect();
            let readings = section.iter().filter(|r| r.contains('%')).count();
            assert_eq!(readings, 2, "a Gemini reading was clipped at width {}: {}", w, plain);
            assert!(section.iter().any(|r| r.contains("50%")), "width {}: {}", w, plain);
            for row in &rows {
                assert!(
                    tc::display_width(row) < w,
                    "a row outgrew the {}-column pane: {:?}",
                    w,
                    row
                );
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

#[cfg(test)]
fn antigravity_quota_rows(
    groups: &[serde_json::Value],
    from: From,
    w: usize,
    p: &Palette,
) -> Vec<String> {
    antigravity_quota_rows_with_plots(groups, from, false, w, p, &mut Vec::new())
}

#[cfg(test)]
fn antigravity_body(d: &Data, w: usize, p: &Palette) -> Vec<String> {
    antigravity_body_with_plots(d, w, p, &mut Vec::new())
}

#[cfg(test)]
pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    tab_with_plots(d, w, _h, _cfg, p, &mut Vec::new())
}
