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
            } else if let Some(fact) = ask(&name, |on| {
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

/// Where the code that sends every event lives. The splash links it and
/// copies it, so "read it yourself" costs one key.
pub const SOURCE_URL: &str =
    "https://github.com/stealth-factory/opscope/blob/main/core/src/analytics.rs";

/// The splash's colours. Every one is measured against the background and
/// the accept tint in `every_splash_colour_clears_aa`, because the palette
/// checks in check.rs read widgets and this screen lives in core.
struct Palette {
    accent: String,
    text: String,
    dim: String,
    ok: String,
    warn: String,
    /// The one tinted thing on the screen: the accept hint, so the default
    /// reads as the button it is. Foreground and background together.
    accept: String,
}

impl Palette {
    fn new() -> Self {
        Self {
            accent: crate::rgb(150, 210, 255),
            text: crate::rgb(225, 235, 245),
            dim: crate::rgb(127, 147, 172),
            ok: crate::rgb(130, 220, 160),
            warn: crate::rgb(255, 200, 120),
            accept: format!("{}{}", crate::bg(28, 72, 56), crate::rgb(190, 255, 215)),
        }
    }
}

/// What a `c` press left to say, under the link it copied.
struct Note {
    text: String,
    copied: bool,
}

/// One block of the splash. Prose wraps to the pane; a labelled row keeps
/// its label in a column so `sent` / `never` / `stored` scan down the left.
enum Block {
    Headline(&'static str),
    Para(&'static str),
    Row(&'static str, String),
    Link,
    Quiet(&'static str),
    Blank,
}

/// The copy, in order. `name` is this binary, so the event shown is the one
/// this launch would send and not an example.
fn blocks(name: &str) -> Vec<Block> {
    let event = serde_json::to_string(&properties(&Fact::start(name))).unwrap_or_default();
    vec![
        // A blank row above and below the headline, so it reads as the
        // screen's heading rather than the first line of the paragraph.
        Block::Blank,
        Block::Headline("Help choose which widgets get the attention"),
        Block::Blank,
        Block::Para(
            "Widget launches are tracked anonymously so the project can be supported and so we can see which widgets are used. The widgets you open are the ones that get the attention.",
        ),
        Block::Blank,
        Block::Row("sent", "the widget or launcher name and that a start happened".into()),
        Block::Row("event", event),
        Block::Row(
            "once",
            "this choice, opted in or opted out, as \"opted\" and \"decided_at\", the time of the choice".into(),
        ),
        Block::Row(
            "never",
            "a hostname, an account, the contents of config, or an identity".into(),
        ),
        Block::Row(
            "also",
            "the opscope version (Vercel), a throwaway id for that one send (Google), and the analytics id if config.json sets one".into(),
        ),
        Block::Row(
            "where",
            "Vercel Analytics and Google Analytics, https only; one not set in config.json is skipped".into(),
        ),
        Block::Row(
            "stored",
            "the time of the choice, on this machine, and the choice itself in config.json, where it can be edited later".into(),
        ),
        Block::Blank,
        Block::Para(
            "Accept turns counting on: later starts send only that a start happened. Decline turns it off and still sends this one opt-out, then nothing further while counting stays off. Either way you are not asked again.",
        ),
        Block::Blank,
        Block::Headline("Read the code that sends it. It is this one file:"),
        Block::Link,
        Block::Blank,
        Block::Quiet("Close this without choosing and it is asked again next time."),
    ]
}

/// The widest label, so every row's text starts in the same column.
fn label_width(blocks: &[Block]) -> usize {
    blocks
        .iter()
        .filter_map(|b| match b {
            Block::Row(label, _) => Some(crate::display_width(label)),
            _ => None,
        })
        .max()
        .unwrap_or(0)
}

/// A labelled row: the label in its column, the text wrapped beside it and
/// every later line indented under the first. A pane too narrow for a
/// column puts the label on its own line, so nothing is cut.
fn labelled(p: &Palette, label: &str, text: &str, label_w: usize, w: usize) -> Vec<String> {
    let room = w.saturating_sub(1);
    let beside = label_w + 1 + 16 <= room;
    let text_w = if beside { room - label_w - 1 } else { room };
    let mut out = Vec::new();
    if !beside {
        out.push(crate::seg(&[(p.accent.as_str(), format!(" {label}"))], w));
    }
    for (i, line) in wrap_paragraph(text, text_w).into_iter().enumerate() {
        let lead = if beside && i == 0 {
            format!(" {label:<label_w$} ")
        } else if beside {
            " ".repeat(label_w + 2)
        } else {
            " ".to_string()
        };
        out.push(crate::seg(
            &[(p.accent.as_str(), lead), (p.text.as_str(), line)],
            w,
        ));
    }
    out
}

/// The body at `w` columns, built at whatever height it needs. The frame
/// is a window onto it.
fn splash_rows(name: &str, w: usize, note: Option<&Note>) -> Vec<String> {
    let p = Palette::new();
    let blocks = blocks(name);
    let label_w = label_width(&blocks);
    let room = w.saturating_sub(1);
    let mut rows = Vec::new();
    for block in &blocks {
        match block {
            Block::Headline(text) => {
                for line in wrap_paragraph(text, room) {
                    rows.push(crate::seg(&[(p.accent.as_str(), format!(" {line}"))], w));
                }
            }
            Block::Para(text) => {
                for line in wrap_paragraph(text, room) {
                    rows.push(crate::seg(&[(p.text.as_str(), format!(" {line}"))], w));
                }
            }
            Block::Row(label, text) => rows.extend(labelled(&p, label, text, label_w, w)),
            Block::Link => {
                // Wrapped as plain text first, then each piece made a link
                // to the whole address, so a narrow pane still has every
                // character of it on screen and every piece opens it.
                for piece in wrap_paragraph(SOURCE_URL, room) {
                    let linked = crate::link(SOURCE_URL, &piece);
                    rows.push(crate::seg(&[(p.accent.as_str(), format!(" {linked}"))], w));
                }
                if let Some(note) = note {
                    let colour = if note.copied { &p.ok } else { &p.warn };
                    for line in wrap_paragraph(&note.text, room) {
                        rows.push(crate::seg(&[(colour.as_str(), format!(" {line}"))], w));
                    }
                }
            }
            Block::Quiet(text) => {
                for line in wrap_paragraph(text, room) {
                    rows.push(crate::seg(&[(p.dim.as_str(), format!(" {line}"))], w));
                }
            }
            Block::Blank => rows.push(String::new()),
        }
    }
    rows
}

/// The footer: accept first and tinted, because Enter is what the screen
/// recommends; decline beside it in full body contrast, because a choice
/// that is hard to read is not a choice.
fn hints(p: &Palette) -> Vec<Vec<(&str, String)>> {
    vec![
        vec![
            (p.accept.as_str(), " [\u{21b5}] accept ".to_string()),
            (crate::RST, String::new()),
        ],
        vec![(p.text.as_str(), "[d]ecline".to_string())],
        vec![(p.dim.as_str(), "[c]opy link".to_string())],
    ]
}

/// One frame of the splash: the title pinned, a window onto the body, and
/// the hints pinned under it whatever the height.
struct Frame {
    rows: Vec<String>,
    footer: crate::Footer,
    foot_top: usize,
    /// Body rows the window shows, which is what a scroll is clamped to.
    window: usize,
    body_len: usize,
}

fn frame(name: &str, w: usize, h: usize, scroll: usize, note: Option<&Note>) -> Frame {
    let p = Palette::new();
    let body = splash_rows(name, w, note);
    let packed = crate::pack_hints_placed(&hints(&p), w.saturating_sub(2), "  ");
    let foot: Vec<String> = packed
        .lines
        .iter()
        .map(|line| format!(" {line}{}", crate::RST))
        .collect();
    let room = h.saturating_sub(foot.len());
    let window = room.saturating_sub(1).max(1);
    let scroll = clamped_scroll(scroll, body.len(), window);
    let mut rows = vec![crate::title("help improve opscope", w, &p.accent)];
    rows.extend(body.iter().skip(scroll).take(window).cloned());
    while rows.len() < room {
        rows.push(String::new());
    }
    let foot_top = rows.len();
    rows.extend(foot);
    rows.truncate(h);
    Frame {
        rows,
        footer: packed,
        foot_top,
        window,
        body_len: body.len(),
    }
}

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

/// Ask, and save the answer before the splash closes, so a save that failed
/// is said on the splash rather than discovered by being asked again.
fn ask<T>(name: &str, save: impl FnOnce(bool) -> (T, Option<String>)) -> Option<T> {
    if !interactive() {
        return None;
    }
    crate::setup();
    let mut keyboard = crate::Keyboard::new();
    let answer = ask_loop(&mut keyboard, name);
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

fn ask_loop(keyboard: &mut crate::Keyboard, name: &str) -> bool {
    let mut scroll = 0usize;
    let mut note: Option<Note> = None;
    loop {
        let (w, h) = crate::size();
        let f = frame(name, w, h, scroll, note.as_ref());
        scroll = clamped_scroll(scroll, f.body_len, f.window);
        crate::draw(&f.rows, w, h);
        keyboard.footer_at(&f.footer, f.foot_top, 1);
        keyboard.wait(std::time::Duration::from_millis(200));
        for key in keyboard.poll() {
            match key.as_str() {
                "enter" | "a" | "A" => return true,
                "d" | "D" => return false,
                "c" | "C" => {
                    // The address stays on screen either way: OSC 52 is
                    // refused by some terminals and swallowed by some
                    // multiplexers, and a copy that silently did nothing
                    // would leave nothing to read instead.
                    let copied = crate::clipboard(SOURCE_URL);
                    note = Some(Note {
                        text: if copied {
                            "copied to your clipboard".to_string()
                        } else {
                            "no clipboard here; the link above still opens, or select it".to_string()
                        },
                        copied,
                    });
                }
                other => scroll = scrolled(scroll, other, f.body_len, f.window),
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

    /// The splash as text, every row's escapes stripped, one string.
    fn splash_text(name: &str, w: usize) -> String {
        splash_rows(name, w, None)
            .iter()
            .map(|row| crate::source_text(row).trim().to_string())
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn the_splash_says_what_is_sent_and_that_the_time_is_stored() {
        // Wide enough that no phrase is split across rows.
        let joined = splash_text("clocks", 400);
        for phrase in [
            "supported",
            "which widgets are used",
            "widget or launcher name",
            "a start happened",
            "opted in",
            "opted out",
            "the time of the choice",
            "stored",
            "on this machine",
            "not asked again",
            "one opt-out",
            "nothing further",
            "hostname",
            "an account",
            "contents of config",
            "identity",
            "config.json",
            "can be edited later",
            "https only",
            "Vercel Analytics and Google Analytics",
            SOURCE_URL,
        ] {
            assert!(joined.contains(phrase), "missing {phrase}");
        }
        assert!(!joined.to_lowercase().contains("remind"));
        assert!(!joined.contains("weeks"));
        for width in [8usize, 20, 40, 80] {
            for line in splash_rows("clocks", width, None) {
                assert!(
                    crate::display_width(&line) <= width,
                    "width {width}: {line:?}"
                );
            }
        }
    }

    #[test]
    fn the_event_on_the_splash_is_the_one_a_start_sends() {
        // The literal comes from `properties`, so the screen cannot drift
        // from the code: the row says exactly what a later start carries.
        let joined = splash_text("ports", 400);
        let shown = serde_json::to_string(&properties(&Fact::start("ports"))).unwrap();
        assert_eq!(shown, "{\"name\":\"ports\",\"start\":1}");
        assert!(joined.contains(&shown), "{joined}");
        // And the choice event's two extra fields are named by their keys.
        assert!(joined.contains("\"opted\"") && joined.contains("\"decided_at\""));
        let launcher = splash_text("launcher", 400);
        assert!(launcher.contains("{\"name\":\"launcher\",\"start\":1}"));
    }

    #[test]
    fn the_link_is_clickable_and_whole_at_every_width() {
        // One OSC 8 opener per piece, all to the one address, and the
        // pieces together are the address. A pane narrower than the URL
        // wraps it; nothing is cut.
        for w in [40usize, 60, 80, 200] {
            let rows = splash_rows("clocks", w, None);
            let linked: Vec<&String> = rows
                .iter()
                .filter(|r| r.contains(&format!("\x1b]8;;{SOURCE_URL}\x1b\\")))
                .collect();
            assert!(!linked.is_empty(), "{w}: no link row");
            let text: String = linked
                .iter()
                .map(|r| crate::source_text(r).trim().to_string())
                .collect();
            assert_eq!(text, SOURCE_URL, "{w}");
            for row in &linked {
                assert!(crate::display_width(row) <= w, "{w}: {row:?}");
            }
        }
    }

    #[test]
    fn the_splash_fits_every_pane_from_40x12_to_200x60() {
        let widths = [40usize, 41, 47, 48, 60, 79, 80, 100, 120, 160, 200];
        for w in widths {
            let body = splash_rows("clocks", w, None);
            let first = crate::source_text(&body[0]).trim().to_string();
            let last = body
                .iter()
                .rev()
                .find(|r| !r.is_empty())
                .map(|r| crate::source_text(r).trim().to_string())
                .unwrap();
            for h in 12..=60 {
                let f = frame("clocks", w, h, 0, None);
                assert_eq!(f.rows.len(), h, "{w}x{h}");
                for row in &f.rows {
                    assert!(crate::display_width(row) <= w, "{w}x{h}: {row:?}");
                }
                // The hints are on screen, whole, below the body window.
                assert!(f.foot_top + f.footer.lines.len() <= h, "{w}x{h}: hints off screen");
                let foot: String = f.rows[f.foot_top..]
                    .iter()
                    .map(|r| crate::source_text(r))
                    .collect::<Vec<_>>()
                    .join(" ");
                for hint in ["[\u{21b5}] accept", "[d]ecline", "[c]opy link"] {
                    assert!(foot.contains(hint), "{w}x{h}: {hint} missing from {foot:?}");
                }
                // The window is at least a row, and the title is pinned.
                assert!(f.window >= 1, "{w}x{h}");
                assert!(crate::source_text(&f.rows[0]).contains("HELP IMPROVE OPSCOPE"));
                // The top of the body at scroll 0, the bottom at the end:
                // a short pane is one you scroll, not one that hides.
                assert!(crate::source_text(&f.rows[1]).contains(&first), "{w}x{h}");
                let end = frame("clocks", w, h, usize::MAX, None);
                let shown: Vec<String> = end.rows[1..end.foot_top]
                    .iter()
                    .map(|r| crate::source_text(r).trim().to_string())
                    .collect();
                assert!(shown.contains(&last), "{w}x{h}: last row {last:?} unreachable");
                // The clicks land on the hints as drawn.
                for spot in &end.footer.spots {
                    assert!(end.foot_top + spot.line < h, "{w}x{h}: spot off screen");
                }
                assert!(end.footer.spots.iter().any(|s| s.key == "enter"));
                assert!(end.footer.spots.iter().any(|s| s.key == "d"));
                assert!(end.footer.spots.iter().any(|s| s.key == "c"));
            }
        }
    }

    #[test]
    fn a_copy_note_sits_under_the_link_and_fits() {
        for (copied, word) in [(true, "copied"), (false, "no clipboard")] {
            let note = Note {
                text: if copied {
                    "copied to your clipboard".into()
                } else {
                    "no clipboard here; the link above still opens, or select it".into()
                },
                copied,
            };
            for w in [40usize, 80] {
                let rows = splash_rows("clocks", w, Some(&note));
                let at = rows
                    .iter()
                    .position(|r| crate::source_text(r).contains(word))
                    .unwrap_or_else(|| panic!("{w}: {word} missing"));
                assert!(rows[at - 1].contains("\x1b]8;;"), "{w}: note not under the link");
                for row in &rows {
                    assert!(crate::display_width(row) <= w, "{w}: {row:?}");
                }
            }
        }
    }

    fn luminance(c: (f64, f64, f64)) -> f64 {
        let ch = |v: f64| {
            let v = v / 255.0;
            if v <= 0.04045 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
        };
        0.2126 * ch(c.0) + 0.7152 * ch(c.1) + 0.0722 * ch(c.2)
    }

    fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    /// The first `(r, g, b)` triple after `call` in `s`.
    fn triple(s: &str, call: &str) -> (f64, f64, f64) {
        let at = s.find(call).unwrap_or_else(|| panic!("{call} in {s:?}"));
        let open = s[at..].find('(').unwrap() + at;
        let close = s[open..].find(')').unwrap() + open;
        let n: Vec<f64> = s[open + 1..close]
            .split(',')
            .map(|x| x.trim().parse().unwrap())
            .collect();
        (n[0], n[1], n[2])
    }

    /// Every colour the splash declares, measured: the palette checks in
    /// check.rs read widget sources and never see this screen.
    #[test]
    fn every_splash_colour_clears_aa() {
        let src = include_str!("analytics.rs");
        let body = src.split("impl Palette {").nth(1).unwrap();
        let body = body.split("\n}\n").next().unwrap();
        let background = (0.0, 0.0, 0.0);
        let tint = triple(body, "crate::bg(");
        let mut measured = 0;
        let mut wrong = Vec::new();
        for line in body.lines() {
            let Some((field, _)) = line.trim().split_once(':') else { continue };
            if !line.contains("crate::rgb(") {
                continue;
            }
            let c = triple(line, "crate::rgb(");
            measured += 1;
            let on = if field == "accept" { tint } else { background };
            let r = contrast(c, on);
            if r < 4.5 {
                wrong.push(format!("{field} {c:?} on {on:?} measures {r:.2}"));
            }
        }
        assert_eq!(measured, 6, "a palette field was not measured");
        assert!(wrong.is_empty(), "under AA 4.5:\n{}", wrong.join("\n"));
        // A deliberately bad pair has to fail, or this cannot.
        assert!(contrast((80.0, 30.0, 30.0), background) < 4.5);
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
