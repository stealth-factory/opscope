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
}

struct Field {
    section: String,
    key: String,
    help: String,
    default: Value,
}

impl Field {
    fn path(&self) -> String {
        format!("{}.{}", self.section, self.key)
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
    if !same_kind(value, expected) {
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

fn summary(v: &Value, secret: bool, reveal: bool) -> String {
    if secret && !reveal {
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
    live.get(live_section(live, &field.section, legacy))
        .and_then(|s| s.get(&field.key))
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
    },
}

struct App {
    widget: &'static str,
    section: &'static str,
    legacy_section: Option<&'static str>,
    schema: &'static str,
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
    reveal: bool,
    mode: Mode,
    status: Option<String>,
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
        reveal: false,
        mode: Mode::List,
        status: None,
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
    let (key, schema_path, schema_section) = {
        let field = app.fields.get(index).ok_or("no such field")?;
        validate_value(
            &value,
            &field.default,
            app.constraints.get(&field.key),
        )?;
        (
            field.key.clone(),
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
    let path = [section.as_str(), key.as_str()];
    let next = set_json_path(&fresh_raw, &path, &value)?;
    serde_json::from_str::<Value>(&next).map_err(|e| format!("refusing to write: {e}"))?;
    atomic_write(&app.path, &next, Some(&fresh_raw))?;
    app.raw = next;
    app.live = serde_json::from_str(&app.raw).map_err(|e| e.to_string())?;
    app.exists = true;
    app.status = Some(if section != schema_section {
        format!(
            "wrote {section}.{key} · this file still uses `{section}` · restart {}",
            app.widget
        )
    } else {
        format!(
            "wrote {schema_path} · restart {} for the change to take effect",
            app.widget
        )
    });
    Ok(())
}

fn reset_field(app: &mut App, index: usize) -> Result<(), String> {
    let field = app.fields.get(index).ok_or("no such field")?;
    let key = field.key.clone();
    let schema_path = field.path();
    let schema_section = field.section.clone();
    let shown_default = summary(&field.default, field.secret(), false);
    let (fresh_raw, fresh_live) = fresh_config(app)?;
    let section = live_section(
        &fresh_live,
        &schema_section,
        app.legacy_section,
    )
    .to_string();
    if fresh_live
        .get(&section)
        .and_then(|body| body.get(&key))
        .is_none()
    {
        app.status = Some(format!("{schema_path} already uses its default"));
        return Ok(());
    }
    let mut next = fresh_raw.clone();
    loop {
        next = remove_json_path(&next, &[section.as_str(), key.as_str()])?;
        let parsed: Value = serde_json::from_str(&next)
            .map_err(|e| format!("refusing to reset {schema_path}: {e}"))?;
        if parsed
            .get(&section)
            .and_then(|body| body.get(&key))
            .is_none()
        {
            break;
        }
    }
    atomic_write(&app.path, &next, Some(&fresh_raw))?;
    app.raw = next;
    app.live = serde_json::from_str(&app.raw).map_err(|e| e.to_string())?;
    app.status = Some(format!(
        "removed {section}.{key} · default {shown_default} · restart {}",
        app.widget
    ));
    Ok(())
}

fn copy_field(app: &mut App) {
    let Some(field) = app.fields.get(app.selected) else {
        return;
    };
    if field.secret() {
        app.status = Some("not copied: that is a token".into());
        return;
    }
    let text = match current_of(&app.live, field, app.legacy_section) {
        Some(v) => compact(v),
        None => {
            app.status = Some("not copied: unset".into());
            return;
        }
    };
    if crate::clipboard(&text) {
        app.status = Some(format!("copied {}", field.path()));
    } else {
        app.status = Some("clipboard unavailable; stdout is not a terminal".into());
    }
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
}

impl PickKind {
    /// Whether an empty query means "show me only what I have chosen".
    ///
    /// True only for the catalogue. A checklist that hid its unticked half
    /// until you searched would be hiding the choice it exists to offer.
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
fn picker_kind(app: &App, key: &str) -> Option<PickKind> {
    let rule = app.constraints.get(key)?.as_object()?.get("picker")?;
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
    current_of(&app.live, field, app.legacy_section)
        .or(Some(&field.default))
        .and_then(Value::as_array)
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
fn zone_choices(app: &App, index: usize, query: &str) -> Vec<(String, bool)> {
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

fn handle_pick_key(app: &mut App, key: &str) -> bool {
    let Mode::Pick { index, query, sel, scroll } = &mut app.mode else {
        return false;
    };
    let (index, mut q, mut s_, mut sc) = (*index, query.clone(), *sel, *scroll);
    let total = zone_choices(app, index, &q).len();
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
        "enter" => {
            if let Some((zone, _)) = zone_choices(app, index, &q).get(s_).cloned() {
                let label = alias_hit(&zone, &q).map(str::to_string);
                toggle_zone(app, index, &zone, label);
            }
        }
        other if other.chars().count() == 1 => {
            let ch = other.chars().next().unwrap();
            if !ch.is_control() {
                q.push(ch);
                s_ = 0;
            }
        }
        _ => {}
    }
    let total = zone_choices(app, index, &q).len();
    if let Mode::Pick { query, sel, scroll, .. } = &mut app.mode {
        *query = q;
        *sel = s_.min(total.saturating_sub(1));
        *scroll = sc;
    }
    false
}

fn handle_list_key(app: &mut App, key: &str) -> bool {
    match key {
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
        "s" | "S" => app.reveal = !app.reveal,
        "r" | "R" => {
            let keep = (app.selected, app.reveal);
            *app = load(SettingsSpec {
                widget: app.widget,
                section: app.section,
                legacy_section: app.legacy_section,
                schema: app.schema,
            });
            app.selected = keep.0.min(app.fields.len().saturating_sub(1));
            app.reveal = keep.1;
            app.status = Some("reloaded from disk".into());
        }
        "c" | "C" => copy_field(app),
        "d" | "D" => {
            if let Err(e) = reset_field(app, app.selected) {
                app.status = Some(e);
            }
        }
        "enter" => {
            if let Some(field) = app.fields.get(app.selected) {
                if field.default.is_boolean() {
                    let current = current_of(&app.live, field, app.legacy_section)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(field.default.as_bool().unwrap_or(false));
                    if let Err(e) = write_field(app, app.selected, Value::Bool(!current)) {
                        app.status = Some(e);
                    }
                } else if picker_kind(app, &field.key).is_some() {
                    app.mode = Mode::Pick {
                        index: app.selected,
                        query: String::new(),
                        sel: 0,
                        scroll: 0,
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
    let mut body = vec![crate::title(&format!("{} settings", app.widget), w, &p.accent)];
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
                " {} keys · {} unset · a running widget will not pick a change up until it restarts",
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

    let hints = list_hints(p, app.reveal);
    let foot: Vec<String> = crate::pack_hints(&hints, w.saturating_sub(2), "  ")
        .into_iter()
        .map(|l| format!(" {l}"))
        .collect();

    let aside = 8usize;
    let room = h.saturating_sub(body.len() + foot.len() + aside).max(1);
    if !app.fields.is_empty() {
        app.selected = app.selected.min(app.fields.len() - 1);
        app.scroll = if app.chase {
            crate::follow(app.scroll, app.selected, room)
        } else {
            app.scroll
                .min(app.fields.len().saturating_sub(room))
        };
        app.chase = false;
    }
    let key_w = app
        .fields
        .iter()
        .map(|f| f.key.chars().count())
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

    for (i, field) in app
        .fields
        .iter()
        .enumerate()
        .skip(app.scroll)
        .take(room)
    {
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
                crate::pad(&field.key, key_w + 1),
            ),
        ];
        if show_value {
            let shown = match current {
                Some(v) => summary(v, field.secret(), app.reveal),
                None => "—".into(),
            };
            parts.push((
                c_of(if set { &p.txt } else { &p.dim }),
                format!(" {}", crate::pad(&shown, value_w)),
            ));
        }
        if show_default {
            let shown = summary(&field.default, field.secret(), app.reveal);
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
                .map(|value| summary(value, field.secret(), app.reveal))
                .unwrap_or_else(|| "—".into()),
            summary(&field.default, field.secret(), app.reveal),
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
        let constraints = constraint_summary(app.constraints.get(&field.key));
        if !constraints.is_empty() {
            for line in wrap_help(&constraints, w.saturating_sub(4), 2) {
                body.push(crate::seg(
                    &[(p.lbl.as_str(), format!("  {line}"))],
                    w.saturating_sub(1),
                ));
            }
        }
        let help = if field.help.is_empty() {
            format!("{} · restart {} for a change to take effect", field.kind(), field.widget())
        } else {
            field.help.clone()
        };
        for line in wrap_help(&help, w.saturating_sub(4), 2) {
            body.push(crate::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1)));
        }
        if !field.help.is_empty() {
            body.push(crate::seg(
                &[(
                    p.dim.as_str(),
                    format!("  restart {} for a change to take effect", field.widget()),
                )],
                w.saturating_sub(1),
            ));
        }
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

    while body.len() + foot.len() < h {
        body.push(String::new());
    }
    body.extend(foot);
    body.truncate(h);
    body
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
            format!(
                "  {} · default {} · restart {} after writing",
                field.kind(),
                summary(&field.default, field.secret(), true),
                field.widget()
            ),
        )],
        w.saturating_sub(1),
    ));
    if field.secret() {
        body.push(crate::seg(
            &[(p.warn.as_str(), "  a token · never copied to the clipboard".into())],
            w.saturating_sub(1),
        ));
    }
    body.push(String::new());
    let room = w.saturating_sub(4).max(8);
    let chars: Vec<char> = buffer.chars().collect();
    let start = if *cursor >= room {
        cursor + 1 - room
    } else {
        0
    };
    let window: String = chars.iter().skip(start).take(room).collect();
    body.push(crate::seg(
        &[(p.txt.as_str(), format!("  {window}"))],
        w.saturating_sub(1),
    ));
    let caret_at = cursor.saturating_sub(start);
    body.push(crate::seg(
        &[(p.accent.as_str(), format!("  {}▲", " ".repeat(caret_at)))],
        w.saturating_sub(1),
    ));
    if let Some(err) = error {
        body.push(crate::seg(&[(p.bad.as_str(), format!("  {err}"))], w.saturating_sub(1)));
    }

    let hints: Vec<Vec<(&str, String)>> = vec![
        vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " write".into())],
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

fn draw_pick(app: &App, w: usize, h: usize, p: &Palette) -> Vec<String> {
    let Mode::Pick { index, query, sel, scroll } = &app.mode else {
        return vec![crate::title(&format!("{} settings", app.widget), w, &p.accent)];
    };
    let field = &app.fields[*index];
    let choices = zone_choices(app, *index, query);
    let kind = picker_kind(app, &field.key).unwrap_or(PickKind::Timezone);
    let total_choices = match &kind {
        PickKind::Timezone => chrono_tz::TZ_VARIANTS.len(),
        PickKind::Choices(all) => all.len(),
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
    let catalogue = matches!(kind, PickKind::Timezone);
    let heading = match (catalogue, query.is_empty()) {
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
    body.push(crate::seg(&[(p.dim.as_str(), heading)], w.saturating_sub(1)));
    // How the widget will order what is picked here, said on the screen
    // rather than only in the file's comment: whoever is arranging a list is
    // exactly the person who would otherwise assume the order they choose is
    // the order they get. Which for one of these is true and the other not.
    let ordering = if catalogue {
        "  drawn west to east by each zone's offset, not in the order added"
    } else {
        "  shown in the order picked here"
    };
    body.push(crate::seg(
        &[(p.dim.as_str(), ordering.to_string())],
        w.saturating_sub(1),
    ));
    body.push(String::new());
    // A field you can see the edges of. A bare caret on a bare line reads as
    // output rather than as somewhere to type, and the one thing this screen
    // has to make obvious is that typing does something.
    //
    // Blinking on the wall clock rather than a frame counter, so the cadence
    // is the same half-second whatever the redraw interval happens to be -
    // and so it does not race the 80ms loop into a flicker.
    let lit = (crate::now() * 2.0) as u64 % 2 == 0;
    let caret = if lit { "▏" } else { " " };
    let box_w = w.saturating_sub(6).max(12);
    let shown: String = {
        let chars: Vec<char> = query.chars().collect();
        let room = box_w.saturating_sub(2);
        // Keep the tail in view: what was typed last is what is being
        // corrected, and a query long enough to scroll is a query being
        // narrowed.
        let start = chars.len().saturating_sub(room);
        chars[start..].iter().collect()
    };
    let field = crate::bg(24, 36, 50);
    let ink = format!("{field}{}", p.txt);
    let ghost = format!("{field}{}", p.dim);
    let edge = format!("{field}{}", p.accent);
    let used = shown.chars().count() + 1;
    body.push(crate::seg(
        &[
            (p.dim.as_str(), "  ".to_string()),
            (edge.as_str(), " ".to_string()),
            (ink.as_str(), shown.clone()),
            (edge.as_str(), caret.to_string()),
            (
                ghost.as_str(),
                if query.is_empty() {
                    // Only where it fits, and never over what was typed.
                    let hint = if catalogue {
                        "type a city or a zone"
                    } else {
                        "type to filter"
                    };
                    if hint.len() + used + 1 <= box_w {
                        format!(" {hint}")
                    } else {
                        String::new()
                    }
                } else {
                    String::new()
                },
            ),
            (field.as_str(), " ".repeat(box_w)),
        ],
        w.saturating_sub(1),
    ));
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
        let why = match (catalogue, query.is_empty()) {
            (true, true) => "  no cities yet - type to search and add one".to_string(),
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
                        (None, true) if catalogue => picked_label(app, *index, zone)
                            .unwrap_or_else(|| zone_label(zone)),
                        (None, _) => String::new(),
                    },
                ),
                (tint.as_str(), " ".repeat(w)),
            ],
            w.saturating_sub(1),
        ));
    }

    let hints: Vec<Vec<(&str, String)>> = if !catalogue {
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

fn list_hints<'a>(p: &'a Palette, reveal: bool) -> Vec<Vec<(&'a str, String)>> {
    let secret = if reveal { "[s]hide tokens" } else { "[s]how tokens" };
    vec![
        vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
        vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " edit".into())],
        vec![(p.dim.as_str(), secret.into())],
        vec![(p.dim.as_str(), "[r]eload".into())],
        vec![(p.dim.as_str(), "[d]efault".into())],
        vec![(p.dim.as_str(), "[c]opy".into())],
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

#[cfg(test)]
mod tests {
    use super::*;

    const EXAMPLE: &str = include_str!("../../config.example.json");

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
    fn tokens_are_redacted_until_revealed() {
        let secret = Value::String("ghp_not-a-real-token".into());
        assert_eq!(summary(&secret, true, false), "••••••••");
        assert!(summary(&secret, true, true).contains("ghp_"));
        assert_eq!(summary(&Value::String(String::new()), true, false), "(empty)");
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
            key: "refresh".into(),
            help: String::new(),
            default: Value::from(4),
        };
        assert_eq!(field.widget(), "herdr-panes");
        assert_eq!(field.path(), "herdr_panes.refresh");
        let actions = Field {
            section: "github_actions".into(),
            key: "max_repos".into(),
            help: String::new(),
            default: Value::from(16),
        };
        assert_eq!(actions.widget(), "github-actions");
        let prs = Field {
            section: "github_prs".into(),
            key: "sources".into(),
            help: String::new(),
            default: serde_json::json!({}),
        };
        assert_eq!(prs.widget(), "github-prs");
        let usage = Field {
            section: "agent_usage".into(),
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
