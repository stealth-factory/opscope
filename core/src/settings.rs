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

//! The settings screen every configurable widget owns.
//!
//! A widget supplies its own `settings.json`; this module supplies one
//! interaction model and the secure raw-JSON writer. Writes go to the file
//! `load_config` actually reads and patch only the selected value, so key
//! order, comments-as-keys, and concurrent unrelated edits survive.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

/// Everything shared code needs to open one widget's settings.
#[derive(Clone, Copy)]
pub struct SettingsSpec {
    /// Binary name shown in the settings title.
    pub widget: &'static str,
    /// Canonical `config.json` section.
    pub section: &'static str,
    /// Pre-rename section still read when the canonical one is absent.
    pub legacy_section: Option<&'static str>,
    /// The widget-owned JSON object of defaults and `_comment` help.
    pub schema: &'static str,
    /// Tables of candidate keys for map-valued fields, by field name.
    ///
    /// Some maps are keyed by something the widget already knows in code and
    /// cannot sensibly restate in `settings.json` - `agent-usage`'s rate card
    /// is sixty-eight models with their published prices. The widget hands
    /// the table over and the settings screen offers it as a picker, which
    /// keeps one record of the fact and leaves core drawing the screen.
    pub catalogues: &'static [(&'static str, Catalogue)],
}

/// A table a widget owns, as `(key, who publishes it, the numbers each field
/// defaults to)`.
///
/// Shaped like `agent-usage`'s `LIST_RATES` so a widget passes the constant it
/// already has rather than building a second copy of it.
///
/// The middle field is drawn beside the key, because a key does not always say
/// who it belongs to: `o3` and `codex-mini-latest` are OpenAI's,
/// `grok-build-0.1` is xAI's, and nothing in either string says so. It is
/// carried rather than derived from the name - a prefix rule reads fine over
/// today's table and mislabels the first entry that does not follow it.
pub type Catalogue =
    &'static [(&'static str, &'static str, &'static [(&'static str, f64)])];

#[derive(Clone)]
struct Field {
    section: String,
    key: String,
    /// The object keys this one lives inside, outermost first. Empty for
    /// everything at the top of a section.
    ///
    /// The invariant is on *navigation*, not on the JSON: a reader should
    /// never be more than one screen from where they started, because a
    /// screen that can descend for ever is a screen nobody can find their
    /// way out of. A path may still be deeper than that when one screen
    /// shows the leaves of a two-deep object flat - `rates` lists its models
    /// as `model · kind` rows rather than a screen per model, so the path is
    /// three keys long while the walk back is one `esc`.
    parents: Vec<String>,
    help: String,
    default: Value,
}

impl Field {
    fn path(&self) -> String {
        let mut out = self.section.clone();
        for p in &self.parents {
            out.push('.');
            out.push_str(p);
        }
        out.push('.');
        out.push_str(&self.key);
        out
    }

    /// The keys under the section, in order, for the readers and writers.
    fn steps(&self) -> Vec<&str> {
        let mut out: Vec<&str> = self.parents.iter().map(String::as_str).collect();
        out.push(self.key.as_str());
        out
    }

    /// How the row is labelled on a screen that flattens a level: the
    /// enclosing key is part of the name, because `input` alone appears
    /// five times over and says nothing about which model it prices.
    fn label(&self) -> String {
        match self.parents.len() {
            0 | 1 => self.key.clone(),
            _ => format!("{} · {}", self.parents[self.parents.len() - 1], self.key),
        }
    }

    fn widget(&self) -> String {
        self.section.replace('_', "-")
    }

    fn secret(&self) -> bool {
        is_secret(&self.key)
    }

    fn kind(&self) -> &'static str {
        match &self.default {
            Value::Number(number)
                if number.as_u64().is_some() || number.as_i64().is_some() =>
            {
                "integer"
            }
            _ => kind_name(&self.default),
        }
    }
}

fn is_secret(key: &str) -> bool {
    // token_env is the *name* of a variable, not a secret. The tokens
    // themselves are the keys called `token`.
    key == "token" || key.ends_with("_token")
}

fn kind_name(v: &Value) -> &'static str {
    match v {
        Value::Null => "null",
        Value::Bool(_) => "bool",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

fn same_kind(a: &Value, b: &Value) -> bool {
    match (a, b) {
        (Value::Null, Value::Null)
        | (Value::Bool(_), Value::Bool(_))
        | (Value::String(_), Value::String(_)) => true,
        (Value::Number(actual), Value::Number(expected)) => {
            if expected.as_u64().is_some() {
                actual.as_u64().is_some()
            } else if expected.as_i64().is_some() {
                actual.as_i64().is_some()
            } else {
                true
            }
        }
        (Value::Array(actual), Value::Array(expected)) => expected
            .first()
            .is_none_or(|shape| actual.iter().all(|item| same_kind(item, shape))),
        (Value::Object(actual), Value::Object(expected)) => expected
            .values()
            .next()
            .is_none_or(|shape| actual.values().all(|item| same_kind(item, shape))),
        _ => false,
    }
}

fn named_kind(value: &Value, kind: &str) -> bool {
    match kind {
        "string" => value.is_string(),
        "integer" => value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "object" => value.is_object(),
        "number-map" => value
            .as_object()
            .is_some_and(|map| map.values().all(Value::is_number)),
        "array" => value.is_array(),
        _ => false,
    }
}

fn validate_value(
    value: &Value,
    expected: &Value,
    rule: Option<&Value>,
) -> Result<(), String> {
    // A null default means the widget has no figure to offer, not that null
    // is the type it wants - a model the rate card does not carry has every
    // kind and a price for none of them. Demanding null back made those rows
    // unwritable, so the one case config exists for could not be configured.
    if !expected.is_null() && !same_kind(value, expected) {
        return Err(format!(
            "expected {}, got {}",
            kind_name(expected),
            kind_name(value)
        ));
    }
    let Some(rule) = rule.and_then(Value::as_object) else {
        return Ok(());
    };
    if let Some(choices) = rule.get("choices").and_then(Value::as_array) {
        if !choices.contains(value) {
            let names = choices.iter().map(compact).collect::<Vec<_>>().join(", ");
            return Err(format!("expected one of {names}"));
        }
    }
    if let Some(kind) = rule.get("items").and_then(Value::as_str) {
        if let Some(items) = value.as_array() {
            if !items.iter().all(|item| named_kind(item, kind)) {
                return Err(format!("every item must be {kind}"));
            }
        }
    }
    if let Some(kind) = rule.get("values").and_then(Value::as_str) {
        if let Some(values) = value.as_object() {
            if !values.values().all(|item| named_kind(item, kind)) {
                return Err(format!("every value must be {kind}"));
            }
        }
    }
    if let (Some(actual), Some(minimum)) = (
        value.as_f64(),
        rule.get("minimum").and_then(Value::as_f64),
    ) {
        if actual < minimum {
            return Err(format!("must be at least {}", compact(&Value::from(minimum))));
        }
    }
    if let (Some(actual), Some(maximum)) = (
        value.as_f64(),
        rule.get("maximum").and_then(Value::as_f64),
    ) {
        if actual > maximum {
            return Err(format!("must be at most {}", compact(&Value::from(maximum))));
        }
    }
    Ok(())
}

fn constraint_summary(rule: Option<&Value>) -> String {
    let Some(rule) = rule.and_then(Value::as_object) else {
        return String::new();
    };
    let mut parts = Vec::new();
    // First, because it is what the number means. A field showing `200` and
    // a default of `0` says nothing about whether that is dollars a month,
    // dollars a million tokens, or seconds - and the reader has to guess
    // right to set it correctly.
    if let Some(unit) = rule.get("unit").and_then(Value::as_str) {
        parts.push(unit.to_string());
    }
    if let Some(choices) = rule.get("choices").and_then(Value::as_array) {
        parts.push(format!(
            "choices {}",
            choices.iter().map(compact).collect::<Vec<_>>().join(" / ")
        ));
    }
    if let Some(kind) = rule.get("items").and_then(Value::as_str) {
        parts.push(format!("{kind} items"));
    }
    if let Some(kind) = rule.get("values").and_then(Value::as_str) {
        parts.push(format!("{kind} values"));
    }
    if let Some(minimum) = rule.get("minimum") {
        parts.push(format!("min {}", compact(minimum)));
    }
    if let Some(maximum) = rule.get("maximum") {
        parts.push(format!("max {}", compact(maximum)));
    }
    parts.join(" · ")
}

/// One setting per example key, in file order, `_comment` keys excluded.
fn fields_from_example(text: &str) -> Result<Vec<Field>, String> {
    let parsed: Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
    let root = match parsed.as_object() {
        Some(o) => o,
        None => return Err("example is not an object".into()),
    };
    let mut fields = Vec::new();
    for section in object_keys(text, 0)? {
        if section.starts_with('_') {
            continue;
        }
        let Some(body) = root.get(&section).and_then(|v| v.as_object()) else {
            continue;
        };
        let Some(section_open) = find_object_at(text, &[&section])? else {
            continue;
        };
        let section_help = body
            .get("_comment")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        for key in object_keys(text, section_open)? {
            if key.starts_with('_') {
                continue;
            }
            let Some(default) = body.get(&key) else {
                continue;
            };
            let help = comment_for(body, &key)
                .map(|s| s.to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| section_help.clone());
            fields.push(Field {
                section: section.clone(),
                key,
                parents: Vec::new(),
                help,
                default: default.clone(),
            });
        }
    }
    Ok(fields)
}

fn comment_for<'a>(body: &'a serde_json::Map<String, Value>, key: &str) -> Option<&'a str> {
    for name in [
        format!("_{key}_comment"),
        format!("_comment_{key}"),
        format!("_{key}"),
    ] {
        if let Some(text) = body.get(&name).and_then(|v| v.as_str()) {
            return Some(text);
        }
    }
    None
}

fn skip_ws(s: &str, mut i: usize) -> usize {
    while let Some(c) = s[i..].chars().next() {
        if !c.is_whitespace() {
            break;
        }
        i += c.len_utf8();
    }
    i
}

fn skip_string(s: &str, start: usize) -> Result<usize, String> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return Err("expected a string".into());
    }
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => {
                if bytes.get(i + 1) == Some(&b'u') {
                    i = i.saturating_add(6);
                } else {
                    i = i.saturating_add(2);
                }
            }
            b'"' => return Ok(i + 1),
            _ => i += 1,
        }
    }
    Err("unterminated string".into())
}

fn read_string(s: &str, start: usize) -> Result<(String, usize), String> {
    let end = skip_string(s, start)?;
    let parsed: Value = serde_json::from_str(&s[start..end]).map_err(|e| e.to_string())?;
    match parsed {
        Value::String(text) => Ok((text, end)),
        _ => Err("expected a string".into()),
    }
}

fn skip_value(s: &str, start: usize) -> Result<usize, String> {
    let i = skip_ws(s, start);
    let rest = &s[i..];
    if rest.starts_with("null") {
        return Ok(i + 4);
    }
    if rest.starts_with("true") {
        return Ok(i + 4);
    }
    if rest.starts_with("false") {
        return Ok(i + 5);
    }
    if rest.starts_with('"') {
        return skip_string(s, i);
    }
    if rest.starts_with('[') || rest.starts_with('{') {
        let close = if rest.starts_with('[') { b']' } else { b'}' };
        let mut i = i + 1;
        loop {
            i = skip_ws(s, i);
            let b = s.as_bytes().get(i).copied();
            if b == Some(close) {
                return Ok(i + 1);
            }
            if b == Some(b',') {
                i += 1;
                continue;
            }
            if close == b'}' {
                let (_, after) = read_string(s, i)?;
                i = skip_ws(s, after);
                if s.as_bytes().get(i) != Some(&b':') {
                    return Err("expected ':'".into());
                }
                i += 1;
            }
            i = skip_value(s, i)?;
        }
    }
    let bytes = s.as_bytes();
    let mut i = i;
    let begin = i;
    if bytes.get(i) == Some(&b'-') {
        i += 1;
    }
    while matches!(
        bytes.get(i),
        Some(b'0'..=b'9' | b'.' | b'e' | b'E' | b'+' | b'-')
    ) {
        i += 1;
    }
    if i == begin {
        return Err("expected a value".into());
    }
    Ok(i)
}

/// Keys of the object whose `{` sits at `obj_open`, in file order.
fn object_keys(s: &str, obj_open: usize) -> Result<Vec<String>, String> {
    Ok(object_entries(s, obj_open)?
        .into_iter()
        .map(|e| e.key)
        .collect())
}

struct Entry {
    key: String,
    key_start: usize,
    value_start: usize,
    value_end: usize,
}

fn object_close(s: &str, obj_open: usize) -> Result<usize, String> {
    let end = skip_value(s, obj_open)?;
    if end == 0 {
        return Err("empty object span".into());
    }
    Ok(end - 1)
}

fn object_entries(s: &str, obj_open: usize) -> Result<Vec<Entry>, String> {
    let i = skip_ws(s, obj_open);
    if s.as_bytes().get(i) != Some(&b'{') {
        return Err("expected an object".into());
    }
    let mut i = i + 1;
    let mut out = Vec::new();
    loop {
        i = skip_ws(s, i);
        match s.as_bytes().get(i) {
            Some(b'}') => return Ok(out),
            Some(b',') => {
                i += 1;
                continue;
            }
            Some(b'"') => {
                let key_start = i;
                let (key, after) = read_string(s, i)?;
                i = skip_ws(s, after);
                if s.as_bytes().get(i) != Some(&b':') {
                    return Err(format!("expected ':' after {key}"));
                }
                i += 1;
                let value_start = skip_ws(s, i);
                let value_end = skip_value(s, value_start)?;
                out.push(Entry {
                    key,
                    key_start,
                    value_start,
                    value_end,
                });
                i = value_end;
            }
            _ => return Err("expected a key or '}'".into()),
        }
    }
}

/// Byte offset of the `{` of the object at `path`, or `None` if a key
/// along the path is missing. An intermediate value that is not an
/// object is an error — writing there would replace a scalar with a map.
fn find_object_at(s: &str, path: &[&str]) -> Result<Option<usize>, String> {
    let mut i = skip_ws(s, 0);
    if s.as_bytes().get(i) != Some(&b'{') {
        return Err("root is not an object".into());
    }
    for key in path {
        let entries = object_entries(s, i)?;
        // serde_json, and therefore load_config, keeps the last duplicate.
        // Edit that same effective object rather than an earlier dead copy.
        let Some(found) = entries.into_iter().rev().find(|e| e.key == *key) else {
            return Ok(None);
        };
        let at = skip_ws(s, found.value_start);
        if s.as_bytes().get(at) != Some(&b'{') {
            return Err(format!("{key} is not an object"));
        }
        i = at;
    }
    Ok(Some(i))
}

fn line_indent(s: &str, at: usize) -> String {
    let start = s[..at].rfind('\n').map(|n| n + 1).unwrap_or(0);
    s[start..at].chars().take_while(|c| c.is_whitespace()).collect()
}

fn infer_indent(s: &str, obj_open: usize, entries: &[Entry]) -> String {
    if let Some(first) = entries.first() {
        return line_indent(s, first.key_start);
    }
    format!("{}  ", line_indent(s, obj_open))
}

/// Replace or insert `path` so every other byte of `text` is left alone.
///
/// serde_json in this workspace has no preserve_order, so a parse-and-dump
/// would sort every object and hoist `_comment` above the section it
/// documents. This edits the raw text instead.
fn set_json_path(text: &str, path: &[&str], value: &Value) -> Result<String, String> {
    if path.is_empty() {
        return Err("empty path".into());
    }
    let mut text = if text.trim().is_empty() {
        "{\n}\n".to_string()
    } else {
        text.to_string()
    };
    let rendered = serde_json::to_string(value).map_err(|e| e.to_string())?;
    // Create any missing parent objects, outermost first, so each insert
    // sees a path that is only one key short.
    for depth in 0..path.len() - 1 {
        if find_object_at(&text, &path[..=depth])?.is_some() {
            continue;
        }
        let parent = find_object_at(&text, &path[..depth])?
            .ok_or_else(|| format!("missing {}", path[..depth].join(".")))?;
        text = insert_member(&text, parent, path[depth], "{}")?;
    }
    let parent = find_object_at(&text, &path[..path.len() - 1])?
        .ok_or_else(|| "parent object missing after insert".to_string())?;
    let key = path[path.len() - 1];
    let entries = object_entries(&text, parent)?;
    // Match serde_json's last-duplicate-wins behavior.
    if let Some(found) = entries.iter().rev().find(|e| e.key == key) {
        let mut out = String::with_capacity(text.len() + rendered.len());
        out.push_str(&text[..found.value_start]);
        out.push_str(&rendered);
        out.push_str(&text[found.value_end..]);
        return Ok(out);
    }
    insert_member(&text, parent, key, &rendered)
}

/// Remove the effective (last) occurrence of a setting without reserializing
/// its object or touching sibling bytes.
fn remove_json_path(text: &str, path: &[&str]) -> Result<String, String> {
    if path.is_empty() || text.trim().is_empty() {
        return Ok(text.to_string());
    }
    let Some(parent) = find_object_at(text, &path[..path.len() - 1])? else {
        return Ok(text.to_string());
    };
    let entries = object_entries(text, parent)?;
    let key = path[path.len() - 1];
    let Some(index) = entries.iter().rposition(|entry| entry.key == key) else {
        return Ok(text.to_string());
    };
    let found = &entries[index];
    let (start, end) = if entries.len() == 1 {
        (found.key_start, found.value_end)
    } else if index + 1 < entries.len() {
        (found.key_start, entries[index + 1].key_start)
    } else {
        (entries[index - 1].value_end, found.value_end)
    };
    let mut out = String::with_capacity(text.len());
    out.push_str(&text[..start]);
    out.push_str(&text[end..]);
    serde_json::from_str::<Value>(&out)
        .map_err(|e| format!("refusing to remove {key}: {e}"))?;
    Ok(out)
}

fn insert_member(text: &str, obj_open: usize, key: &str, value: &str) -> Result<String, String> {
    let entries = object_entries(text, obj_open)?;
    let close = object_close(text, obj_open)?;
    let piece = format!("\"{key}\": {value}");
    if entries.is_empty() {
        let indent = infer_indent(text, obj_open, &entries);
        let parent = line_indent(text, obj_open);
        let inner = format!("\n{indent}{piece}\n{parent}");
        let mut out = String::new();
        out.push_str(&text[..obj_open + 1]);
        out.push_str(&inner);
        out.push_str(&text[close..]);
        return Ok(out);
    }
    let last = entries.last().unwrap();
    let one_line = !text[obj_open..=close].contains('\n');
    let sep = if one_line {
        ",".to_string()
    } else {
        format!(",\n{}", infer_indent(text, obj_open, &entries))
    };
    let mut out = String::new();
    out.push_str(&text[..last.value_end]);
    out.push_str(&sep);
    out.push_str(&piece);
    out.push_str(&text[last.value_end..]);
    Ok(out)
}

/// Write `contents` via a temp file in the same directory, then rename.
///
/// The temp is `0600` on creation, not chmodded afterwards. A file that
/// does not re-parse is refused and the temp is removed, so an interrupted
/// or refused save cannot leave a truncated config that every widget then
/// fails to parse.
fn atomic_write(path: &Path, contents: &str, expected: Option<&str>) -> Result<(), String> {
    serde_json::from_str::<Value>(contents)
        .map_err(|e| format!("refusing to write: {e}"))?;
    // Keep an intentional config symlink intact. Rename over its resolved
    // target; renaming over the link itself would silently replace it.
    let destination = match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.file_type().is_symlink() => std::fs::canonicalize(path)
            .map_err(|e| format!("{}: {e}", path.display()))?,
        _ => path.to_path_buf(),
    };
    let path = destination.as_path();
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .map_err(|e| format!("{}: {e}", parent.display()))?;
        }
    }
    let name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config.json");
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let mut created = None;
    for attempt in 0..32 {
        let tmp = path.with_file_name(format!(
            ".{name}.tmp-{}-{nonce}-{attempt}",
            std::process::id()
        ));
        let mut opts = OpenOptions::new();
        opts.write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW);
        let mut file = match opts.open(&tmp) {
            Ok(file) => file,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(format!("{}: {e}", tmp.display())),
        };
        file.write_all(contents.as_bytes())
            .and_then(|_| file.sync_all())
            .map_err(|e| {
                let _ = std::fs::remove_file(&tmp);
                format!("{}: {e}", tmp.display())
            })?;
        created = Some(tmp);
        break;
    }
    let tmp = created.ok_or_else(|| {
        format!(
            "{}: could not reserve a private temporary file",
            path.display()
        )
    })?;
    let mode = std::fs::metadata(&tmp)
        .map_err(|e| format!("{}: {e}", tmp.display()))?
        .permissions()
        .mode()
        & 0o777;
    if mode != 0o600 {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!(
            "{} was created with mode {mode:04o}, not 0600",
            tmp.display()
        ));
    }
    let read_back =
        std::fs::read_to_string(&tmp).map_err(|e| format!("{}: {e}", tmp.display()))?;
    if let Err(e) = serde_json::from_str::<Value>(&read_back) {
        let _ = std::fs::remove_file(&tmp);
        return Err(format!("wrote a file that does not parse: {e}"));
    }
    if let Some(expected) = expected {
        let current = match std::fs::read_to_string(path) {
            Ok(text) => text,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(format!("{}: {e}", path.display()));
            }
        };
        if current != expected {
            let _ = std::fs::remove_file(&tmp);
            return Err("config changed again while saving · retry the edit".into());
        }
    }
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    if let Some(parent) = path.parent() {
        if let Ok(dir) = std::fs::File::open(parent) {
            let _ = dir.sync_all();
        }
    }
    Ok(())
}

fn parse_edit(
    raw: &str,
    expected: &Value,
    rule: Option<&Value>,
) -> Result<Value, String> {
    let trimmed = raw.trim();
    if expected.is_string() {
        let value = if trimmed.starts_with('"') {
            serde_json::from_str::<Value>(trimmed)
                .map_err(|_| "expected a JSON string or unquoted text".to_string())?
        } else {
            Value::String(raw.to_string())
        };
        validate_value(&value, expected, rule)?;
        return Ok(value);
    }
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        validate_value(&v, expected, rule)?;
        return Ok(v);
    }
    if expected.is_boolean() {
        let value = match trimmed.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Value::Bool(true),
            "false" | "no" | "off" | "0" => Value::Bool(false),
            _ => return Err("expected bool".into()),
        };
        validate_value(&value, expected, rule)?;
        return Ok(value);
    }
    Err(format!(
        "expected {}, could not parse that as JSON",
        kind_name(expected)
    ))
}

fn compact(v: &Value) -> String {
    serde_json::to_string(v).unwrap_or_else(|_| "?".into())
}

/// What a value reads as on a row, masked when it is one of the secrets.
///
/// **Only a value read out of the reader's own file is ever masked.** A
/// declared default ships in the widget's `settings.json`, in a public repo,
/// so hiding it says nothing and costs something: an unset token would draw
/// as `••••••••` in the default column and read as though a value were
/// already there. Callers pass `false` for a default deliberately.
///
/// There is no way to unmask. A key used to do it, and nothing on this screen
/// needed one - the value is in the file for anyone who has to read it, and a
/// settings screen that can put a live credential on a shared terminal is a
/// settings screen with a footgun on it.
fn summary(v: &Value, secret: bool) -> String {
    if secret {
        return match v {
            Value::String(s) if s.is_empty() => "(empty)".into(),
            Value::String(_) => "••••••••".into(),
            _ => "••••••••".into(),
        };
    }
    match v {
        Value::Array(a) => format!("array · {}", a.len()),
        Value::Object(o) => format!("object · {}", o.len()),
        Value::String(s) if s.chars().count() > 24 => format!("string · {}", s.chars().count()),
        other => compact(other),
    }
}

/// The new section wins. A legacy section is consulted only when its
/// owning widget explicitly declares it and the new section is absent.
fn live_section<'a>(
    live: &Value,
    canonical: &'a str,
    legacy: Option<&'a str>,
) -> &'a str {
    if live.get(canonical).is_some() {
        return canonical;
    }
    if let Some(old) = legacy {
        if live.get(old).is_some() {
            return old;
        }
    }
    canonical
}

fn current_of<'a>(
    live: &'a Value,
    field: &Field,
    legacy: Option<&str>,
) -> Option<&'a Value> {
    let mut at = live.get(live_section(live, &field.section, legacy))?;
    for step in field.steps() {
        at = at.get(step)?;
    }
    Some(at)
}

struct Palette {
    dim: String,
    dim_lit: String,
    txt: String,
    lbl: String,
    accent: String,
    ok: String,
    warn: String,
    bad: String,
}

fn palette() -> Palette {
    Palette {
        // dim 3.81 on bg(38,56,76); dim_lit 4.74. Measured, not eyeballed.
        dim: crate::rgb(127, 147, 172),
        dim_lit: crate::rgb(140, 170, 195),
        txt: crate::rgb(225, 235, 245),
        lbl: crate::rgb(130, 165, 200),
        accent: crate::rgb(150, 210, 255),
        ok: crate::rgb(90, 240, 160),
        warn: crate::rgb(255, 200, 90),
        bad: crate::rgb(255, 100, 110),
    }
}

#[derive(Clone)]
enum Mode {
    List,
    Edit {
        index: usize,
        buffer: String,
        cursor: usize,
        error: Option<String>,
    },
    /// A search over every zone the timezone database knows, for a field
    /// whose schema asks for `"picker": "timezone"`.
    Pick {
        index: usize,
        query: String,
        sel: usize,
        scroll: usize,
        /// Whether the whole table is on show, or only the entries this
        /// reader has set something on. Starts on the reader's own, which
        /// is the shorter list and the one they came to look at.
        show_all: bool,
        /// Whether the rows have focus rather than the box.
        ///
        /// A list you type into cannot also let a letter be a verb: the box
        /// starts empty, which is exactly when somebody types the first
        /// character of a new entry, and `d` meaning delete there costs you
        /// every entry beginning with a `d`. So the verb lives on the rows,
        /// reached with tab, and the box keeps every printable key.
        on_list: bool,
    },
}

struct App {
    widget: &'static str,
    section: &'static str,
    legacy_section: Option<&'static str>,
    schema: &'static str,
    catalogues: &'static [(&'static str, Catalogue)],
    constraints: serde_json::Map<String, Value>,
    fields: Vec<Field>,
    live: Value,
    raw: String,
    path: PathBuf,
    exists: bool,
    skipped: Vec<String>,
    selected: usize,
    scroll: usize,
    chase: bool,
    mode: Mode,
    status: Option<String>,
    /// The screens left behind while standing inside a declared object: the
    /// fields that were on show, and which of them was selected. Swapping
    /// `fields` rather than adding a mode means the list screen, the editor,
    /// the writers and every check on them work one level down unchanged.
    /// Whether anything reached the file. Leaving with this set restarts the
    /// widget, because a running one holds the config it started with.
    wrote: bool,
    /// The screen to restore, and the mode it was in. A model's prices are
    /// opened out of the picker, so going back means going back to the
    /// picker - with the search that found it still typed.
    stack: Vec<(Vec<Field>, usize, Option<Mode>)>,
}

fn load(spec: SettingsSpec) -> App {
    let schema_value: Value =
        serde_json::from_str(spec.schema).expect("the baked-in settings schema is valid JSON");
    let constraints = schema_value
        .get("_schema")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let section = serde_json::to_string(spec.section).expect("a section name is JSON");
    let wrapped = format!("{{{section}:{}}}", spec.schema);
    let fields = fields_from_example(&wrapped).expect("the baked-in settings schema is valid JSON");
    let mut skipped = Vec::new();
    let mut found = None;
    for path in crate::config_paths() {
        match std::fs::read_to_string(&path) {
            Ok(text) => match serde_json::from_str::<Value>(&text) {
                Ok(v) => {
                    found = Some((path, text, v));
                    break;
                }
                Err(e) => skipped.push(format!("{} does not parse ({e})", path.display())),
            },
            Err(_) => continue,
        }
    }
    let (path, raw, live, exists) = match found {
        Some((p, t, v)) => (p, t, v, true),
        None => (crate::default_config_path(), String::new(), serde_json::json!({}), false),
    };
    App {
        widget: spec.widget,
        section: spec.section,
        legacy_section: spec.legacy_section,
        schema: spec.schema,
        catalogues: spec.catalogues,
        wrote: false,
        constraints,
        fields,
        live,
        raw,
        path,
        exists,
        skipped,
        selected: 0,
        scroll: 0,
        chase: true,
        mode: Mode::List,
        status: None,
        stack: Vec::new(),
    }
}

fn edit_seed(field: &Field, current: Option<&Value>) -> String {
    let src = current.unwrap_or(&field.default);
    match src {
        Value::String(s) => s.clone(),
        other => compact(other),
    }
}

/// Re-read immediately before a mutation so another pane's unrelated
/// change is never overwritten by this screen's older snapshot.
fn fresh_config(app: &App) -> Result<(String, Value), String> {
    let resolved = crate::resolved_config_path().unwrap_or_else(crate::default_config_path);
    if resolved != app.path {
        return Err(format!(
            "config source changed to {} · reload before writing",
            resolved.display()
        ));
    }
    let raw = if app.path.exists() {
        std::fs::read_to_string(&app.path)
            .map_err(|e| format!("{}: {e}", app.path.display()))?
    } else {
        String::new()
    };
    let live = if raw.trim().is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str::<Value>(&raw)
            .map_err(|e| format!("config changed and no longer parses: {e}"))?
    };
    Ok((raw, live))
}

fn write_field(app: &mut App, index: usize, value: Value) -> Result<(), String> {
    let (key, steps, schema_path, schema_section) = {
        let field = app.fields.get(index).ok_or("no such field")?;
        validate_value(
            &value,
            &field.default,
            constraint_for(app, field),
        )?;
        (
            field.key.clone(),
            field.steps().iter().map(|s| s.to_string()).collect::<Vec<_>>(),
            field.path(),
            field.section.clone(),
        )
    };
    let (fresh_raw, fresh_live) = fresh_config(app)?;
    let section = live_section(
        &fresh_live,
        &schema_section,
        app.legacy_section,
    )
    .to_string();
    let mut path = vec![section.as_str()];
    path.extend(steps.iter().map(String::as_str));
    let next = set_json_path(&fresh_raw, &path, &value)?;
    serde_json::from_str::<Value>(&next).map_err(|e| format!("refusing to write: {e}"))?;
    atomic_write(&app.path, &next, Some(&fresh_raw))?;
    app.wrote = true;
    app.raw = next;
    app.live = serde_json::from_str(&app.raw).map_err(|e| e.to_string())?;
    app.exists = true;
    app.status = Some(if section != schema_section {
        format!(
            "wrote {section}.{key} · this file still uses `{section}` · reloading {}",
            app.widget
        )
    } else {
        format!(
            "wrote {schema_path} · {} reloads when you leave",
            app.widget
        )
    });
    Ok(())
}

/// Take an override back out, wherever it sits.
///
/// This walked `[section, key]` while the writer walked `steps()`, so one
/// screen down it looked for `agent_usage.input` rather than
/// `agent_usage.rates.gpt-5.6-sol.input`, found nothing, and said the field
/// already used its default - which was false, and left the override in
/// place. Un-setting one number is most of what `d` is for on a screen of
/// numbers, so it has to follow the same path the write took.
fn reset_field(app: &mut App, index: usize) -> Result<(), String> {
    let field = app.fields.get(index).ok_or("no such field")?;
    let steps: Vec<String> = field.steps().iter().map(|s| s.to_string()).collect();
    let schema_path = field.path();
    let schema_section = field.section.clone();
    let shown_default = summary(&field.default, false);
    let (fresh_raw, fresh_live) = fresh_config(app)?;
    let section = live_section(
        &fresh_live,
        &schema_section,
        app.legacy_section,
    )
    .to_string();
    // section, then every key the field sits under, then the field itself.
    let mut full: Vec<&str> = vec![section.as_str()];
    full.extend(steps.iter().map(String::as_str));
    let held = |doc: &Value| -> bool {
        let mut here = doc;
        for step in &full {
            match here.get(step) {
                Some(next) => here = next,
                None => return false,
            }
        }
        true
    };
    if !held(&fresh_live) {
        app.status = Some(format!("{schema_path} already uses its default"));
        return Ok(());
    }
    let mut next = fresh_raw.clone();
    loop {
        next = remove_json_path(&next, &full)?;
        let parsed: Value = serde_json::from_str(&next)
            .map_err(|e| format!("refusing to reset {schema_path}: {e}"))?;
        if !held(&parsed) {
            break;
        }
    }
    atomic_write(&app.path, &next, Some(&fresh_raw))?;
    app.wrote = true;
    app.raw = next;
    app.live = serde_json::from_str(&app.raw).map_err(|e| e.to_string())?;
    app.status = Some(format!(
        "removed {schema_path} · default {shown_default} · {} reloads when you leave",
        app.widget
    ));
    Ok(())
}

fn move_sel(app: &mut App, delta: isize) {
    if app.fields.is_empty() {
        return;
    }
    let next = (app.selected as isize + delta)
        .clamp(0, app.fields.len() as isize - 1) as usize;
    app.selected = next;
    app.chase = true;
}

fn wrap_help(text: &str, width: usize, limit: usize) -> Vec<String> {
    if width == 0 {
        return Vec::new();
    }
    let mut lines = Vec::new();
    let mut rest: Vec<char> = text.trim().chars().collect();
    while !rest.is_empty() && lines.len() < limit {
        if rest.len() <= width {
            lines.push(rest.iter().collect());
            break;
        }
        let cut = rest[..width.min(rest.len())]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|c| *c > width / 3)
            .unwrap_or(width);
        lines.push(rest[..cut].iter().collect());
        rest = rest[cut..].iter().skip_while(|c| **c == ' ').copied().collect();
    }
    lines
}

fn insert_chars(buffer: &mut String, cursor: &mut usize, ch: &str) {
    let byte = buffer
        .chars()
        .take(*cursor)
        .map(|c| c.len_utf8())
        .sum::<usize>();
    buffer.insert_str(byte, ch);
    *cursor += ch.chars().count();
}

fn delete_before(buffer: &mut String, cursor: &mut usize) {
    if *cursor == 0 {
        return;
    }
    let chars: Vec<char> = buffer.chars().collect();
    let at = *cursor - 1;
    *buffer = chars
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != at)
        .map(|(_, c)| *c)
        .collect();
    *cursor = at;
}

/// Why a field is currently doing nothing, if it is.
///
/// A setting that is read only when another one says so is the quietest kind
/// of trap: it accepts what you type, saves it, shows it back, and changes
/// nothing. `"inactive_when": {"key": "detect_agents", "is": true}` lets the
/// field say so, and the screen says it before you edit rather than after.
fn inactive_because(app: &App, key: &str) -> Option<String> {
    let rule = app.constraints.get(key)?.as_object()?.get("inactive_when")?;
    let rule = rule.as_object()?;
    let other = rule.get("key")?.as_str()?;
    let when = rule.get("is")?;
    let field = app.fields.iter().find(|f| f.key == other)?;
    let live = current_of(&app.live, field, app.legacy_section).unwrap_or(&field.default);
    (live == when).then(|| format!("not in use while {other} is {}", compact(when)))
}

/// Where a picker's candidates come from.
///
/// Two shapes, and they want opposite defaults. A closed set - the six agents
/// this machine knows about, the seven days of a week - is a checklist: show
/// all of it, ticked or not, because seeing what you did *not* choose is half
/// the information. A catalogue of five hundred and ninety-seven zones is not
/// a checklist, and opening on Africa/Abidjan tells nobody anything, so it
/// opens on what is already configured and searches from there.
enum PickKind {
    /// Every zone the timezone database knows. Written as `[label, zone]`
    /// pairs, because a zone name is not what anyone wants on screen.
    Timezone,
    /// A finite set the schema names. Written as bare strings.
    Choices(Vec<String>),
    /// A list the reader writes themselves: hostnames, org names, repo
    /// paths. There is nothing to offer, so the box composes a new entry
    /// rather than searching for an existing one, and the rows below are
    /// what is already in the list.
    Free,
    /// The keys of a table the widget owns in code, written as the keys of
    /// an object rather than as members of a list - each one holds the
    /// numbers set against it, so it has to be a key with something under
    /// it. Ticking a name adds an empty object: membership, no values, so
    /// every number keeps tracking the widget's own defaults until the
    /// reader changes one.
    Catalogue(Catalogue),
}

impl PickKind {
    /// Whether an empty query means "show me only what I have chosen".
    ///
    /// True only for the timezone database, where the unticked half is five
    /// hundred and ninety-seven rows and nobody is browsing it. A short
    /// checklist that hid its unticked half until you searched would be
    /// hiding the choice it exists to offer, and a rate card would make you
    /// search for a model you have not chosen yet.
    fn opens_on_chosen(&self) -> bool {
        matches!(self, PickKind::Timezone)
    }
}

/// The picker a field asks for, if it asks for one.
///
/// `"picker": "timezone"` for the built-in catalogue; `"picker": {"choices":
/// [..]}` for a set the widget names itself. Declared by the widget rather
/// than known to core, which is the same division the rest of this screen
/// keeps: the widget owns its settings, core owns the editing of them.
/// The `_schema` rules for a field, wherever it sits.
///
/// A nested field's rules live under its parent's `fields`, so the lookup has
/// to follow the same path the value does. Everything that validates or draws
/// goes through here rather than reaching into `constraints` by key, which
/// only ever worked for the top level.
fn constraint_for<'a>(app: &'a App, field: &Field) -> Option<&'a Value> {
    let Some((first, rest)) = field.parents.split_first() else {
        return app.constraints.get(&field.key);
    };
    let mut here = app.constraints.get(first)?;
    // Every step but the first is a key inside the enclosing `fields`. A
    // catalogue-backed map has no `fields` to descend into - its keys are
    // whatever the reader added - so the rules of the map itself stand for
    // each entry, and `values` below reads the same either way.
    for step in rest {
        let Some(next) = here.get("fields").and_then(Value::as_object).and_then(|f| f.get(step))
        else {
            return Some(here);
        };
        here = next;
    }
    here.get("fields")
        .and_then(Value::as_object)
        .and_then(|f| f.get(&field.key))
        .or(Some(here))
}

/// The fields a widget declared inside an object, if it declared any.
///
/// This is the whole of the opt-in: a section key whose schema carries
/// `"fields": { .. }` gets a screen of its own, and one that does not keeps
/// the JSON box it has always had. Nothing is required to declare anything.
fn nested_fields(app: &App, index: usize) -> Option<Vec<Field>> {
    let field = app.fields.get(index)?;
    if !field.parents.is_empty() {
        return None; // one screen down, and no further
    }
    let declared = app
        .constraints
        .get(&field.key)?
        .as_object()?
        .get("fields")?
        .as_object()?;
    // The order the widget wrote them in, read back out of the schema text.
    // A parsed map sorts its keys, which put agent-usage's six agents in
    // alphabetical order on screen while the widget draws them in its own -
    // and its own is the meaningful one, being the order of the tabs. The
    // file already solves this for the top-level fields; the same helper
    // solves it one level down.
    let wrapped = format!("{{{}:{}}}", "\"_\"", app.schema);
    let order: Vec<String> = find_object_at(&wrapped, &["_", "_schema", &field.key, "fields"])
        .ok()
        .flatten()
        .and_then(|open| object_keys(&wrapped, open).ok())
        .unwrap_or_default();
    let ordered: Vec<(&String, &Value)> = if order.is_empty() {
        declared.iter().collect()
    } else {
        order
            .iter()
            .filter_map(|k| declared.get_key_value(k))
            .collect()
    };
    Some(
        ordered
            .into_iter()
            .map(|(name, rule)| {
                let rule = rule.as_object();
                let default = rule
                    .and_then(|r| r.get("default"))
                    .cloned()
                    // No declared default means the widget has none to offer,
                    // and the type still has to come from somewhere for the
                    // editor to validate against.
                    .unwrap_or(Value::Number(0.into()));
                let help = rule
                    .and_then(|r| r.get("help"))
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_string();
                Field {
                    section: field.section.clone(),
                    key: name.clone(),
                    parents: vec![field.key.clone()],
                    help,
                    default,
                }
            })
            .collect(),
    )
}

/// The rows for a map whose candidate keys live in the widget's code.
///
/// `agent-usage`'s rate card is sixty-eight models with five prices each.
/// Copying that into `settings.json` would be two records of one fact, and
/// the one in the schema would go stale the first time a vendor moved a
/// price - which has happened here already. So the widget hands core the
/// table itself and the screen reads it.
///
/// Only models the reader has actually added get rows, because sixty-eight
/// models times five kinds is a screen nobody can read. The picker adds and
/// removes them; these rows are for setting the numbers.
///
/// The published price is each row's **default**, never a value written into
/// the file. Copying it in would pin that reader to today's number and stop
/// them getting the correction when it moves.
fn model_fields(app: &App, field: &Field, model: &str) -> Vec<Field> {
    let Some(table) = catalogue_for(app, &field.key) else {
        return Vec::new();
    };
    let entry = table.iter().find(|(m, _, _)| *m == model);
    let listed = entry.map(|(_, _, r)| *r);
    let group = entry.map(|(_, g, _)| *g);
    // A model the reader named themselves has no published price, so its
    // kinds are the ones the card prices everywhere else - offered at no
    // default rather than not offered at all.
    let kinds: Vec<(&str, Option<f64>)> = match listed {
        Some(rates) => rates.iter().map(|(k, v)| (*k, Some(*v))).collect(),
        None => catalogue_kinds(table).into_iter().map(|k| (k, None)).collect(),
    };
    kinds
        .into_iter()
        .map(|(kind, price)| Field {
            section: field.section.clone(),
            key: kind.to_string(),
            parents: vec![field.key.clone(), model.to_string()],
            help: match (price, group) {
                (Some(_), Some(who)) => {
                    format!("{who}'s published list price for {model}.")
                }
                (Some(_), None) => format!("The published list price for {model}."),
                (None, _) => {
                    format!("No published price for {model}: this is yours to set.")
                }
            },
            default: price
                .and_then(serde_json::Number::from_f64)
                .map(Value::Number)
                .unwrap_or(Value::Null),
        })
        .collect()
}

/// The entries a reader has actually set a number on.
///
/// There is no separate membership any more. A model is "yours" when it holds
/// a price, which is the only state worth distinguishing and the only one
/// that can go stale - an empty object used to mean "picked", and it meant a
/// screen could say "configured" over a column of shipped defaults.
fn catalogue_chosen(app: &App, field: &Field) -> Vec<String> {
    current_of(&app.live, field, app.legacy_section)
        .and_then(Value::as_object)
        .map(|o| {
            o.iter()
                .filter(|(_, v)| v.as_object().is_some_and(|m| !m.is_empty()))
                .map(|(k, _)| k.clone())
                .collect()
        })
        .unwrap_or_default()
}

/// Every kind the card prices anywhere, in the order it first names them.
fn catalogue_kinds(table: Catalogue) -> Vec<&'static str> {
    let mut seen: Vec<&'static str> = Vec::new();
    for (_, _, rates) in table {
        for (kind, _) in *rates {
            if !seen.contains(kind) {
                seen.push(kind);
            }
        }
    }
    seen
}


/// Who publishes a catalogue entry, for the column beside its name.
fn catalogue_group(kind: &PickKind, key: &str) -> Option<String> {
    let PickKind::Catalogue(table) = kind else {
        return None;
    };
    table
        .iter()
        .find(|(k, _, _)| *k == key)
        .map(|(_, group, _)| group.to_string())
}

fn catalogue_for(app: &App, key: &str) -> Option<Catalogue> {
    app.catalogues.iter().find(|(f, _)| *f == key).map(|(_, t)| *t)
}


fn picker_kind(app: &App, key: &str) -> Option<PickKind> {
    // A field the widget handed a table for needs no `picker` declaration:
    // offering the table *is* the picker, and asking settings.json to say so
    // as well would be a second place for the two to disagree.
    if let Some(table) = catalogue_for(app, key) {
        return Some(PickKind::Catalogue(table));
    }
    let rule = match app.constraints.get(key).and_then(Value::as_object) {
        Some(rule) => rule,
        // No rules at all still leaves a list of strings worth editing as a
        // list - latency's hosts declare no `items` and are plainly strings.
        None => return free_list_kind(app, key),
    };
    let Some(rule) = rule.get("picker") else {
        return free_list_kind(app, key);
    };
    if rule.as_str() == Some("timezone") {
        return Some(PickKind::Timezone);
    }
    let choices = rule.as_object()?.get("choices")?.as_array()?;
    Some(PickKind::Choices(
        choices
            .iter()
            .filter_map(Value::as_str)
            .map(str::to_string)
            .collect(),
    ))
}

/// Whether a field is a list of strings the reader fills in themselves.
///
/// Declared `items: "string"` says so outright. Failing that, a default that
/// is a non-empty array of strings says it just as well - `latency.hosts`
/// ships three and declares nothing. An array of numbers is left alone: an
/// RGB triple is one value in three parts, not a list anybody adds to.
fn free_list_kind(app: &App, key: &str) -> Option<PickKind> {
    let field = app.fields.iter().find(|f| f.key == key)?;
    if !field.default.is_array() {
        return None;
    }
    let declared = app
        .constraints
        .get(key)
        .and_then(Value::as_object)
        .and_then(|r| r.get("items"))
        .and_then(Value::as_str);
    let stringy = match declared {
        Some("string") => true,
        Some(_) => false,
        None => {
            let items = field.default.as_array()?;
            !items.is_empty() && items.iter().all(Value::is_string)
        }
    };
    stringy.then_some(PickKind::Free)
}

/// Cities the timezone database does not name, and the zone that carries
/// them.
///
/// IANA names one representative city per zone, so `America/Los_Angeles` is
/// the whole US Pacific coast and there is no `America/San_Francisco` to
/// find - searching for one of these returns nothing at all, which reads as
/// "not supported" rather than "filed under another name".
///
/// Deliberately a short list of cities people actually put on a wall, not an
/// attempt at a gazetteer. A city missing from here still works: its zone is
/// searchable under whatever IANA calls it. This only saves the reader from
/// having to know that Los Angeles stands in for San Jose.
///
/// The label comes from the alias, so adding "san francisco" writes
/// `["San Francisco", "America/Los_Angeles"]` - which is exactly the pair
/// `config.example.json` has shipped by hand since the beginning.
const CITY_ALIASES: &[(&str, &str)] = &[
    // United States and Canada
    ("San Francisco", "America/Los_Angeles"),
    ("San Jose", "America/Los_Angeles"),
    ("San Diego", "America/Los_Angeles"),
    ("Seattle", "America/Los_Angeles"),
    ("Portland", "America/Los_Angeles"),
    ("Las Vegas", "America/Los_Angeles"),
    ("Austin", "America/Chicago"),
    ("Dallas", "America/Chicago"),
    ("Houston", "America/Chicago"),
    ("Boston", "America/New_York"),
    ("Washington", "America/New_York"),
    ("Miami", "America/New_York"),
    ("Atlanta", "America/New_York"),
    ("Philadelphia", "America/New_York"),
    ("Salt Lake City", "America/Denver"),
    ("Boulder", "America/Denver"),
    ("Montreal", "America/Toronto"),
    ("Ottawa", "America/Toronto"),
    // Europe
    ("Manchester", "Europe/London"),
    ("Edinburgh", "Europe/London"),
    ("Cambridge", "Europe/London"),
    ("Munich", "Europe/Berlin"),
    ("Frankfurt", "Europe/Berlin"),
    ("Hamburg", "Europe/Berlin"),
    ("Barcelona", "Europe/Madrid"),
    ("Milan", "Europe/Rome"),
    ("Geneva", "Europe/Zurich"),
    // Asia and Oceania
    ("Shanghai", "Asia/Shanghai"),
    ("Beijing", "Asia/Shanghai"),
    ("Shenzhen", "Asia/Shanghai"),
    ("Guangzhou", "Asia/Shanghai"),
    ("Kyoto", "Asia/Tokyo"),
    ("Osaka", "Asia/Tokyo"),
    ("Bangalore", "Asia/Kolkata"),
    ("Bengaluru", "Asia/Kolkata"),
    ("Mumbai", "Asia/Kolkata"),
    ("Delhi", "Asia/Kolkata"),
    ("Hyderabad", "Asia/Kolkata"),
    ("Pune", "Asia/Kolkata"),
    ("Tel Aviv", "Asia/Jerusalem"),
    ("Abu Dhabi", "Asia/Dubai"),
    ("Canberra", "Australia/Sydney"),
    ("Wellington", "Pacific/Auckland"),
    // South America and Africa
    ("Rio de Janeiro", "America/Sao_Paulo"),
    ("Cape Town", "Africa/Johannesburg"),
];

/// The alias that answers this query for a zone, if one does.
fn alias_hit(zone: &str, query: &str) -> Option<&'static str> {
    if query.is_empty() {
        return None;
    }
    let needle = query.to_lowercase();
    CITY_ALIASES
        .iter()
        .filter(|(_, z)| *z == zone)
        .find(|(city, _)| {
            let c = city.to_lowercase();
            c.starts_with(&needle) || c.split_whitespace().any(|w| w.starts_with(&needle))
        })
        .map(|(city, _)| *city)
}

/// The label a zone gets when it is picked: its last path segment, with the
/// underscores the database uses turned back into spaces.
///
/// `Asia/Hong_Kong` becomes `Hong Kong`, which is what every entry in
/// `config.example.json` already says by hand. The pair stays editable
/// afterwards for the ones wanting something shorter.
fn zone_label(zone: &str) -> String {
    zone.rsplit('/').next().unwrap_or(zone).replace('_', " ")
}

/// How well a zone answers the query, lower being better, `None` for no
/// match at all.
///
/// Matched against the whole path with the separators flattened, so `hong`,
/// `asia`, `asia/hong` and `hong kong` all reach `Asia/Hong_Kong`. Someone
/// searching for a city should not have to know it is filed under Asia, and
/// someone browsing a region should not have to know the city.
///
/// Ranked rather than merely filtered because alphabetical order buries the
/// obvious answer: searching `hong` matched `Asia/Chongqing` first, on the
/// "hong" inside "Chongqing", with `Asia/Hong_Kong` below it. A city whose
/// own name starts with what was typed comes first, then one that starts
/// with it anywhere in the path, then a bare substring.
fn zone_rank(zone: &str, query: &str) -> Option<u8> {
    if query.is_empty() {
        return Some(0);
    }
    let needle = query.to_lowercase().replace(['_', '/'], " ");
    let flat = zone.to_lowercase().replace(['_', '/'], " ");
    let leaf = zone_label(zone).to_lowercase();
    if leaf.starts_with(&needle) {
        return Some(0);
    }
    if flat.split_whitespace().any(|w| w.starts_with(&needle)) {
        return Some(1);
    }
    if flat.contains(&needle) {
        return Some(2);
    }
    // A city the database does not name, filed under the zone that carries
    // it. Ranked with the leaf-prefix matches: someone typing "san fran"
    // means it at least as precisely as someone typing "los ang".
    if alias_hit(zone, query).is_some() {
        return Some(0);
    }
    None
}

/// The zones currently configured, in the order the file lists them.
///
/// Anything in the array that is not a `[label, zone]` pair is passed over
/// rather than repaired: this screen adds and removes whole entries, and a
/// half-understood one is the user's to fix in the file.
fn picked_pairs(app: &App, index: usize) -> Vec<(String, String)> {
    let Some(field) = app.fields.get(index) else {
        return Vec::new();
    };
    let held = current_of(&app.live, field, app.legacy_section).or(Some(&field.default));
    // A catalogue-backed field is an object, and its keys are the choices.
    // The value under each is the numbers, which the picker never touches.
    if matches!(held, Some(Value::Object(_))) && catalogue_for(app, &field.key).is_some() {
        return catalogue_chosen(app, field)
            .into_iter()
            .map(|k| (k.clone(), k))
            .collect();
    }
    held.and_then(Value::as_array)
        .map(|rows| {
            rows.iter()
                .filter_map(|row| match row {
                    // `[label, value]`, as clocks writes its cities.
                    Value::Array(pair) => {
                        let value = pair.get(1).and_then(Value::as_str)?;
                        let label = pair
                            .first()
                            .and_then(Value::as_str)
                            .unwrap_or(value)
                            .to_string();
                        Some((value.to_string(), label))
                    }
                    // A bare string, as every other list writes its entries.
                    Value::String(one) => Some((one.clone(), one.clone())),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Just the zones, for the membership tests that do not care what it is
/// called.
fn picked_zones(app: &App, index: usize) -> Vec<String> {
    picked_pairs(app, index)
        .into_iter()
        .map(|(zone, _)| zone)
        .collect()
}

/// What the file calls a configured zone, or a derived label if it somehow
/// has none. The name the reader chose beats the one the database implies:
/// an entry written as "San Francisco" should not read back "Los Angeles".
fn picked_label(app: &App, index: usize, zone: &str) -> Option<String> {
    picked_pairs(app, index)
        .into_iter()
        .find(|(z, _)| z == zone)
        .map(|(_, label)| label)
}

/// What the screen lists, which depends on whether anything has been typed.
///
/// **Nothing typed:** the cities already configured, and only those. That is
/// the list somebody removing one wants - four rows, not four rows hidden in
/// five hundred and ninety-seven - and it means removing never involves
/// searching for what you already have.
///
/// **Anything typed:** every zone the query matches, ranked, with a flag for
/// the ones already in. Deliberately *not* hoisting the configured ones to
/// the top here: a city that moves when you tick it is a city you then have
/// to find again, and it puts the same name in two places depending on a
/// state you are in the middle of changing. The tick moves, the row does not.
fn zone_choices(app: &App, index: usize, query: &str, show_all: bool) -> Vec<(String, bool)> {
    let Some(field) = app.fields.get(index) else {
        return Vec::new();
    };
    let Some(kind) = picker_kind(app, &field.key) else {
        return Vec::new();
    };
    let picked = picked_zones(app, index);
    if kind.opens_on_chosen() && query.is_empty() {
        return picked.into_iter().map(|z| (z, true)).collect();
    }
    // A catalogue shows the reader's own entries until they ask for the rest
    // or type something. Sixty-eight rows is a table to search, not a list to
    // read, and the three you have set are what you came back for.
    if let PickKind::Catalogue(_) = &kind {
        if !show_all && query.is_empty() {
            return picked.into_iter().map(|z| (z, true)).collect();
        }
    }
    // A free list has no candidates: what is on screen is what is in it,
    // and typing composes an entry rather than filtering these.
    if matches!(kind, PickKind::Free) {
        return picked.into_iter().map(|z| (z, true)).collect();
    }
    let mut out: Vec<(u8, String)> = match &kind {
        PickKind::Timezone => chrono_tz::TZ_VARIANTS
            .iter()
            .map(|tz| tz.name().to_string())
            .filter_map(|name| zone_rank(&name, query).map(|r| (r, name)))
            .collect(),
        PickKind::Choices(all) => all
            .iter()
            .filter_map(|one| zone_rank(one, query).map(|r| (r, one.clone())))
            .collect(),
        // Never reached: a free list returns above, before any ranking.
        PickKind::Free => Vec::new(),
        PickKind::Catalogue(table) => table
            .iter()
            .filter_map(|(name, group, _)| {
                // The publisher is searchable too, because it is on screen
                // and a column you can read is a column you will type at.
                // A name match always outranks one: "gpt" wants the models
                // called that, above everything OpenAI happens to publish.
                zone_rank(name, query)
                    .or_else(|| zone_rank(group, query).map(|r| r.saturating_add(4)))
                    .map(|r| (r, name.to_string()))
            })
            .collect(),
    };
    // Stable, so within a rank the source's own order holds - alphabetical
    // for the database, and the widget's own order for a named set, which is
    // the order it draws them in.
    out.sort_by_key(|(rank, _)| *rank);
    out.into_iter()
        .map(|(_, name)| {
            let on = picked.iter().any(|p| *p == name);
            (name, on)
        })
        .collect()
}

/// Add the zone if it is absent, drop it if it is there, and write.
///
/// Writing per toggle rather than collecting an edit and committing it: the
/// write is atomic and the file is small, and it means what is on screen and
/// what is on disk never disagree - which is the whole reason this screen
/// exists rather than an instruction to go and edit JSON.
fn toggle_zone(app: &mut App, index: usize, zone: &str, label: Option<String>) {
    let Some(field) = app.fields.get(index) else {
        return;
    };
    let key = field.key.clone();
    // A catalogue is an object: the name is a key, and what it holds are the
    // numbers set against it. Ticking writes an empty object rather than any
    // values - membership alone - so every kind keeps reading the widget's
    // published default until somebody edits that one number. Unticking
    // takes the whole entry, numbers included, which is what "remove this
    // model" has to mean.
    if catalogue_for(app, &key).is_some() {
        let mut held: serde_json::Map<String, Value> =
            match current_of(&app.live, field, app.legacy_section) {
                Some(Value::Object(map)) => map.clone(),
                _ => serde_json::Map::new(),
            };
        let removed = held.remove(zone).is_some();
        if !removed {
            held.insert(zone.to_string(), Value::Object(serde_json::Map::new()));
        }
        match write_field(app, index, Value::Object(held)) {
            Ok(()) => {
                app.status = Some(format!(
                    "{} {}",
                    if removed { "removed" } else { "added" },
                    zone
                ))
            }
            Err(e) => app.status = Some(e),
        }
        return;
    }
    let mut rows: Vec<Value> = current_of(&app.live, field, app.legacy_section)
        .or(Some(&field.default))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let at = rows.iter().position(|row| match row {
        Value::Array(pair) => pair.get(1).and_then(Value::as_str) == Some(zone),
        Value::String(one) => one == zone,
        _ => false,
    });
    let removed = at.is_some();
    match at {
        Some(i) => {
            rows.remove(i);
        }
        None => rows.push(match picker_kind(app, &key) {
            // A pair, so the pane has something to print that is not a
            // database identifier.
            Some(PickKind::Timezone) => Value::Array(vec![
                Value::String(label.unwrap_or_else(|| zone_label(zone))),
                Value::String(zone.to_string()),
            ]),
            // Everything else is the value itself. Wrapping a name the user
            // already recognises in a pair would only make the file harder
            // to read by hand, which is still how most of it gets edited.
            _ => Value::String(zone.to_string()),
        }),
    }
    match write_field(app, index, Value::Array(rows)) {
        Ok(()) => {
            app.status = Some(format!(
                "{} {}",
                if removed { "removed" } else { "added" },
                zone
            ))
        }
        Err(e) => app.status = Some(e),
    }
}

/// Put one entry into a free list, unless it is already there.
///
/// Appended rather than sorted: the widget decides what order means, and for
/// a list of hosts the order somebody typed them in is the one they expect
/// to see back.
fn add_free_entry(app: &mut App, index: usize, entry: &str) {
    let Some(field) = app.fields.get(index) else {
        return;
    };
    let mut rows: Vec<Value> = current_of(&app.live, field, app.legacy_section)
        .or(Some(&field.default))
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    if rows.iter().any(|v| v.as_str() == Some(entry)) {
        app.status = Some(format!("{entry} is already in the list"));
        return;
    }
    rows.push(Value::String(entry.to_string()));
    match write_field(app, index, Value::Array(rows)) {
        Ok(()) => app.status = Some(format!("added {entry}")),
        Err(e) => app.status = Some(e),
    }
}

/// Take one entry out, leaving the rest in the order they were in.
fn remove_free_entry(app: &mut App, index: usize, entry: &str) {
    let Some(field) = app.fields.get(index) else {
        return;
    };
    let Some(rows) = current_of(&app.live, field, app.legacy_section)
        .or(Some(&field.default))
        .and_then(Value::as_array)
        .cloned()
    else {
        return;
    };
    let kept: Vec<Value> = rows
        .into_iter()
        .filter(|v| v.as_str() != Some(entry))
        .collect();
    match write_field(app, index, Value::Array(kept)) {
        Ok(()) => app.status = Some(format!("removed {entry}")),
        Err(e) => app.status = Some(e),
    }
}

fn handle_pick_key(app: &mut App, key: &str) -> bool {
    let Mode::Pick { index, query, sel, scroll, show_all, on_list } = &mut app.mode else {
        return false;
    };
    let (index, mut q, mut s_, mut sc, mut all, mut rows) =
        (*index, query.clone(), *sel, *scroll, *show_all, *on_list);
    let catalogue = app
        .fields
        .get(index)
        .is_some_and(|f| catalogue_for(app, &f.key).is_some());
    let free = app
        .fields
        .get(index)
        .is_some_and(|f| matches!(picker_kind(app, &f.key), Some(PickKind::Free)));
    let total = zone_choices(app, index, &q, all).len();
    match key {
        "esc" => {
            app.mode = Mode::List;
            return false;
        }
        "q" | "Q" if q.is_empty() => {
            app.mode = Mode::List;
            return false;
        }
        "up" => s_ = s_.saturating_sub(1),
        "down" => s_ = (s_ + 1).min(total.saturating_sub(1)),
        "pgup" => s_ = s_.saturating_sub(10),
        "pgdn" => s_ = (s_ + 10).min(total.saturating_sub(1)),
        "home" => s_ = 0,
        "end" => s_ = total.saturating_sub(1),
        // The wheel moves the view and never the selection, as everywhere.
        "ctrl-y" | "wheel-up" => sc = sc.saturating_sub(1),
        "ctrl-e" | "wheel-down" => sc = sc.saturating_add(1),
        "backspace" => {
            q.pop();
            s_ = 0;
        }
        // Back to your own list in one key rather than one per character.
        "ctrl-u" => {
            q.clear();
            s_ = 0;
            sc = 0;
        }
        // A catalogue entry is a thing with numbers under it, so opening it
        // is what enter should do. A list of names has nothing to open, and
        // enter still ticks.
        // Tab crosses between the box and the rows. It is the only key on
        // this screen that is not a character somebody might want typed.
        "tab" if free => {
            rows = !rows;
        }
        // What is typed is the entry. There is nothing to search, so enter
        // on an empty box would have nothing to mean.
        "enter" if free => {
            let typed = q.trim().to_string();
            if !typed.is_empty() {
                add_free_entry(app, index, &typed);
                q.clear();
                s_ = 0;
                sc = 0;
            }
        }
        // Only with the rows in focus. In the box it is a letter, always -
        // including as the first one, which is where guarding on an empty
        // box got it wrong.
        "d" | "D" if free && rows => {
            if let Some((entry, _)) = zone_choices(app, index, &q, all).get(s_).cloned() {
                remove_free_entry(app, index, &entry);
                s_ = s_.saturating_sub(1);
            }
        }
        "enter" if catalogue => {
            if let Some((model, _)) = zone_choices(app, index, &q, all).get(s_).cloned() {
                let Some(parent) = app.fields.get(index).cloned() else {
                    return false;
                };
                let rows = model_fields(app, &parent, &model);
                if !rows.is_empty() {
                    app.stack
                        .push((app.fields.clone(), index, Some(app.mode.clone())));
                    app.fields = rows;
                    app.selected = 0;
                    app.scroll = 0;
                    app.status = None;
                    app.mode = Mode::List;
                    return false;
                }
            }
        }
        // Clearing an entry from the picker, where the entry is visible as
        // one row. Doing it from inside would mean deleting five fields and
        // then finding your own way out.
        "d" | "D" if catalogue && q.is_empty() => {
            if let Some((model, on)) = zone_choices(app, index, &q, all).get(s_).cloned() {
                if on {
                    clear_catalogue_entry(app, index, &model);
                } else {
                    app.status = Some(format!("{model} has nothing set on it"));
                }
            }
        }
        "tab" if catalogue => {
            all = !all;
            s_ = 0;
            sc = 0;
        }
        "enter" => {
            if let Some((zone, _)) = zone_choices(app, index, &q, all).get(s_).cloned() {
                let label = alias_hit(&zone, &q).map(str::to_string);
                toggle_zone(app, index, &zone, label);
            }
        }
        other if other.chars().count() == 1 => {
            let ch = other.chars().next().unwrap();
            if !ch.is_control() {
                // Typing is for the box, so it takes focus back. Somebody
                // who starts typing has said where they want to be.
                rows = false;
                q.push(ch);
                s_ = 0;
            }
        }
        _ => {}
    }
    let total = zone_choices(app, index, &q, all).len();
    if let Mode::Pick { query, sel, scroll, show_all, on_list, .. } = &mut app.mode {
        *query = q;
        *sel = s_.min(total.saturating_sub(1));
        *scroll = sc;
        *show_all = all;
        // Nothing to stand on is nothing to focus.
        *on_list = rows && total > 0;
    }
    false
}

/// Take a catalogue entry back out of the file, numbers and all.
///
/// Removing the object rather than each number is the point: a model with
/// nothing set is a model priced from the card, and leaving an empty husk
/// behind would be a row claiming to be configured with nothing in it.
fn clear_catalogue_entry(app: &mut App, index: usize, model: &str) {
    let Some(field) = app.fields.get(index) else {
        return;
    };
    let mut held: serde_json::Map<String, Value> =
        match current_of(&app.live, field, app.legacy_section) {
            Some(Value::Object(map)) => map.clone(),
            _ => return,
        };
    if held.remove(model).is_none() {
        return;
    }
    match write_field(app, index, Value::Object(held)) {
        Ok(()) => app.status = Some(format!("{model} back to list prices")),
        Err(e) => app.status = Some(e),
    }
}

fn handle_list_key(app: &mut App, key: &str) -> bool {
    match key {
        // Coming out of a declared object is not leaving the screen. Only
        // the outermost list quits.
        "esc" if !app.stack.is_empty() => {
            if let Some((fields, sel, mode)) = app.stack.pop() {
                app.fields = fields;
                app.selected = sel;
                app.scroll = 0;
                app.status = None;
                if let Some(mode) = mode {
                    app.mode = mode;
                }
            }
        }
        "q" | "Q" | "esc" | "," => return true,
        "up" | "k" | "K" => move_sel(app, -1),
        "down" | "j" | "J" => move_sel(app, 1),
        "ctrl-y" | "wheel-up" => {
            app.scroll = app.scroll.saturating_sub(1);
            app.chase = false;
        }
        "ctrl-e" | "wheel-down" => {
            app.scroll = app.scroll.saturating_add(1);
            app.chase = false;
        }
        "pgup" => move_sel(app, -10),
        "pgdn" => move_sel(app, 10),
        "home" => {
            app.selected = 0;
            app.chase = true;
        }
        "end" => {
            app.selected = app.fields.len().saturating_sub(1);
            app.chase = true;
        }
        "r" | "R" => {
            let keep = app.selected;
            *app = load(SettingsSpec {
                widget: app.widget,
                section: app.section,
                legacy_section: app.legacy_section,
                schema: app.schema,
                catalogues: app.catalogues,
            });
            app.selected = keep.min(app.fields.len().saturating_sub(1));
            app.status = Some("reloaded from disk".into());
        }
        "d" | "D" => {
            if let Err(e) = reset_field(app, app.selected) {
                app.status = Some(e);
            }
        }
        "enter" => {
            if let Some(field) = app.fields.get(app.selected) {
                if field.default.is_boolean() {
                    // Three states, not two. A boolean that is not in the
                    // file is not the same as one written to the value the
                    // default happens to have today: the default can move in
                    // a release, and the unwritten one moves with it. Toggling
                    // between true and false could only ever reach two of the
                    // three, and left the third behind a different key.
                    //
                    // unset -> true -> false -> unset, so the cycle always
                    // comes back and nothing is a one-way door.
                    let current = current_of(&app.live, field, app.legacy_section)
                        .and_then(|v| v.as_bool());
                    let outcome = match current {
                        None => write_field(app, app.selected, Value::Bool(true)),
                        Some(true) => write_field(app, app.selected, Value::Bool(false)),
                        Some(false) => reset_field(app, app.selected),
                    };
                    if let Err(e) = outcome {
                        app.status = Some(e);
                    }
                } else if let Some(inner) =
                    nested_fields(app, app.selected).filter(|rows| !rows.is_empty())
                {
                    app.stack.push((app.fields.clone(), app.selected, None));
                    app.fields = inner;
                    app.selected = 0;
                    app.scroll = 0;
                } else if picker_kind(app, &field.key).is_some() {
                    app.mode = Mode::Pick {
                        index: app.selected,
                        query: String::new(),
                        sel: 0,
                        scroll: 0,
                        // Your own entries first, because that is the short
                        // list and the one you came back for - unless there
                        // are none, where opening on it would be a screen
                        // saying you have nothing and offering nothing.
                        show_all: picked_zones(app, app.selected).is_empty(),
                        // The box first: a list you type into is a list you
                        // came to type into.
                        on_list: false,
                    };
                } else {
                    let seed = edit_seed(field, current_of(&app.live, field, app.legacy_section));
                    let cursor = seed.chars().count();
                    app.mode = Mode::Edit {
                        index: app.selected,
                        buffer: seed,
                        cursor,
                        error: None,
                    };
                }
            }
        }
        _ => {}
    }
    false
}

fn handle_edit_key(app: &mut App, key: &str) -> bool {
    let Mode::Edit {
        index,
        buffer,
        cursor,
        error,
    } = &mut app.mode
    else {
        return false;
    };
    match key {
        "esc" => {
            app.mode = Mode::List;
            app.status = Some("edit cancelled".into());
        }
        "enter" => {
            let field = match app.fields.get(*index) {
                Some(f) => f,
                None => {
                    app.mode = Mode::List;
                    return false;
                }
            };
            match parse_edit(
                buffer,
                &field.default,
                app.constraints.get(&field.key),
            ) {
                Ok(value) => {
                    let i = *index;
                    match write_field(app, i, value) {
                        Ok(()) => app.mode = Mode::List,
                        Err(e) => {
                            if let Mode::Edit { error, .. } = &mut app.mode {
                                *error = Some(e);
                            }
                        }
                    }
                }
                Err(e) => *error = Some(e),
            }
        }
        "backspace" => delete_before(buffer, cursor),
        // The box opens with the current value in it, so replacing a number
        // meant holding backspace over it. Same key the search field uses,
        // and readline's, so it is one habit rather than two.
        "ctrl-u" => {
            buffer.clear();
            *cursor = 0;
            *error = None;
        }
        "left" => *cursor = cursor.saturating_sub(1),
        "right" => *cursor = (*cursor + 1).min(buffer.chars().count()),
        "home" => *cursor = 0,
        "end" => *cursor = buffer.chars().count(),
        other if other.chars().count() == 1 => {
            let ch = other.chars().next().unwrap();
            if !ch.is_control() {
                insert_chars(buffer, cursor, other);
                *error = None;
            }
        }
        _ => {}
    }
    false
}

fn draw_list(app: &mut App, w: usize, h: usize, p: &Palette) -> Vec<String> {
    // Pinned: it names the widget whose settings these are, which is the one
    // thing that must not scroll away from somebody halfway down a long list.
    let head = crate::title(&format!("{} settings", app.widget), w, &p.accent);
    let mut body: Vec<String> = Vec::new();
    let path_note = if app.exists {
        format!(" {}", app.path.display())
    } else {
        format!(" {} · file does not exist yet · will be created 0600", app.path.display())
    };
    body.push(crate::seg(&[(p.dim.as_str(), path_note)], w.saturating_sub(1)));
    let unset = app
        .fields
        .iter()
        .filter(|f| current_of(&app.live, f, app.legacy_section).is_none())
        .count();
    body.push(crate::seg(
        &[(
            p.dim.as_str(),
            format!(
                " {} keys · {} unset",
                app.fields.len(),
                unset
            ),
        )],
        w.saturating_sub(1),
    ));
    for note in app.skipped.iter().take(2) {
        body.push(crate::seg(&[(p.warn.as_str(), format!(" {note}"))], w.saturating_sub(1)));
    }
    if let Some(old) = app.legacy_section {
        let canonical = app
            .fields
            .first()
            .map(|field| field.section.as_str())
            .unwrap_or("");
        if app.live.get(canonical).is_none() && app.live.get(old).is_some() {
            let note = format!(
                " this file still has `{old}` · {} reads it until the section is renamed",
                app.widget
            );
            body.push(crate::seg(&[(p.warn.as_str(), note)], w.saturating_sub(1)));
        }
    }
    if let Some(status) = &app.status {
        let colour = if status.starts_with("wrote ")
            || status.starts_with("removed ")
            || status.starts_with("copied ")
        {
            p.ok.as_str()
        } else if status.starts_with("not copied") || status.contains("refusing") {
            p.bad.as_str()
        } else {
            p.dim.as_str()
        };
        body.push(crate::seg(&[(colour, format!(" {status}"))], w.saturating_sub(1)));
    }
    body.push(String::new());

    let boolean = app
        .fields
        .get(app.selected)
        .is_some_and(|f| f.default.is_boolean());
    let hints = list_hints(p, boolean);
    let foot: Vec<String> = crate::pack_hints(&hints, w.saturating_sub(2), "  ")
        .into_iter()
        .map(|l| format!(" {l}"))
        .collect();

    if !app.fields.is_empty() {
        app.selected = app.selected.min(app.fields.len() - 1);
    }
    // Where the field rows begin, so the cursor can be followed through the
    // whole screen rather than through a list that is only part of it.
    let first_field_row = body.len();
    let key_w = app
        .fields
        .iter()
        .map(|f| f.label().chars().count())
        .max()
        .unwrap_or(8)
        .max(8);
    let after_key = w.saturating_sub(key_w + 4);
    let show_value = after_key >= 14;
    let show_default = after_key >= 32;
    let show_set = after_key >= 42;
    let value_w = if show_default {
        ((after_key - if show_set { 8 } else { 0 }) / 2).max(8)
    } else if show_value {
        after_key.saturating_sub(if show_set { 8 } else { 0 }).max(8)
    } else {
        0
    };

    // Every field, at its natural height. What does not fit is below the
    // fold and reachable, rather than absent and indistinguishable from a
    // widget with fewer settings than it has.
    for (i, field) in app.fields.iter().enumerate() {
        let here = i == app.selected;
        let tint = if here {
            crate::bg(38, 56, 76)
        } else {
            String::new()
        };
        let c_of = |colour: &str| {
            let colour = if tint.is_empty() {
                colour
            } else if colour == p.dim {
                p.dim_lit.as_str()
            } else {
                colour
            };
            format!("{}{}", tint, colour)
        };
        let current = current_of(&app.live, field, app.legacy_section);
        let set = current.is_some();
        let mark = if here { "▸ " } else { "  " };
        let mut parts: Vec<(String, String)> = vec![
            (
                c_of(if here { &p.accent } else { &p.dim }),
                mark.to_string(),
            ),
            (
                c_of(if here { &p.txt } else { &p.lbl }),
                crate::pad(&field.label(), key_w + 1),
            ),
        ];
        if show_value {
            let shown = match current {
                Some(v) => summary(v, field.secret()),
                None => "—".into(),
            };
            parts.push((
                c_of(if set { &p.txt } else { &p.dim }),
                format!(" {}", crate::pad(&shown, value_w)),
            ));
        }
        if show_default {
            let shown = summary(&field.default, false);
            parts.push((
                c_of(&p.dim),
                format!(" {}", crate::pad(&shown, value_w)),
            ));
        }
        if show_set {
            parts.push((
                c_of(if set { &p.ok } else { &p.dim }),
                if set { " set".into() } else { " unset".into() },
            ));
        }
        if here {
            parts.push((tint.clone(), " ".repeat(w)));
        }
        let refs: Vec<(&str, String)> = parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        body.push(crate::seg(&refs, w.saturating_sub(1)));
    }

    if let Some(field) = app.fields.get(app.selected) {
        body.push(String::new());
        body.push(crate::seg(
            &[(p.lbl.as_str(), format!(" ── {} ── ", field.path().to_uppercase()))],
            w.saturating_sub(1),
        ));
        let current = current_of(&app.live, field, app.legacy_section);
        let facts = format!(
            "current {} · default {} · {}",
            current
                .map(|value| summary(value, field.secret()))
                .unwrap_or_else(|| "—".into()),
            summary(&field.default, false),
            if current.is_some() {
                "set in file"
            } else {
                "using default"
            }
        );
        for line in wrap_help(&facts, w.saturating_sub(4), 2) {
            body.push(crate::seg(
                &[(p.txt.as_str(), format!("  {line}"))],
                w.saturating_sub(1),
            ));
        }
        let constraints = constraint_summary(constraint_for(app, field));
        if !constraints.is_empty() {
            for line in wrap_help(&constraints, w.saturating_sub(4), 2) {
                body.push(crate::seg(
                    &[(p.lbl.as_str(), format!("  {line}"))],
                    w.saturating_sub(1),
                ));
            }
        }
        let help = if field.help.is_empty() {
            field.kind().to_string()
        } else {
            field.help.clone()
        };
        for line in wrap_help(&help, w.saturating_sub(4), usize::MAX) {
            body.push(crate::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1)));
        }
        // Its own paragraph, and in the warn colour: it is the one line here
        // that is about what happens next rather than about the field under
        // the cursor, and run on from the help in the same grey it read as
        // more of the field's description. "This widget" rather than the
        // name, because the name is already in the title above it.
        body.push(String::new());
        body.push(crate::seg(
            &[(
                p.warn.as_str(),
                "  This widget reloads when you leave this screen.".to_string(),
            )],
            w.saturating_sub(1),
        ));
        let file_section = live_section(&app.live, &field.section, app.legacy_section);
        if file_section != field.section {
            body.push(crate::seg(
                &[(
                    p.warn.as_str(),
                    format!(
                        "  this file still uses `{file_section}` — {} reads that",
                        field.widget()
                    ),
                )],
                w.saturating_sub(1),
            ));
        }
    }

    let room = h.saturating_sub(1 + foot.len()).max(1);
    // The wheel moves the view and leaves the cursor where it is; a key that
    // moved the cursor drags the view back to it. Same rule as every widget.
    if app.chase {
        app.scroll = crate::follow(app.scroll, first_field_row + app.selected, room);
        app.chase = false;
    }
    app.scroll = app.scroll.min(body.len().saturating_sub(room));

    let mut out = vec![head];
    out.extend(body.iter().skip(app.scroll).take(room).cloned());
    while out.len() + foot.len() < h {
        out.push(String::new());
    }
    out.extend(foot);
    out.truncate(h);
    out
}

fn draw_edit(app: &App, w: usize, h: usize, p: &Palette) -> Vec<String> {
    let Mode::Edit {
        index,
        buffer,
        cursor,
        error,
    } = &app.mode
    else {
        return vec![crate::title(
            &format!("{} settings", app.widget),
            w,
            &p.accent,
        )];
    };
    let field = &app.fields[*index];
    let mut body = vec![crate::title(
        &format!("{} settings", app.widget),
        w,
        &p.accent,
    )];
    body.push(crate::seg(
        &[(p.dim.as_str(), format!(" {}", app.path.display()))],
        w.saturating_sub(1),
    ));
    body.push(String::new());
    body.push(crate::seg(
        &[(p.lbl.as_str(), format!(" ── {} ── ", field.path().to_uppercase()))],
        w.saturating_sub(1),
    ));
    for line in wrap_help(&field.help, w.saturating_sub(4), 3) {
        body.push(crate::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1)));
    }
    body.push(crate::seg(
        &[(
            p.dim.as_str(),
            {
                let unit = constraint_for(app, field)
                    .and_then(Value::as_object)
                    .and_then(|r| r.get("unit"))
                    .and_then(Value::as_str)
                    .map(|u| format!(" · {u}"))
                    .unwrap_or_default();
                format!(
                    "  {}{} · default {} · {} reloads on the way out",
                    field.kind(),
                    unit,
                    summary(&field.default, false),
                    field.widget()
                )
            },
        )],
        w.saturating_sub(1),
    ));
    if field.secret() {
        body.push(crate::seg(
            &[(p.warn.as_str(), "  a token · never shown once written".into())],
            w.saturating_sub(1),
        ));
    }
    body.push(String::new());
    // The same field the search uses. It was two rows before - the text on
    // one and a caret pointing up at it from the next - which reads as a
    // note about the value rather than as the value being typed, and cost a
    // row on a short pane for the privilege.
    body.push(input_row(
        p,
        w,
        buffer,
        *cursor,
        // The kind, and nothing about clearing: an empty buffer is a parse
        // error here, not an unset. `[d]` on the list is what removes a key,
        // and a placeholder promising otherwise would be a line on screen
        // that is not true.
        field.kind(),
        // The editor is the only thing on its screen; it always has focus.
        true,
    ));
    if let Some(err) = error {
        body.push(crate::seg(&[(p.bad.as_str(), format!("  {err}"))], w.saturating_sub(1)));
    }

    let hints: Vec<Vec<(&str, String)>> = vec![
        vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " write".into())],
        vec![(p.accent.as_str(), "ctrl-u".into()), (p.dim.as_str(), " clear".into())],
        vec![(p.dim.as_str(), "esc cancel".into())],
    ];
    let foot: Vec<String> = crate::pack_hints(&hints, w.saturating_sub(2), "  ")
        .into_iter()
        .map(|l| format!(" {l}"))
        .collect();
    while body.len() + foot.len() < h {
        body.push(String::new());
    }
    body.extend(foot);
    body.truncate(h);
    body
}

/// One text field, drawn the same way everywhere something is typed.
///
/// A bare caret on a bare line reads as output rather than as somewhere to
/// type, and the one thing an input has to make obvious is that typing does
/// something. So: a box with visible edges, and a caret that blinks.
///
/// The blink runs off the wall clock rather than a frame counter, so the
/// cadence is the same half-second whatever the redraw interval is - the
/// list redraws at 80ms and a counter would flicker.
///
/// `cursor` is a character offset, not a byte one, and the window follows it:
/// what is being corrected is what should be on screen, so a value longer
/// than the box scrolls to keep the caret in view rather than pinning either
/// end.
fn input_row(
    p: &Palette,
    w: usize,
    text: &str,
    cursor: usize,
    placeholder: &str,
    focused: bool,
) -> String {
    let box_w = w.saturating_sub(6).max(12);
    let room = box_w.saturating_sub(2);
    let chars: Vec<char> = text.chars().collect();
    let cursor = cursor.min(chars.len());
    // Enough of the tail to hold the caret, and no more.
    let start = if cursor >= room { cursor + 1 - room } else { 0 };
    let head: String = chars[start..cursor].iter().collect();
    let tail: String = chars[cursor..(start + room).min(chars.len())].iter().collect();

    let field = crate::bg(24, 36, 50);
    let ink = format!("{field}{}", p.txt);
    let ghost = format!("{field}{}", p.dim);
    let edge = format!("{field}{}", p.accent);
    // Blinking is the box saying it is listening; a box that is not being
    // typed into does not blink, and does not pretend to be where the keys
    // are going.
    let lit = focused && (crate::now() * 2.0) as u64 % 2 == 0;

    let hint = if text.is_empty() && !placeholder.is_empty() {
        // Only where it fits, and never over what has been typed.
        let room_left = box_w.saturating_sub(2);
        if placeholder.chars().count() <= room_left {
            format!(" {placeholder}")
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    crate::seg(
        &[
            (p.dim.as_str(), "  ".to_string()),
            (edge.as_str(), " ".to_string()),
            (ink.as_str(), head),
            (edge.as_str(), if lit { "▏".into() } else { " ".to_string() }),
            (ink.as_str(), tail),
            (ghost.as_str(), hint),
            (field.as_str(), " ".repeat(box_w)),
        ],
        w.saturating_sub(1),
    )
}

fn draw_pick(app: &App, w: usize, h: usize, p: &Palette) -> Vec<String> {
    let Mode::Pick { index, query, sel, scroll, show_all, on_list } = &app.mode else {
        return vec![crate::title(&format!("{} settings", app.widget), w, &p.accent)];
    };
    let field = &app.fields[*index];
    let choices = zone_choices(app, *index, query, *show_all);
    let kind = picker_kind(app, &field.key).unwrap_or(PickKind::Timezone);
    let total_choices = match &kind {
        PickKind::Timezone => chrono_tz::TZ_VARIANTS.len(),
        PickKind::Choices(all) => all.len(),
        PickKind::Catalogue(table) => table.len(),
        PickKind::Free => choices.len(),
    };
    // Counted from the configuration, not from the filtered list: counting
    // the visible ticks made a search that matched nothing report "0 of 597"
    // to someone with four cities configured, which reads as having none.
    let chosen = picked_zones(app, *index).len();

    let mut body = vec![crate::title(&format!("{} settings", app.widget), w, &p.accent)];
    body.push(crate::seg(
        &[(p.dim.as_str(), format!(" {}", app.path.display()))],
        w.saturating_sub(1),
    ));
    body.push(String::new());
    body.push(crate::seg(
        &[(p.lbl.as_str(), format!(" ── {} ── ", field.path().to_uppercase()))],
        w.saturating_sub(1),
    ));
    // Said here rather than only in the file's comment: someone arranging
    // this list is exactly the person who would otherwise assume the order
    // they choose is the order they get.
    let zones = matches!(kind, PickKind::Timezone);
    let heading = match (zones, query.is_empty()) {
        (true, true) => format!(
            "  {} configured · type to search {} zones and add more",
            chosen,
            chrono_tz::TZ_VARIANTS.len()
        ),
        (true, false) => format!(
            "  {} of {} zones match · {} configured in all",
            choices.len(),
            chrono_tz::TZ_VARIANTS.len(),
            chosen
        ),
        // A closed set says how much of itself is chosen, which is the
        // question a checklist answers.
        (false, _) => format!("  {} of {} chosen", chosen, total_choices),
    };
    // A catalogue is not a checklist: nothing is "chosen", things are set or
    // they are not, and the count that matters is how many you have changed
    // against how many there are to change.
    let heading = match &kind {
        PickKind::Free if chosen == 0 => {
            "  nothing here yet · type an entry and press ↵".to_string()
        }
        PickKind::Free if *on_list => format!(
            "  {} {} · [d] removes the one selected · tab back to the box",
            chosen,
            if chosen == 1 { "entry" } else { "entries" }
        ),
        PickKind::Free => format!(
            "  {} {} · type to add another · tab to the list to remove one",
            chosen,
            if chosen == 1 { "entry" } else { "entries" }
        ),
        PickKind::Catalogue(_) if !query.is_empty() => format!(
            "  {} of {} match · {} set in all",
            choices.len(),
            total_choices,
            chosen
        ),
        PickKind::Catalogue(_) if *show_all => {
format!(
                "  all {} · {} with custom rates · tab for those",
                total_choices, chosen
            )
        }
        PickKind::Catalogue(_) if chosen == 0 => {
            format!("  nothing set · tab or type to search all {total_choices}")
        }
        PickKind::Catalogue(_) => format!(
            "  {chosen} with custom rates · tab or type to search all {total_choices}"
        ),
        _ => heading,
    };
    body.push(crate::seg(&[(p.dim.as_str(), heading)], w.saturating_sub(1)));
    // How the widget will order what is picked here, said on the screen
    // rather than only in the file's comment: whoever is arranging a list is
    // exactly the person who would otherwise assume the order they choose is
    // the order they get. Which for one of these is true and the other not.
    // A lookup keyed by name has no order to explain, so it says nothing
    // rather than something true of a list and meaningless here.
    let ordering = match &kind {
        PickKind::Timezone => Some("  drawn west to east by each zone's offset, not in the order added"),
        PickKind::Choices(_) => Some("  shown in the order picked here"),
        PickKind::Free => Some("  in the order you add them"),
        PickKind::Catalogue(_) => None,
    };
    if let Some(ordering) = ordering {
        body.push(crate::seg(
            &[(p.dim.as_str(), ordering.to_string())],
            w.saturating_sub(1),
        ));
    }
    if let Some(why) = inactive_because(app, &field.key) {
        body.push(crate::seg(
            &[(p.warn.as_str(), format!("  {why}"))],
            w.saturating_sub(1),
        ));
    }
    body.push(String::new());
    // The box does different work per kind: it searches a catalogue, and it
    // composes an entry for a list that has no candidates to search.
    let hint = match &kind {
        PickKind::Timezone => "type a city or a zone",
        PickKind::Free => "type an entry, then ↵",
        _ => "type to filter",
    };
    // The cursor is what says the box is listening, so it goes away when
    // the rows are the thing being driven.
    body.push(input_row(p, w, query, query.chars().count(), hint, !*on_list));
    body.push(String::new());

    let foot_rows = 2;
    let room = h.saturating_sub(body.len() + foot_rows).max(1);
    let first = if *sel >= *scroll + room {
        sel + 1 - room
    } else if *sel < *scroll {
        *sel
    } else {
        (*scroll).min(choices.len().saturating_sub(room))
    };
    if choices.is_empty() {
        let why = match (zones, query.is_empty()) {
            (true, true) => "  no cities yet - type to search and add one".to_string(),
            // A catalogue is never empty - the card is always there - so an
            // empty screen here means the reader is looking at their own and
            // has none yet. Say which key gets them the rest.
            (false, true) if matches!(kind, PickKind::Catalogue(_)) => {
                "  nothing set yet - tab for the whole card".to_string()
            }
            (false, true) if matches!(kind, PickKind::Free) => {
                "  the list is empty - type an entry above".to_string()
            }
            (false, true) => "  nothing to choose from".to_string(),
            (_, false) => format!("  nothing matches /{query}"),
        };
        body.push(crate::seg(&[(p.dim.as_str(), why)], w.saturating_sub(1)));
    }
    for (i, (zone, on)) in choices.iter().enumerate().skip(first).take(room) {
        let here = i == *sel;
        let tint = if here { crate::bg(28, 44, 62) } else { String::new() };
        let mark = if *on { "✓" } else { " " };
        // Owned first, borrowed after: `seg` takes `&str`, so a colour
        // composed inline would not outlive the slice it is put in. The tint
        // has to lead each one or the foreground that follows resets the
        // background and the highlight stops halfway across.
        let lead = format!("{tint}{}", if here { &p.accent } else { &p.dim });
        let name = format!("{tint}{}", if *on { &p.txt } else { &p.dim });
        let note = format!("{tint}{}", p.dim);
        body.push(crate::seg(
            &[
                (
                    lead.as_str(),
                    format!(" {} {} ", if here { "▸" } else { " " }, mark),
                ),
                (name.as_str(), crate::pad(zone, 34)),
                (
                    note.as_str(),
                    match (alias_hit(zone, query), *on) {
                        // Why a search for a city returned a zone with
                        // another name. Without it the row looks like a
                        // mismatch rather than an answer.
                        (Some(city), _) => format!("{city} is here"),
                        // Only where it says something the first column
                        // does not. For a named set the label is the value,
                        // and printing it twice is furniture.
                        (None, true) if zones => picked_label(app, *index, zone)
                            .unwrap_or_else(|| zone_label(zone)),
                        // Who publishes it. A key does not always say - o3
                        // and codex-mini-latest are OpenAI's - and a flat
                        // list of sixty-eight is a list you scan by vendor.
                        (None, _) => catalogue_group(&kind, zone).unwrap_or_default(),
                    },
                ),
                (tint.as_str(), " ".repeat(w)),
            ],
            w.saturating_sub(1),
        ));
    }

    let hints: Vec<Vec<(&str, String)>> = if matches!(kind, PickKind::Free) {
        let mut h: Vec<Vec<(&str, String)>> = Vec::new();
        if *on_list {
            h.push(vec![(p.dim.as_str(), "[d]elete the entry".into())]);
            h.push(vec![
                (p.accent.as_str(), "tab".into()),
                (p.dim.as_str(), " back to typing".into()),
            ]);
        } else {
            if query.is_empty() {
                h.push(vec![(p.dim.as_str(), "type to add".into())]);
            } else {
                h.push(vec![
                    (p.accent.as_str(), "↵".into()),
                    (p.dim.as_str(), " add it".into()),
                ]);
                h.push(vec![
                    (p.accent.as_str(), "ctrl-u".into()),
                    (p.dim.as_str(), " clear".into()),
                ]);
            }
            if chosen > 0 {
                h.push(vec![
                    (p.accent.as_str(), "tab".into()),
                    (p.dim.as_str(), " the list".into()),
                ]);
            }
        }
        h.push(vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " pick".into())]);
        h.push(vec![(p.dim.as_str(), "esc done".into())]);
        h
    } else if matches!(kind, PickKind::Catalogue(_)) {
        // Enter opens rather than ticks here, so the footer has to say so -
        // this is the one picker where the row is a door, not a checkbox.
        let mut h = vec![vec![
            (p.accent.as_str(), "↵".into()),
            (p.dim.as_str(), " open".into()),
        ]];
        h.push(vec![
            (p.accent.as_str(), "tab".into()),
            (
                p.dim.as_str(),
                if *show_all {
                    " models with custom rates".into()
                } else {
                    " show all".into()
                },
            ),
        ]);
        // Only while the search is empty, because that is the only time the
        // key is a verb rather than a character going into the box.
        if chosen > 0 && query.is_empty() {
            h.push(vec![(p.dim.as_str(), "[d]efault".into())]);
        }
        if !query.is_empty() {
            h.push(vec![
                (p.accent.as_str(), "ctrl-u".into()),
                (p.dim.as_str(), " clear".into()),
            ]);
        }
        h.push(vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " pick".into())]);
        h.push(vec![(p.dim.as_str(), "esc done".into())]);
        h
    } else if !zones {
        // Everything is on screen, ticked or not, so one verb covers it.
        let mut h = vec![vec![
            (p.accent.as_str(), "↵".into()),
            (p.dim.as_str(), " add / remove".into()),
        ]];
        if !query.is_empty() {
            h.push(vec![
                (p.accent.as_str(), "ctrl-u".into()),
                (p.dim.as_str(), " clear".into()),
            ]);
        }
        h.push(vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " pick".into())]);
        h.push(vec![(p.dim.as_str(), "esc done".into())]);
        h
    } else if query.is_empty() {
        vec![
            vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " remove".into())],
            vec![(p.dim.as_str(), "type to add".into())],
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " pick".into())],
            vec![(p.dim.as_str(), "esc done".into())],
        ]
    } else {
        vec![
            vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " add / remove".into())],
            vec![
                (p.accent.as_str(), "ctrl-u".into()),
                (p.dim.as_str(), " clear".into()),
            ],
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " pick".into())],
            vec![(p.dim.as_str(), "esc done".into())],
        ]
    };
    let foot: Vec<String> = crate::pack_hints(&hints, w.saturating_sub(2), "  ")
        .into_iter()
        .map(|l| format!(" {l}"))
        .collect();
    while body.len() + foot.len() < h {
        body.push(String::new());
    }
    body.extend(foot);
    body.truncate(h);
    body
}

fn list_hints<'a>(p: &'a Palette, boolean: bool) -> Vec<Vec<(&'a str, String)>> {
    // `edit` is wrong for the row under the cursor when that row is a
    // boolean: enter does not open anything, it moves it on one place.
    let enter = if boolean { " true / false / default" } else { " edit" };
    vec![
        vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
        vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), enter.into())],
        vec![(p.dim.as_str(), "[r]eload".into())],
        vec![(p.dim.as_str(), "[d]efault".into())],
        vec![(p.dim.as_str(), "esc / [,] back".into())],
    ]
}

/// Take over the current terminal until the user returns to the widget.
///
/// The caller keeps ownership of the already-active keyboard so entering
/// settings never stacks a second raw-mode guard over the widget's.
pub fn run_settings(keyboard: &mut crate::Keyboard, spec: SettingsSpec) {
    let p = palette();
    let mut app = load(spec);

    loop {
        for key in keyboard.poll() {
            let quit = match app.mode {
                Mode::List => handle_list_key(&mut app, &key),
                Mode::Edit { .. } => handle_edit_key(&mut app, &key),
                Mode::Pick { .. } => handle_pick_key(&mut app, &key),
            };
            if quit {
                if app.wrote {
                    relaunch(keyboard);
                }
                return;
            }
        }
        let (w, h) = crate::size();
        let body = match app.mode {
            Mode::List => draw_list(&mut app, w, h, &p),
            Mode::Edit { .. } => draw_edit(&app, w, h, &p),
            Mode::Pick { .. } => draw_pick(&app, w, h, &p),
        };
        crate::draw(&body, w, h);
        std::thread::sleep(Duration::from_millis(80));
    }
}

/// Start this binary again, in place, carrying the arguments it was given.
///
/// A widget reads its config once and builds everything from it - poll
/// intervals, hosts, colours, which tabs exist - so a value written here
/// cannot reach a widget already running. The screen used to say so and
/// leave it to the reader. Doing it for them means one behaviour for every
/// widget rather than fourteen half-implementations, and it lands at the one
/// moment it is safe: on the way out, when the terminal is being handed back
/// anyway.
///
/// `exec` replaces this process, so nothing after it runs when it works.
/// When it does not - a binary deleted or replaced under a running pane -
/// the terminal is put back the way the widget expects and the caller
/// carries on with the config it started with, which is exactly where it
/// was before.
fn relaunch(keyboard: &mut crate::Keyboard) {
    use std::os::unix::process::CommandExt;

    let Ok(exe) = std::env::current_exe() else {
        return;
    };
    keyboard.restore();
    crate::restore_screen();
    let args: Vec<String> = std::env::args().skip(1).collect();
    let _ = std::process::Command::new(exe).args(args).exec();
    // Only reached when exec failed. Put the screen back so the widget is
    // not drawing into a terminal that has been handed away.
    crate::setup();
}

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../config.example.json");

    /// A two-model card shaped exactly like a widget's own, so the tests
    /// exercise the same path `agent-usage` takes rather than a stand-in.
    const CARD: Catalogue = &[
        ("model-a", "Acme", &[("input", 4.0), ("output", 20.0)]),
        ("model-b", "Others", &[("input", 1.0), ("output", 2.0), ("cache_read", 0.1)]),
    ];

    /// An App standing on one catalogue-backed field, holding `live`.
    fn catalogue_app(live: Value) -> App {
        App {
            widget: "w",
            section: "w",
            legacy_section: None,
            schema: "{}",
            catalogues: &[("rates", CARD)],
            constraints: serde_json::Map::new(),
            fields: vec![Field {
                section: "w".into(),
                key: "rates".into(),
                parents: Vec::new(),
                help: String::new(),
                default: Value::Object(serde_json::Map::new()),
            }],
            live,
            raw: String::new(),
            path: PathBuf::from("/nonexistent"),
            exists: false,
            skipped: Vec::new(),
            selected: 0,
            scroll: 0,
            chase: false,
            mode: Mode::List,
            status: None,
            wrote: false,
            stack: Vec::new(),
        }
    }

    /// The published number is the row's default and never the row's value.
    ///
    /// Writing list prices into somebody's config pins them to the day they
    /// opened the screen: the next correction from the vendor never reaches
    /// them, which is exactly the staleness this table has already had once.
    #[test]
    fn a_models_prices_are_offered_as_defaults_not_written_as_values() {
        let app = catalogue_app(serde_json::json!({"w": {"rates": {}}}));
        let parent = app.fields[0].clone();
        let rows = model_fields(&app, &parent, "model-a");
        assert_eq!(rows.len(), 2, "model-a prices two kinds");
        assert_eq!(rows[0].key, "input");
        assert_eq!(rows[0].parents, vec!["rates".to_string(), "model-a".to_string()]);
        assert_eq!(rows[0].default, serde_json::json!(4.0));
        // Opening a model writes nothing, so nothing is set.
        assert!(current_of(&app.live, &rows[0], None).is_none());
        // The row names its model: "input" alone appears once per model.
        assert_eq!(rows[0].label(), "model-a · input");
    }

    /// The publisher rides with the entry rather than being read off the
    /// name. `o3` and `codex-mini-latest` are OpenAI's and say so nowhere,
    /// so a prefix rule would have to guess - and would mislabel the first
    /// model that does not follow it.
    #[test]
    fn an_entry_carries_who_publishes_it() {
        let app = catalogue_app(serde_json::json!({"w": {"rates": {}}}));
        let kind = picker_kind(&app, "rates").expect("a catalogue");
        assert_eq!(catalogue_group(&kind, "model-a").as_deref(), Some("Acme"));
        assert_eq!(catalogue_group(&kind, "model-b").as_deref(), Some("Others"));
        // A key the table does not carry has nobody to name.
        assert_eq!(catalogue_group(&kind, "mine"), None);
        // And it reaches the row that prices it.
        let parent = app.fields[0].clone();
        let rows = model_fields(&app, &parent, "model-a");
        assert!(
            rows[0].help.starts_with("Acme's published list price"),
            "{}",
            rows[0].help
        );
        // A model with no published price says that instead of a vendor.
        let unknown = model_fields(&app, &parent, "mine");
        assert!(
            unknown[0].help.starts_with("No published price"),
            "{}",
            unknown[0].help
        );
    }

    /// A column you can read is a column you will type at.
    #[test]
    fn typing_a_publisher_finds_what_it_publishes() {
        let app = catalogue_app(serde_json::json!({"w": {"rates": {}}}));
        let found = zone_choices(&app, 0, "Acme", true);
        assert_eq!(found, vec![("model-a".to_string(), false)]);
        // Case is not a trap: the column is capitalised and nobody types it.
        let lower = zone_choices(&app, 0, "acme", true);
        assert_eq!(lower.len(), 1);
        // A name still beats a publisher. Were it the other way round,
        // searching a vendor's own model name would bury it under its
        // siblings.
        let both = zone_choices(&app, 0, "model-b", true);
        assert_eq!(both.first().map(|(k, _)| k.as_str()), Some("model-b"));
    }

    /// Enter on a catalogue field opens the picker rather than a list of
    /// every model's every kind, which for a real card is three hundred rows.
    #[test]
    fn a_catalogue_field_has_no_flat_screen_of_its_own() {
        let app = catalogue_app(serde_json::json!({"w": {"rates": {"model-a": {"input": 1.0}}}}));
        assert!(nested_fields(&app, 0).is_none());
        assert!(matches!(picker_kind(&app, "rates"), Some(PickKind::Catalogue(_))));
    }

    /// A model the card has never heard of still offers every kind, with no
    /// default, because a name the reader typed is the case config exists for.
    #[test]
    fn a_model_the_card_lacks_offers_every_kind_it_knows() {
        let app = catalogue_app(serde_json::json!({"w": {"rates": {}}}));
        let parent = app.fields[0].clone();
        let rows = model_fields(&app, &parent, "mine");
        let kinds: Vec<&str> = rows.iter().map(|r| r.key.as_str()).collect();
        assert_eq!(kinds, vec!["input", "output", "cache_read"]);
        assert!(rows.iter().all(|r| r.default.is_null()), "nothing invented");
    }

    /// A model is the reader's when it holds a price - there is no separate
    /// membership to get out of step with the numbers.
    #[test]
    fn a_model_counts_as_set_only_when_it_holds_a_number() {
        let empty = catalogue_app(serde_json::json!({"w": {"rates": {}}}));
        assert!(picked_zones(&empty, 0).is_empty());

        // An entry left behind with nothing in it is not "chosen": it would
        // otherwise show as configured over a column of shipped defaults.
        let husk = catalogue_app(serde_json::json!({"w": {"rates": {"model-a": {}}}}));
        assert!(picked_zones(&husk, 0).is_empty(), "an empty object is not a choice");

        let edited =
            catalogue_app(serde_json::json!({"w": {"rates": {"model-a": {"input": 3.5}}}}));
        assert_eq!(picked_zones(&edited, 0), vec!["model-a".to_string()]);

        // With nothing typed the screen shows the reader's own; tab shows
        // the whole card. That is the toggle, and it is what makes a list of
        // sixty-eight readable when three of them are yours.
        let mine = zone_choices(&edited, 0, "", false);
        assert_eq!(mine, vec![("model-a".to_string(), true)]);
        let offered = zone_choices(&edited, 0, "", true);
        assert_eq!(offered.len(), 2);
        assert_eq!(offered[0], ("model-a".to_string(), true));
        assert_eq!(offered[1], ("model-b".to_string(), false));

        // Typing searches the whole card whichever view is on, or a model
        // you have not set yet could not be found.
        let found = zone_choices(&edited, 0, "model-b", false);
        assert_eq!(found, vec![("model-b".to_string(), false)]);
    }

    /// A row whose widget offers no figure must still accept one.
    ///
    /// A model the rate card does not carry has every kind and a price for
    /// none of them, so its default is null - and validation read that as
    /// "null is the type wanted" and refused every number. The one case
    /// config exists for was the one case that could not be configured.
    #[test]
    fn a_row_with_no_default_still_takes_a_value() {
        assert!(validate_value(&serde_json::json!(3.5), &Value::Null, None).is_ok());
        assert!(validate_value(&serde_json::json!("x"), &Value::Null, None).is_ok());
        // A declared type is still enforced where there is one.
        assert!(validate_value(&serde_json::json!("x"), &serde_json::json!(1.0), None).is_err());
    }

    /// An App holding one field, for asking what kind of picker it gets.
    fn field_app(key: &str, default: Value, rule: Option<Value>) -> App {
        let mut app = catalogue_app(serde_json::json!({"w": {}}));
        app.catalogues = &[];
        app.fields = vec![Field {
            section: "w".into(),
            key: key.to_string(),
            parents: Vec::new(),
            help: String::new(),
            default,
        }];
        if let Some(rule) = rule {
            app.constraints.insert(key.to_string(), rule);
        }
        app
    }

    /// A list of strings is typed into, one entry at a time.
    ///
    /// It used to fall through to the JSON box, so adding a second host
    /// meant typing the brackets and the quotes in the right places.
    #[test]
    fn a_list_of_strings_is_filled_in_entry_by_entry() {
        // Declared outright.
        let declared = field_app(
            "accounts",
            serde_json::json!([]),
            Some(serde_json::json!({"items": "string"})),
        );
        assert!(matches!(picker_kind(&declared, "accounts"), Some(PickKind::Free)));

        // Undeclared, but the shipped default says what the elements are -
        // latency.hosts declares nothing and is plainly a list of hosts.
        let inferred = field_app("hosts", serde_json::json!(["1.1.1.1", "8.8.8.8"]), None);
        assert!(matches!(picker_kind(&inferred, "hosts"), Some(PickKind::Free)));
    }

    /// Typing `d` into an empty box types a `d`.
    ///
    /// It was guarded on the box being empty, which is exactly the moment
    /// somebody types the first character of a new entry - so `d` deleted
    /// one instead, and every entry beginning with that letter was
    /// unreachable. The verb lives on the rows now, and tab is what gets
    /// there.
    #[test]
    fn a_letter_stays_a_letter_while_the_box_has_focus() {
        let mut app = field_app("hosts", serde_json::json!(["a.example"]), None);
        app.mode = Mode::Pick {
            index: 0,
            query: String::new(),
            sel: 0,
            scroll: 0,
            show_all: false,
            on_list: false,
        };

        handle_pick_key(&mut app, "d");
        let Mode::Pick { query, on_list, .. } = &app.mode else {
            panic!("still on the picker");
        };
        assert_eq!(query, "d", "the box takes the letter");
        assert!(!on_list, "typing keeps focus in the box");

        // Tab crosses to the rows, and there it is a verb.
        handle_pick_key(&mut app, "tab");
        let Mode::Pick { on_list, .. } = &app.mode else {
            panic!("still on the picker");
        };
        assert!(on_list, "tab moves focus to the rows");

        // And typing anywhere brings focus back, so nobody is stranded.
        handle_pick_key(&mut app, "x");
        let Mode::Pick { query, on_list, .. } = &app.mode else {
            panic!("still on the picker");
        };
        assert!(!on_list, "typing returns focus to the box");
        assert_eq!(query, "dx");
    }

    /// A list of numbers is left alone.
    ///
    /// `pomodoro_flash_rgb` is one colour in three parts, not a list anybody
    /// adds a fourth entry to, and `[d]elete the entry` on it would be an
    /// offer to make a two-component colour.
    #[test]
    fn a_list_of_numbers_keeps_the_box_it_had() {
        let rgb = field_app("pomodoro_flash_rgb", serde_json::json!([246, 248, 252]), None);
        assert!(picker_kind(&rgb, "pomodoro_flash_rgb").is_none());

        let declared = field_app(
            "ports",
            serde_json::json!([]),
            Some(serde_json::json!({"items": "integer"})),
        );
        assert!(picker_kind(&declared, "ports").is_none());

        // And nothing that is not a list at all.
        let text = field_app("aggregate", serde_json::json!("median"), None);
        assert!(picker_kind(&text, "aggregate").is_none());
    }

    /// A declared picker still wins: a checklist is not a free list.
    #[test]
    fn a_declared_picker_is_not_turned_into_a_typed_list() {
        let choices = field_app(
            "agents",
            serde_json::json!([]),
            Some(serde_json::json!({"picker": {"choices": ["claude", "codex"]}})),
        );
        assert!(matches!(picker_kind(&choices, "agents"), Some(PickKind::Choices(_))));

        let zones = field_app(
            "cities",
            serde_json::json!([]),
            Some(serde_json::json!({"picker": "timezone"})),
        );
        assert!(matches!(picker_kind(&zones, "cities"), Some(PickKind::Timezone)));
    }

    /// A field the widget handed no table for keeps the box it always had.
    #[test]
    fn a_field_with_no_catalogue_is_untouched() {
        let mut app = catalogue_app(serde_json::json!({"w": {"other": {}}}));
        app.fields[0].key = "other".into();
        assert!(catalogue_for(&app, "other").is_none());
        assert!(picker_kind(&app, "other").is_none());
        assert!(nested_fields(&app, 0).is_none());
    }

    #[test]
    fn the_schema_is_the_example_file() {
        // include_str bakes the file in. A copy sitting beside this
        // widget would be a second description, and adding a key to the
        // example would not appear here.
        let on_disk = std::fs::read_to_string(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../config.example.json"),
        )
        .expect("config.example.json");
        assert_eq!(EXAMPLE, on_disk);
    }

    #[test]
    fn every_example_key_is_a_field() {
        let fields = fields_from_example(EXAMPLE).expect("example");
        let parsed: Value = serde_json::from_str(EXAMPLE).unwrap();
        let mut expected = 0usize;
        for (section, body) in parsed.as_object().unwrap() {
            if section.starts_with('_') {
                continue;
            }
            for key in body.as_object().unwrap().keys() {
                if key.starts_with('_') {
                    continue;
                }
                expected += 1;
                assert!(
                    fields.iter().any(|f| f.section == *section && f.key == *key),
                    "missing {section}.{key}"
                );
            }
        }
        assert_eq!(fields.len(), expected);
        assert!(expected > 20, "the example is not empty");
    }

    #[test]
    fn fields_follow_file_order_not_alphabetical() {
        // Widget folders own section order independently. Within one
        // declaration, serde_json's BTreeMap must not alphabetize fields
        // or detach comments from the setting they explain.
        let fields = fields_from_example(EXAMPLE).unwrap();
        let clocks: Vec<&str> = fields
            .iter()
            .filter(|f| f.section == "clocks")
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(clocks.first().copied(), Some("cities"));
        assert!(clocks.contains(&"show_hints"));
        assert!(!clocks.iter().any(|k| k.starts_with('_')));
    }

    #[test]
    fn comment_keys_become_help() {
        let fields = fields_from_example(EXAMPLE).unwrap();
        let hints = fields
            .iter()
            .find(|f| f.section == "clocks" && f.key == "show_hints")
            .unwrap();
        assert!(hints.help.contains("pomodoro key hints"));
        let token = fields
            .iter()
            .find(|f| f.section == "github" && f.key == "token")
            .unwrap();
        assert!(token.help.contains("GitHub token"));
        let history = fields
            .iter()
            .find(|f| f.section == "tailnet" && f.key == "history")
            .unwrap();
        assert!(history.help.contains("rate samples"));
    }

    #[test]
    fn a_round_trip_does_not_reorder() {
        // Measured against the workspace serde_json: Map is a BTreeMap,
        // so {"zebra":1,"_comment":"hi","alpha":{"z":1,"a":2}} dumps as
        // {"_comment":"hi","alpha":{"a":2,"z":1},"zebra":1}.
        let src = r#"{"zebra":1,"_comment":"hi","alpha":{"z":1,"a":2}}"#;
        let dumped = serde_json::to_string(&serde_json::from_str::<Value>(src).unwrap()).unwrap();
        assert_ne!(src, dumped, "the workspace serde_json now preserves order?");
        let next = set_json_path(src, &["alpha", "a"], &Value::from(3)).unwrap();
        assert_eq!(next, r#"{"zebra":1,"_comment":"hi","alpha":{"z":1,"a":3}}"#);
        let inserted = set_json_path(src, &["alpha", "b"], &Value::from(4)).unwrap();
        assert_eq!(
            inserted,
            r#"{"zebra":1,"_comment":"hi","alpha":{"z":1,"a":2,"b": 4}}"#
        );
    }

    #[test]
    fn inserting_a_missing_section_keeps_the_rest() {
        let src = "{\n  \"clocks\": {\n    \"show_hints\": true\n  }\n}\n";
        let next = set_json_path(src, &["latency", "interval"], &Value::from(0.5)).unwrap();
        assert!(next.contains("\"show_hints\": true"));
        assert!(next.contains("\"latency\""));
        assert!(next.contains("\"interval\": 0.5"));
        let parsed: Value = serde_json::from_str(&next).unwrap();
        assert_eq!(parsed["clocks"]["show_hints"], true);
        assert_eq!(parsed["latency"]["interval"], 0.5);
    }

    #[test]
    fn empty_text_becomes_a_minimal_object() {
        let next = set_json_path("", &["clocks", "show_hints"], &Value::Bool(false)).unwrap();
        let parsed: Value = serde_json::from_str(&next).unwrap();
        assert_eq!(parsed["clocks"]["show_hints"], false);
    }

    #[test]
    fn shape_is_the_example_type() {
        let expected = Value::Bool(true);
        assert_eq!(
            parse_edit("false", &expected, None).unwrap(),
            Value::Bool(false)
        );
        assert_eq!(
            parse_edit("off", &expected, None).unwrap(),
            Value::Bool(false)
        );
        assert!(parse_edit("\"nope\"", &expected, None).is_err());
        assert_eq!(
            parse_edit("hello", &Value::String(String::new()), None).unwrap(),
            Value::String("hello".into())
        );
        assert_eq!(
            parse_edit("\"hello\"", &Value::String(String::new()), None).unwrap(),
            Value::String("hello".into())
        );
        assert_eq!(
            parse_edit("true", &Value::String(String::new()), None).unwrap(),
            Value::String("true".into())
        );
        assert!(parse_edit("[1]", &Value::from(3), None).is_err());
        assert_eq!(
            parse_edit("[1, 2]", &serde_json::json!([]), None).unwrap(),
            serde_json::json!([1, 2])
        );
        assert!(parse_edit("1.5", &Value::from(1), None).is_err());
        assert!(parse_edit("-1", &Value::from(1_u64), None).is_err());

        let strings = serde_json::json!({"items": "string"});
        assert!(parse_edit(r#"["one","two"]"#, &serde_json::json!([]), Some(&strings)).is_ok());
        assert!(parse_edit("[1]", &serde_json::json!([]), Some(&strings)).is_err());

        let choices = serde_json::json!({"choices": ["total", "live"]});
        assert!(parse_edit("live", &Value::from("total"), Some(&choices)).is_ok());
        assert!(parse_edit("newest", &Value::from("total"), Some(&choices)).is_err());
    }

    #[test]
    fn a_token_is_redacted_with_no_way_to_unredact_it() {
        // There was a key to reveal one. Nothing on this screen needed it -
        // the value is in the file for anyone who has to read it - and a
        // screen that can put a live credential on a shared terminal is a
        // screen with a footgun on it. So the masking has no off switch.
        let secret = Value::String("ghp_not-a-real-token".into());
        assert_eq!(summary(&secret, true), "••••••••");
        assert_eq!(summary(&Value::String(String::new()), true), "(empty)");
        assert!(!is_secret("token_env"));
        assert!(is_secret("token"));
    }

    #[test]
    fn atomic_write_is_0600_and_refuses_junk() {
        let dir = std::env::temp_dir().join(format!(
            "opscope-config-write-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.json");
        atomic_write(&path, "{\"a\":1}\n", Some("")).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(atomic_write(&path, "{", Some("{\"a\":1}\n")).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(
            atomic_write(&path, "{\"a\":2}\n", Some("{\"other\":true}\n")).is_err()
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftover.len(), 1, "a refused write left a temp file: {leftover:?}");
        let target = dir.join("target.json");
        let linked = dir.join("linked.json");
        std::fs::write(&target, "{\"linked\":false}\n").unwrap();
        std::os::unix::fs::symlink(&target, &linked).unwrap();
        atomic_write(
            &linked,
            "{\"linked\":true}\n",
            Some("{\"linked\":false}\n"),
        )
        .unwrap();
        assert!(std::fs::symlink_metadata(&linked).unwrap().file_type().is_symlink());
        assert_eq!(
            std::fs::read_to_string(&target).unwrap(),
            "{\"linked\":true}\n"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_picks_the_same_file_as_core() {
        // The widget walks the list so it can name a higher-priority file
        // that does not parse. The file it then edits must still be the
        // one first_readable_config would pick, or a write is a no-op.
        let dir = std::env::temp_dir().join(format!(
            "opscope-config-load-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let broken = dir.join("broken.json");
        let first = dir.join("first.json");
        std::fs::write(&broken, "{").unwrap();
        std::fs::write(&first, "{\"clocks\":{\"show_hints\":false}}").unwrap();
        let paths = vec![broken, first.clone()];
        assert_eq!(crate::first_readable_config(&paths), Some(first));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn widget_name_uses_the_file_hyphen() {
        let field = Field {
            section: "herdr_panes".into(),
            parents: Vec::new(),
            key: "refresh".into(),
            help: String::new(),
            default: Value::from(4),
        };
        assert_eq!(field.widget(), "herdr-panes");
        assert_eq!(field.path(), "herdr_panes.refresh");
        let actions = Field {
            section: "github_actions".into(),
            parents: Vec::new(),
            key: "max_repos".into(),
            help: String::new(),
            default: Value::from(16),
        };
        assert_eq!(actions.widget(), "github-actions");
        let prs = Field {
            section: "github_prs".into(),
            parents: Vec::new(),
            key: "sources".into(),
            help: String::new(),
            default: serde_json::json!({}),
        };
        assert_eq!(prs.widget(), "github-prs");
        let usage = Field {
            section: "agent_usage".into(),
            parents: Vec::new(),
            key: "grok_ping".into(),
            help: String::new(),
            default: Value::Bool(false),
        };
        assert_eq!(usage.widget(), "agent-usage");
    }

    #[test]
    fn renamed_and_new_sections_are_fields() {
        // The example is the schema. usage / pr / gha are leftovers the
        // widgets still read; they are not fields here, or this pane
        // would invent keys check.rs does not keep honest.
        let fields = fields_from_example(EXAMPLE).unwrap();
        assert!(fields
            .iter()
            .any(|f| f.section == "agent_usage" && f.key == "grok_ping"));
        assert!(fields
            .iter()
            .any(|f| f.section == "github_prs" && f.key == "sources"));
        assert!(fields
            .iter()
            .any(|f| f.section == "github_actions" && f.key == "max_repos"));
        assert!(!fields
            .iter()
            .any(|f| f.section == "usage" || f.section == "pr" || f.section == "gha"));
        let sources = fields
            .iter()
            .find(|f| f.section == "github_prs" && f.key == "sources")
            .unwrap();
        assert_eq!(sources.kind(), "object");
        let parsed =
            parse_edit(r#"{"orgs":"is:open is:pr @mine"}"#, &sources.default, None)
                .unwrap();
        assert!(parsed.is_object());
    }

    #[test]
    fn leftover_section_is_read_until_the_new_name_exists() {
        let leftover = serde_json::json!({"usage": {"grok_ping": true}});
        let field = Field {
            section: "agent_usage".into(),
            parents: Vec::new(),
            key: "grok_ping".into(),
            help: String::new(),
            default: Value::Bool(false),
        };
        assert_eq!(
            live_section(&leftover, "agent_usage", Some("usage")),
            "usage"
        );
        assert_eq!(
            current_of(&leftover, &field, Some("usage")),
            Some(&Value::Bool(true))
        );

        let both = serde_json::json!({
            "agent_usage": {"grok_ping": false},
            "usage": {"grok_ping": true}
        });
        assert_eq!(
            live_section(&both, "agent_usage", Some("usage")),
            "agent_usage"
        );
        assert_eq!(
            current_of(&both, &field, Some("usage")),
            Some(&Value::Bool(false))
        );

        let prs = Field {
            section: "github_prs".into(),
            parents: Vec::new(),
            key: "limit".into(),
            help: String::new(),
            default: Value::from(50),
        };
        let old_pr = serde_json::json!({"pr": {"limit": 10}});
        assert_eq!(
            live_section(&old_pr, "github_prs", Some("pr")),
            "pr"
        );
        assert_eq!(
            current_of(&old_pr, &prs, Some("pr")),
            Some(&Value::from(10))
        );

        let actions = Field {
            section: "github_actions".into(),
            parents: Vec::new(),
            key: "max_repos".into(),
            help: String::new(),
            default: Value::from(16),
        };
        let old_gha = serde_json::json!({"gha": {"max_repos": 8}});
        assert_eq!(
            live_section(&old_gha, "github_actions", Some("gha")),
            "gha"
        );
        assert_eq!(
            current_of(&old_gha, &actions, Some("gha")),
            Some(&Value::from(8))
        );
    }

    #[test]
    fn leftover_write_stays_on_the_old_section() {
        // Creating agent_usage beside a leftover usage would make
        // agent-usage ignore the values it has been using.
        let src = "{\n  \"usage\": {\n    \"grok_ping\": false\n  }\n}\n";
        let live: Value = serde_json::from_str(src).unwrap();
        let section = live_section(&live, "agent_usage", Some("usage"));
        assert_eq!(section, "usage");
        let next = set_json_path(src, &[section, "grok_ping"], &Value::Bool(true)).unwrap();
        let parsed: Value = serde_json::from_str(&next).unwrap();
        assert_eq!(parsed["usage"]["grok_ping"], true);
        assert!(parsed.get("agent_usage").is_none());
    }

    #[test]
    fn duplicate_keys_update_the_effective_last_value() {
        let src = r#"{"clocks":{"show_hints":false,"show_hints":true}}"#;
        let next =
            set_json_path(src, &["clocks", "show_hints"], &Value::Bool(false)).unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&next).unwrap()["clocks"]["show_hints"],
            false
        );
        assert_eq!(next.matches("\"show_hints\"").count(), 2);
    }

    #[test]
    fn removing_an_override_keeps_siblings_and_comments() {
        let src = "{\n  \"clocks\": {\n    \"_show_hints_comment\": \"help\",\n    \"show_hints\": false,\n    \"work_end_hour\": 18\n  }\n}\n";
        let next = remove_json_path(src, &["clocks", "show_hints"]).unwrap();
        let parsed: Value = serde_json::from_str(&next).unwrap();
        assert!(parsed["clocks"].get("show_hints").is_none());
        assert_eq!(parsed["clocks"]["work_end_hour"], 18);
        assert_eq!(parsed["clocks"]["_show_hints_comment"], "help");
    }

    #[test]
    fn removing_repeated_overrides_does_not_reveal_an_older_duplicate() {
        let mut text = r#"{"clocks":{"show_hints":false,"show_hints":true}}"#.to_string();
        loop {
            text = remove_json_path(&text, &["clocks", "show_hints"]).unwrap();
            let parsed: Value = serde_json::from_str(&text).unwrap();
            if parsed["clocks"].get("show_hints").is_none() {
                break;
            }
        }
        assert!(!text.contains("\"show_hints\""));
    }
}
