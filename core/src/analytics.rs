// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! One count of starts, sent the same way to both collectors.
//!
//! The launcher and every widget call [`on_launch`] once, after help has
//! already exited, so a `--help` process is not a start. The choice lives
//! in the `analytics` section of config.json. The time of that choice lives
//! in the state directory, and a later launch does not ask again.
//!
//! A destination whose collect URL is empty is skipped. The other still
//! sends. A failed send is dropped on purpose: this is not a source the
//! pane is drawing, and an error row would make a working widget look
//! broken.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use serde_json::{json, Value};

const EVENT: &str = "opscope_start";
const STATE_FILE: &str = "usage.json";

/// Google Analytics 4's Measurement Protocol endpoint. Fixed and public, so
/// it is not a setting: a configurable URL is one that can be pointed at
/// plain HTTP, and the api secret travels in its query string.
const GA_COLLECT: &str = "https://www.google-analytics.com/mp/collect";

/// What this launch should do, before any file is written.
///
/// `On` sends a start and does not touch the timestamp: the choice was
/// already made, and a later launch must not send it again. `Off` with no
/// timestamp still has to send the one opt-out, which is [`Planned::OptOut`].
/// Absent is [`Planned::Ask`], including when a timestamp was left behind
/// without a choice.
#[derive(Debug, PartialEq, Eq)]
enum Planned {
    Ask,
    Quiet,
    Start,
    OptOut,
}

fn planned(choice: Option<bool>, stamped: bool) -> Planned {
    match (choice, stamped) {
        (None, _) => Planned::Ask,
        (Some(false), false) => Planned::OptOut,
        (Some(false), true) => Planned::Quiet,
        (Some(true), _) => Planned::Start,
    }
}

/// The executable's file name, with the launcher called `launcher`.
///
/// `opscope ports` replaces itself with the widget binary, so the menu
/// process never gets here and a named launch is only the widget. The menu
/// itself is the binary named `opscope`.
fn canonical_name(binary: &str) -> String {
    let stem = binary.rsplit(['/', '\\']).next().unwrap_or(binary);
    let stem = stem.strip_suffix(".exe").unwrap_or(stem);
    if stem == "opscope" {
        "launcher".to_string()
    } else {
        stem.to_string()
    }
}

/// `analytics.enabled` from a config document, or nothing when it is absent
/// or not a bool. A string `"true"` is not a choice: the splash writes a bool.
pub fn parse_stored_choice(text: &str) -> Option<bool> {
    let value: Value = serde_json::from_str(text).ok()?;
    match value
        .get("analytics")
        .and_then(|section| section.get("enabled"))
    {
        Some(Value::Bool(on)) => Some(*on),
        _ => None,
    }
}

/// The decision time from the state file, or nothing when it is absent.
pub fn parse_decided_at(text: &str) -> Option<u64> {
    let value: Value = serde_json::from_str(text).ok()?;
    value.get("decided_at").and_then(Value::as_u64)
}

struct Fact {
    name: String,
    opted: Option<&'static str>,
    decided_at: Option<u64>,
}

impl Fact {
    fn start(name: &str) -> Self {
        Self {
            name: name.to_string(),
            opted: None,
            decided_at: None,
        }
    }

    fn choice(name: &str, on: bool, at: u64) -> Self {
        Self {
            name: name.to_string(),
            opted: Some(if on { "in" } else { "out" }),
            decided_at: Some(at),
        }
    }
}

/// The fields an event is allowed to carry. `start` is 1, not a count and
/// not a duration. `opted` and `decided_at` are present only on the one
/// choice event.
fn properties(fact: &Fact) -> Value {
    let mut map = serde_json::Map::new();
    map.insert("name".into(), Value::String(fact.name.clone()));
    map.insert("start".into(), json!(1));
    if let Some(opted) = fact.opted {
        map.insert("opted".into(), Value::String(opted.to_string()));
    }
    if let Some(at) = fact.decided_at {
        map.insert("decided_at".into(), json!(at));
    }
    Value::Object(map)
}

/// Scheme and host of a collect URL, so the event can name the destination
/// it was addressed to and nothing about this machine.
fn origin_of(url: &str) -> Option<String> {
    // HTTPS only: an event over plain HTTP can be read, or rewritten, by
    // anyone on the path.
    let (scheme, rest) = url.split_once("://")?;
    if scheme != "https" {
        return None;
    }
    let host = rest.split(['/', '?', '#']).next().unwrap_or("");
    if host.is_empty() || host.contains(' ') {
        return None;
    }
    Some(format!("{scheme}://{host}"))
}

fn encode_query(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char);
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

fn append_query(url: &str, pairs: &[(&str, &str)]) -> String {
    let mut out = url.to_string();
    let mut sep = if url.contains('?') { '&' } else { '?' };
    for (key, value) in pairs {
        out.push(sep);
        sep = '&';
        out.push_str(&encode_query(key));
        out.push('=');
        out.push_str(&encode_query(value));
    }
    out
}

/// A fresh id for one request, then thrown away.
///
/// The Measurement Protocol requires a client id and will not count an
/// event without one. A stored one would be an identity, so each send mints
/// a new pair of numbers and keeps neither.
fn fresh_client_id() -> String {
    static N: AtomicU64 = AtomicU64::new(1);
    let n = N.fetch_add(1, Ordering::Relaxed).max(1);
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(1)
        .max(1);
    format!("{t}.{n}")
}

fn ga_body(props: &Value, client_id: &str) -> Value {
    json!({
        "client_id": client_id,
        "consent": {
            "ad_user_data": "DENIED",
            "ad_personalization": "DENIED"
        },
        "events": [{
            "name": EVENT,
            "params": props
        }]
    })
}

fn vercel_body(props: &Value, origin: &str, now_ms: u64, analytics_id: &str) -> Value {
    let mut body = json!({
        "o": origin,
        "ts": now_ms,
        "sdkn": "opscope",
        "sdkv": crate::version_number(),
        "r": "",
        "en": EVENT,
        "ed": props
    });
    if !analytics_id.is_empty() {
        body["dsn"] = Value::String(analytics_id.to_string());
    }
    body
}

struct Delivery {
    url: String,
    body: String,
    headers: Vec<(String, String)>,
}

/// The same fact, shaped for whichever destinations are configured.
///
/// Vercel is the collect URL alone: the id is sent when present and is not
/// required, because the URL can already name the project. Google Analytics
/// needs the URL, the measurement id and the api secret together; any empty
/// one skips it. The secret is a query parameter, never a property.
fn deliveries(cfg: &Value, fact: &Fact, client_id: &str, now_ms: u64) -> Vec<Delivery> {
    let props = properties(fact);
    let mut out = Vec::new();
    let vercel_url = crate::cfg_str(cfg, "vercel_url", "");
    let vercel_id = crate::cfg_str(cfg, "vercel_analytics_id", "");
    if let Some(origin) = origin_of(&vercel_url) {
        if let Ok(body) = serde_json::to_string(&vercel_body(&props, &origin, now_ms, &vercel_id)) {
            out.push(Delivery {
                url: vercel_url,
                body,
                headers: vec![("content-type".into(), "application/json".into())],
            });
        }
    }
    let measurement_id = crate::cfg_str(cfg, "ga_measurement_id", "");
    let api_secret = crate::cfg_str(cfg, "ga_api_secret", "");
    if !measurement_id.is_empty() && !api_secret.is_empty() {
        let url = append_query(
            GA_COLLECT,
            &[
                ("measurement_id", measurement_id.as_str()),
                ("api_secret", api_secret.as_str()),
            ],
        );
        if let Ok(body) = serde_json::to_string(&ga_body(&props, client_id)) {
            out.push(Delivery {
                url,
                body,
                headers: vec![("content-type".into(), "application/json".into())],
            });
        }
    }
    out
}

fn read_stamp(path: &Path) -> Option<u64> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_decided_at(&text)
}

fn write_stamp(path: &Path, now: u64) -> Result<(), String> {
    let body = format!("{{\"decided_at\":{now}}}\n");
    crate::statefile::write_atomic(path, &body)
}

fn write_enabled(path: &Path, on: bool) -> Result<(), String> {
    let value = Value::Bool(on);
    for _ in 0..8 {
        let raw = if path.exists() {
            std::fs::read_to_string(path).map_err(|e| format!("{}: {e}", path.display()))?
        } else {
            String::new()
        };
        let next = crate::settings::set_json_path(&raw, &["analytics", "enabled"], &value)?;
        match crate::statefile::write_atomic_if(path, &next, &raw) {
            Ok(()) => return Ok(()),
            Err(error) if error.contains("changed again") => continue,
            Err(error) => return Err(error),
        }
    }
    Err(format!("could not write {}", path.display()))
}

/// `true` when this call recorded the opt-out time and should send it.
///
/// The stamp is written before the send. A crash in between loses one event
/// and the next launch stays quiet; the other order would send the opt-out
/// twice.
fn claim_off_stamp(state: &Path, now: u64) -> bool {
    crate::statefile::with_lock(state, || {
        if read_stamp(state).is_some() {
            return false;
        }
        write_stamp(state, now).is_ok()
    })
    .unwrap_or(false)
}

#[derive(Debug)]
enum AfterAsk {
    SendChoice { on: bool },
    SendStart,
    Quiet,
}

/// First answer wins. A second pane that was still on the splash sends a
/// start only when the winner turned counting on, and sends nothing when
/// the winner turned it off — its own key is not a second choice.
/// The error is the reason the choice was not kept, so the splash can say
/// it before it closes: a choice that silently fails to save is asked for
/// again on the next launch with nothing on screen to say why.
fn save_answer(config: &Path, state: &Path, on: bool, now: u64) -> (AfterAsk, Option<String>) {
    let saved = crate::statefile::with_lock(state, || {
        if read_stamp(state).is_some() {
            match read_choice_file(config) {
                Some(true) => return (AfterAsk::SendStart, None),
                Some(false) => return (AfterAsk::Quiet, None),
                None => {}
            }
        }
        if let Err(error) = write_enabled(config, on) {
            return (AfterAsk::Quiet, Some(unsaved_reason(config, &error)));
        }
        if write_stamp(state, now).is_err() {
            // Off is retried by the missing-timestamp path. On is not: a
            // later launch sends a start only, so the choice has to go now.
            // Either way the choice itself is in config.json and is not
            // asked again, so there is nothing to tell the reader.
            return if on {
                (AfterAsk::SendChoice { on: true }, None)
            } else {
                (AfterAsk::Quiet, None)
            };
        }
        (AfterAsk::SendChoice { on }, None)
    });
    saved.unwrap_or_else(|error| (AfterAsk::Quiet, Some(error)))
}

fn read_choice_file(path: &Path) -> Option<bool> {
    let text = std::fs::read_to_string(path).ok()?;
    parse_stored_choice(&text)
}

fn now_secs() -> u64 {
    crate::now().max(0.0) as u64
}

fn now_millis() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

fn send_all(deliveries: Vec<Delivery>) {
    if deliveries.is_empty() {
        return;
    }
    // Detached on purpose. Joining would hold the pane on the collector,
    // and a pane that never appears is indistinguishable from one that
    // failed to start. The send has nowhere to draw a failure: the body is
    // the widget's own data, and a notice here would read as that data
    // being wrong.
    std::thread::spawn(move || {
        for delivery in deliveries {
            let headers: Vec<(&str, &str)> = delivery
                .headers
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();
            let _ = crate::post_answer(&delivery.url, &headers, &delivery.body, 5);
        }
    });
}

fn interactive() -> bool {
    unsafe { libc::isatty(0) == 1 && libc::isatty(1) == 1 }
}

/// Count one start of `binary`, asking first when nobody has chosen.
pub(crate) fn on_launch(binary: &str) {
    static NOTED: AtomicBool = AtomicBool::new(false);
    if NOTED.swap(true, Ordering::AcqRel) {
        return;
    }
    let name = canonical_name(binary);
    if name.is_empty() {
        return;
    }
    let cfg = crate::load_config("analytics");
    let choice = cfg.get("enabled").and_then(|v| v.as_bool());
    let state = crate::state_dir().join(STATE_FILE);
    let config = crate::resolved_config_path().unwrap_or_else(crate::default_config_path);
    let stamped = read_stamp(&state).is_some();
    let fact = match planned(choice, stamped) {
        Planned::Quiet => None,
        Planned::Start => Some(Fact::start(&name)),
        Planned::OptOut => {
            let now = now_secs();
            if claim_off_stamp(&state, now) {
                Some(Fact::choice(&name, false, now))
            } else {
                None
            }
        }
        Planned::Ask => {
            if !interactive() {
                None
            } else if let Some(fact) = ask(|on| {
                let now = now_secs();
                let (after, error) = save_answer(&config, &state, on, now);
                let fact = match after {
                    AfterAsk::SendChoice { on } => Some(Fact::choice(&name, on, now)),
                    AfterAsk::SendStart => Some(Fact::start(&name)),
                    AfterAsk::Quiet => None,
                };
                (fact, error)
            }) {
                fact
            } else {
                None
            }
        }
    };
    let Some(fact) = fact else {
        return;
    };
    // Read again so a URL written while the splash was up is the one used.
    let cfg = crate::load_config("analytics");
    send_all(deliveries(&cfg, &fact, &fresh_client_id(), now_millis()));
}

fn clamped_scroll(scroll: usize, len: usize, room: usize) -> usize {
    scroll.min(len.saturating_sub(room))
}

fn scrolled(scroll: usize, key: &str, len: usize, room: usize) -> usize {
    let next = match key {
        "ctrl-y" | "wheel-up" => scroll.saturating_sub(1),
        "ctrl-e" | "wheel-down" => scroll.saturating_add(1),
        _ => scroll,
    };
    clamped_scroll(next, len, room)
}

const PROSE: &[&str] = &[
    "Starts are counted so the project can be supported and so we can see which widgets are used.",
    "An event carries the widget or launcher name and that a start happened. This choice is sent once and also says whether you opted in or opted out.",
    "The time of the choice is stored on this machine. Once you choose, you are not asked again.",
    "Accepting turns counting on. Later starts send only that a start happened. Declining turns it off and still sends this one opt-out, then nothing further while counting stays off.",
    "Nothing in an event is a hostname, an account, the contents of config, or an identity. The choice is saved in config.json and can be edited later.",
    "Leave without choosing and this question stays until you answer it.",
];

fn wrap_paragraph(text: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut line = String::new();
    for word in text.split_whitespace() {
        let mut parts = split_long(word, width);
        if parts.is_empty() {
            continue;
        }
        let first = parts.remove(0);
        push_word(&mut lines, &mut line, &first, width);
        for part in parts {
            if !line.is_empty() {
                lines.push(std::mem::take(&mut line));
            }
            line = part;
        }
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

fn push_word(lines: &mut Vec<String>, line: &mut String, word: &str, width: usize) {
    if line.is_empty() {
        *line = word.to_string();
        return;
    }
    let next = format!("{line} {word}");
    if crate::display_width(&next) <= width {
        *line = next;
    } else {
        lines.push(std::mem::take(line));
        *line = word.to_string();
    }
}

fn split_long(word: &str, width: usize) -> Vec<String> {
    if crate::display_width(word) <= width {
        return vec![word.to_string()];
    }
    let mut out = Vec::new();
    let mut buf = String::new();
    for ch in word.chars() {
        buf.push(ch);
        if crate::display_width(&buf) >= width {
            out.push(std::mem::take(&mut buf));
        }
    }
    if !buf.is_empty() {
        out.push(buf);
    }
    out
}

fn splash_lines(width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    for (i, paragraph) in PROSE.iter().enumerate() {
        if i > 0 {
            lines.push(String::new());
        }
        lines.extend(wrap_paragraph(paragraph, width));
    }
    lines
}

/// Ask, and save the answer before the splash closes, so a save that failed
/// is said on the splash rather than discovered by being asked again.
fn ask<T>(save: impl FnOnce(bool) -> (T, Option<String>)) -> Option<T> {
    if !interactive() {
        return None;
    }
    crate::setup();
    let mut keyboard = crate::Keyboard::new();
    let answer = ask_loop(&mut keyboard);
    let (after, error) = save(answer);
    if let Some(error) = error {
        show_unsaved(&mut keyboard, &error);
    }
    keyboard.restore();
    crate::restore_screen();
    Some(after)
}

/// The splash's last screen when the choice could not be written: why, and
/// that the question comes back. Waits for any key, so it is read.
fn show_unsaved(keyboard: &mut crate::Keyboard, error: &str) {
    let lines = unsaved_lines(error);
    loop {
        let (w, h) = crate::size();
        let accent = crate::rgb(150, 210, 255);
        let warn = crate::rgb(255, 200, 120);
        let dim = crate::rgb(127, 147, 172);
        let mut frame = vec![crate::title("help improve opscope", w, &accent)];
        for line in lines.iter().flat_map(|l| wrap_paragraph(l, w.saturating_sub(2))) {
            frame.push(crate::seg(&[(warn.as_str(), format!(" {line}"))], w));
        }
        frame.push(String::new());
        frame.push(crate::seg(&[(dim.as_str(), " any key continues".to_string())], w));
        while frame.len() < h {
            frame.push(String::new());
        }
        frame.truncate(h);
        crate::draw(&frame, w, h);
        keyboard.wait(std::time::Duration::from_millis(200));
        if !keyboard.poll().is_empty() {
            return;
        }
    }
}

/// The file the reader would fix and the OS's reason, without the
/// temporary file the atomic write was using when it failed.
fn unsaved_reason(config: &Path, error: &str) -> String {
    let reason = error.rsplit(": ").next().unwrap_or(error);
    format!("{} could not be written ({reason})", config.display())
}

fn unsaved_lines(error: &str) -> Vec<String> {
    vec![
        format!("Your choice could not be saved: {error}."),
        "Nothing was sent. Make the file writable, or set analytics.enabled to true or false in it yourself; until then you will be asked again on the next launch.".to_string(),
    ]
}

fn ask_loop(keyboard: &mut crate::Keyboard) -> bool {
    let mut scroll = 0usize;
    loop {
        let (w, h) = crate::size();
        let accent = crate::rgb(150, 210, 255);
        let text = crate::rgb(225, 235, 245);
        let dim = crate::rgb(127, 147, 172);
        let body = splash_lines(w);
        let hints = vec![
            vec![(dim.as_str(), "[a]ccept".to_string())],
            vec![(dim.as_str(), "[d]ecline".to_string())],
        ];
        let packed = crate::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
        let foot: Vec<String> = packed.lines.iter().map(|line| format!(" {line}")).collect();
        let room = h.saturating_sub(foot.len());
        let room_below = room.saturating_sub(1).max(1);
        scroll = clamped_scroll(scroll, body.len(), room_below);
        let mut frame = vec![crate::title("help improve opscope", w, &accent)];
        for line in body.iter().skip(scroll).take(room_below) {
            frame.push(if line.is_empty() {
                String::new()
            } else {
                crate::seg(&[(text.as_str(), line.clone())], w)
            });
        }
        while frame.len() < room {
            frame.push(String::new());
        }
        let foot_top = frame.len();
        frame.extend(foot);
        frame.truncate(h);
        crate::draw(&frame, w, h);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(std::time::Duration::from_millis(200));
        for key in keyboard.poll() {
            match key.as_str() {
                "a" | "A" => return true,
                "d" | "D" => return false,
                other => scroll = scrolled(scroll, other, body.len(), room_below),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_save_names_config_json_not_the_temporary_file() {
        let reason = unsaved_reason(
            Path::new("/home/someone/.config/opscope/config.json"),
            "/home/someone/.config/opscope/.config.json.tmp-1-2-0: Permission denied (os error 13)",
        );
        assert_eq!(
            reason,
            "/home/someone/.config/opscope/config.json could not be written (Permission denied (os error 13))"
        );
        assert!(!reason.contains(".tmp-"));
    }

    #[test]
    fn the_choice_matrix_matches_the_decision() {
        assert_eq!(planned(None, false), Planned::Ask);
        assert_eq!(planned(None, true), Planned::Ask);
        assert_eq!(planned(Some(false), false), Planned::OptOut);
        assert_eq!(planned(Some(false), true), Planned::Quiet);
        assert_eq!(planned(Some(true), false), Planned::Start);
        assert_eq!(planned(Some(true), true), Planned::Start);
    }

    #[test]
    fn a_start_carries_the_name_and_that_it_started() {
        let props = properties(&Fact::start("ports"));
        assert_eq!(props["name"], "ports");
        assert_eq!(props["start"], 1);
        assert!(props.get("opted").is_none());
        assert!(props.get("decided_at").is_none());
        let keys: Vec<&str> = props
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(keys, ["name", "start"]);
    }

    #[test]
    fn the_choice_is_one_event_and_it_includes_the_time() {
        let props = properties(&Fact::choice("launcher", true, 1_700_000_000));
        assert_eq!(props["name"], "launcher");
        assert_eq!(props["start"], 1);
        assert_eq!(props["opted"], "in");
        assert_eq!(props["decided_at"], 1_700_000_000);
        let declined = properties(&Fact::choice("ports", false, 42));
        assert_eq!(declined["opted"], "out");
        assert_eq!(declined["decided_at"], 42);
        assert_eq!(declined.as_object().unwrap().len(), 4);
    }

    #[test]
    fn the_launcher_is_not_reported_as_a_widget() {
        assert_eq!(canonical_name("opscope"), "launcher");
        assert_eq!(canonical_name("/usr/local/bin/opscope"), "launcher");
        assert_eq!(canonical_name("ports"), "ports");
        assert_eq!(canonical_name("github-prs"), "github-prs");
        assert_eq!(canonical_name("ports.exe"), "ports");
    }

    #[test]
    fn a_missing_or_mistyped_choice_is_not_a_choice() {
        assert_eq!(parse_stored_choice("{}"), None);
        assert_eq!(parse_stored_choice("{\"clocks\":{}}"), None);
        assert_eq!(
            parse_stored_choice("{\"analytics\":{\"enabled\":true}}"),
            Some(true)
        );
        assert_eq!(
            parse_stored_choice("{\"analytics\":{\"enabled\":false}}"),
            Some(false)
        );
        assert_eq!(
            parse_stored_choice("{\"analytics\":{\"enabled\":\"true\"}}"),
            None
        );
        assert_eq!(parse_decided_at("not json"), None);
        assert_eq!(parse_decided_at("{}"), None);
        assert_eq!(
            parse_decided_at("{\"decided_at\":1700000000}\n"),
            Some(1_700_000_000)
        );
    }

    #[test]
    fn a_collect_url_that_is_not_https_is_never_sent_to() {
        let fact = Fact::start("latency");
        for url in ["http://collect.example/event", "ftp://collect.example/event"] {
            let cfg = json!({ "vercel_url": url, "vercel_analytics_id": "id-test" });
            assert!(deliveries(&cfg, &fact, "1.2", 1).is_empty(), "{url} was sent to");
        }
        // Google's endpoint is built in, and HTTPS.
        let ga = json!({ "ga_measurement_id": "mid-test", "ga_api_secret": "s" });
        let sent = deliveries(&ga, &fact, "1.2", 1);
        assert_eq!(sent.len(), 1);
        assert!(sent[0].url.starts_with("https://www.google-analytics.com/mp/collect?"));
    }

    #[test]
    fn an_unconfigured_destination_is_skipped_and_the_other_still_sends() {
        let fact = Fact::start("latency");
        let both = json!({
            "vercel_url": "https://collect.example/event",
            "vercel_analytics_id": "id-test",
            "ga_measurement_id": "mid-test",
            "ga_api_secret": "secret/test"
        });
        let sent = deliveries(&both, &fact, "10.20", 99);
        assert_eq!(sent.len(), 2);
        assert_eq!(sent[0].url, "https://collect.example/event");
        let vercel: Value = serde_json::from_str(&sent[0].body).unwrap();
        assert_eq!(vercel["en"], EVENT);
        assert_eq!(vercel["ed"]["name"], "latency");
        assert_eq!(vercel["ed"]["start"], 1);
        assert!(vercel["ed"].get("opted").is_none());
        assert_eq!(vercel["o"], "https://collect.example");
        assert_eq!(vercel["dsn"], "id-test");
        assert_eq!(vercel["r"], "");
        assert!(!sent[0].body.contains("secret"));
        assert!(sent[1].url.starts_with(GA_COLLECT), "{}", sent[1].url);
        assert!(sent[1].url.contains("measurement_id=mid-test"));
        assert!(sent[1].url.contains("api_secret=secret%2Ftest"));
        let ga: Value = serde_json::from_str(&sent[1].body).unwrap();
        assert_eq!(ga["client_id"], "10.20");
        assert_eq!(ga["events"][0]["name"], EVENT);
        assert_eq!(ga["events"][0]["params"], vercel["ed"]);
        assert_eq!(ga["consent"]["ad_user_data"], "DENIED");
        assert!(ga.get("user_id").is_none());
        assert!(!sent[1].body.contains("secret"));

        let vercel_only = json!({"vercel_url": "https://collect.example/event"});
        let one = deliveries(&vercel_only, &fact, "1.2", 1);
        assert_eq!(one.len(), 1);
        assert!(serde_json::from_str::<Value>(&one[0].body)
            .unwrap()
            .get("dsn")
            .is_none());

        let ga_half = json!({
            "ga_measurement_id": "mid-test"
        });
        assert!(deliveries(&ga_half, &fact, "1.2", 1).is_empty());
        assert!(deliveries(&json!({}), &fact, "1.2", 1).is_empty());
        assert!(deliveries(
            &json!({"vercel_url": "collect.example/event", "vercel_analytics_id": "id-test"}),
            &fact,
            "1.2",
            1
        )
        .is_empty());
    }

    #[test]
    fn a_client_id_is_two_numbers_and_not_reused() {
        let first = fresh_client_id();
        let second = fresh_client_id();
        assert_ne!(first, second);
        for id in [&first, &second] {
            let (left, right) = id.split_once('.').unwrap();
            assert!(!left.is_empty() && left.chars().all(|c| c.is_ascii_digit()));
            assert!(!right.is_empty() && right.chars().all(|c| c.is_ascii_digit()));
            assert_ne!(left, "0");
        }
    }

    #[test]
    fn the_shipped_settings_carry_no_collect_url_and_no_id() {
        let settings: Value =
            serde_json::from_str(include_str!("../analytics.settings.json")).unwrap();
        for key in [
            "vercel_url",
            "vercel_analytics_id",
            "ga_measurement_id",
            "ga_api_secret",
        ] {
            assert_eq!(settings[key], "", "{key}");
        }
        // Absent, so the first launch asks; a shipped answer would be one
        // nobody gave.
        assert!(settings.get("enabled").is_none(), "enabled is not shipped");
    }

    #[test]
    fn the_splash_says_what_is_sent_and_that_the_time_is_stored() {
        let joined = PROSE.join(" ");
        for phrase in [
            "supported",
            "which widgets are used",
            "widget or launcher name",
            "a start happened",
            "opted in",
            "opted out",
            "time of the choice is stored",
            "not asked again",
            "one opt-out",
            "nothing further",
            "hostname",
            "config.json",
            "can be edited later",
        ] {
            assert!(joined.contains(phrase), "missing {phrase}");
        }
        assert!(!joined.to_lowercase().contains("remind"));
        assert!(!joined.contains("weeks"));
        for width in [8usize, 20, 40, 80] {
            for line in splash_lines(width) {
                assert!(
                    crate::display_width(&line) <= width,
                    "width {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn scrolling_moves_the_view_and_not_a_choice() {
        assert_eq!(scrolled(3, "wheel-up", 10, 4), 2);
        assert_eq!(scrolled(3, "ctrl-e", 10, 4), 4);
        assert_eq!(scrolled(0, "wheel-up", 10, 4), 0);
        assert_eq!(scrolled(20, "wheel-down", 10, 4), 6);
        assert_eq!(scrolled(1, "a", 10, 4), 1);
        assert_eq!(scrolled(1, "d", 10, 4), 1);
    }

    fn temp_dir(tag: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-analytics-{tag}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn accepting_writes_the_choice_and_the_time_without_dropping_other_sections() {
        let dir = temp_dir("accept");
        let config = dir.join("config.json");
        let state = dir.join("usage.json");
        std::fs::write(&config, "{\n  \"clocks\": {\"show_hints\": true}\n}\n").unwrap();
        match save_answer(&config, &state, true, 1_700_000_111).0 {
            AfterAsk::SendChoice { on: true } => {}
            other => panic!("expected the choice to be sent, got {other:?}"),
        }
        let raw = std::fs::read_to_string(&config).unwrap();
        assert!(raw.contains("show_hints"));
        assert_eq!(parse_stored_choice(&raw), Some(true));
        assert_eq!(read_stamp(&state), Some(1_700_000_111));
        // A second pane does not overwrite the choice, and it counts a start.
        match save_answer(&config, &state, false, 1_700_000_999).0 {
            AfterAsk::SendStart => {}
            other => panic!("expected a start once the choice was already on, got {other:?}"),
        }
        assert_eq!(
            parse_stored_choice(&std::fs::read_to_string(&config).unwrap()),
            Some(true)
        );
        assert_eq!(read_stamp(&state), Some(1_700_000_111));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn declining_then_a_later_off_launch_sends_nothing() {
        let dir = temp_dir("decline");
        let config = dir.join("config.json");
        let state = dir.join("usage.json");
        match save_answer(&config, &state, false, 50).0 {
            AfterAsk::SendChoice { on: false } => {}
            other => panic!("expected the opt-out, got {other:?}"),
        }
        assert_eq!(
            parse_stored_choice(&std::fs::read_to_string(&config).unwrap()),
            Some(false)
        );
        assert!(
            !claim_off_stamp(&state, 99),
            "the opt-out time was already stored"
        );
        assert_eq!(read_stamp(&state), Some(50));
        assert_eq!(planned(Some(false), true), Planned::Quiet);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_existing_opt_out_with_no_time_is_stamped_once() {
        let dir = temp_dir("backfill");
        let state = dir.join("usage.json");
        assert!(claim_off_stamp(&state, 70));
        assert_eq!(read_stamp(&state), Some(70));
        assert!(!claim_off_stamp(&state, 80));
        assert_eq!(read_stamp(&state), Some(70));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
