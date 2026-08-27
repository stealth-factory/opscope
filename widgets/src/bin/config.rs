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

//! Every setting the widgets actually read, and a way to change it.
//!
//! The field list is `config.example.json`, compiled in. A key added there
//! appears here with no second edit. Writes go to the file `load_config`
//! would actually read, in place, so key order and `_comment` placement
//! survive; a round-trip through serde_json would sort every object.

use std::fs::OpenOptions;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use opscope_core as tc;
use serde_json::Value;

/// The schema. Adding a key to the example and rebuilding is enough; this
/// file is not described a second time anywhere in this widget.
const EXAMPLE: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../config.example.json"
));

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
        kind_name(&self.default)
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
    std::mem::discriminant(a) == std::mem::discriminant(b)
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
        let Some(found) = entries.into_iter().find(|e| e.key == *key) else {
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
    if let Some(found) = entries.iter().find(|e| e.key == key) {
        let mut out = String::with_capacity(text.len() + rendered.len());
        out.push_str(&text[..found.value_start]);
        out.push_str(&rendered);
        out.push_str(&text[found.value_end..]);
        return Ok(out);
    }
    insert_member(&text, parent, key, &rendered)
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
fn atomic_write(path: &Path, contents: &str) -> Result<(), String> {
    serde_json::from_str::<Value>(contents)
        .map_err(|e| format!("refusing to write: {e}"))?;
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
    let tmp = path.with_file_name(format!(".{name}.tmp-{}", std::process::id()));
    let write = || -> Result<(), String> {
        let mut opts = OpenOptions::new();
        opts.write(true).create(true).truncate(true).mode(0o600);
        let mut file = opts
            .open(&tmp)
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        file.write_all(contents.as_bytes())
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        file.sync_all()
            .map_err(|e| format!("{}: {e}", tmp.display()))?;
        Ok(())
    };
    if let Err(e) = write() {
        let _ = std::fs::remove_file(&tmp);
        return Err(e);
    }
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
    std::fs::rename(&tmp, path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        format!("{}: {e}", path.display())
    })?;
    Ok(())
}

fn parse_edit(raw: &str, expected: &Value) -> Result<Value, String> {
    let trimmed = raw.trim();
    if let Ok(v) = serde_json::from_str::<Value>(trimmed) {
        if same_kind(&v, expected) {
            return Ok(v);
        }
        return Err(format!(
            "expected {}, got {}",
            kind_name(expected),
            kind_name(&v)
        ));
    }
    if expected.is_string() {
        return Ok(Value::String(raw.to_string()));
    }
    if expected.is_boolean() {
        return match trimmed.to_ascii_lowercase().as_str() {
            "true" | "yes" | "on" | "1" => Ok(Value::Bool(true)),
            "false" | "no" | "off" | "0" => Ok(Value::Bool(false)),
            _ => Err("expected bool".into()),
        };
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

fn current_of<'a>(live: &'a Value, field: &Field) -> Option<&'a Value> {
    live.get(&field.section).and_then(|s| s.get(&field.key))
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
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
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
}

struct App {
    fields: Vec<Field>,
    live: Value,
    raw: String,
    path: PathBuf,
    exists: bool,
    skipped: Vec<String>,
    selected: usize,
    scroll: usize,
    reveal: bool,
    mode: Mode,
    status: Option<String>,
}

fn load() -> App {
    let fields = fields_from_example(EXAMPLE).expect("the baked-in example is valid JSON");
    let mut skipped = Vec::new();
    let mut found = None;
    for path in tc::config_paths() {
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
        None => (tc::default_config_path(), String::new(), serde_json::json!({}), false),
    };
    App {
        fields,
        live,
        raw,
        path,
        exists,
        skipped,
        selected: 0,
        scroll: 0,
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

fn write_field(app: &mut App, index: usize, value: Value) -> Result<(), String> {
    let field = app.fields.get(index).ok_or("no such field")?;
    if !same_kind(&value, &field.default) {
        return Err(format!(
            "expected {}, got {}",
            field.kind(),
            kind_name(&value)
        ));
    }
    let path = [field.section.as_str(), field.key.as_str()];
    let next = set_json_path(&app.raw, &path, &value)?;
    serde_json::from_str::<Value>(&next).map_err(|e| format!("refusing to write: {e}"))?;
    atomic_write(&app.path, &next)?;
    let widget = field.widget();
    let name = field.path();
    app.raw = next;
    app.live = serde_json::from_str(&app.raw).map_err(|e| e.to_string())?;
    app.exists = true;
    app.status = Some(format!(
        "wrote {name} · restart {widget} for the change to take effect"
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
    let text = match current_of(&app.live, field) {
        Some(v) => compact(v),
        None => {
            app.status = Some("not copied: unset".into());
            return;
        }
    };
    if tc::clipboard(&text) {
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

fn handle_list_key(app: &mut App, key: &str) -> bool {
    match key {
        "q" | "Q" => return true,
        "up" | "k" | "K" => move_sel(app, -1),
        "down" | "j" | "J" => move_sel(app, 1),
        "pgup" => move_sel(app, -10),
        "pgdn" => move_sel(app, 10),
        "home" => app.selected = 0,
        "end" => app.selected = app.fields.len().saturating_sub(1),
        "s" | "S" => app.reveal = !app.reveal,
        "r" | "R" => {
            let keep = (app.selected, app.reveal);
            *app = load();
            app.selected = keep.0.min(app.fields.len().saturating_sub(1));
            app.reveal = keep.1;
            app.status = Some("reloaded from disk".into());
        }
        "c" | "C" => copy_field(app),
        "enter" => {
            if let Some(field) = app.fields.get(app.selected) {
                if field.default.is_boolean() {
                    let current = current_of(&app.live, field)
                        .and_then(|v| v.as_bool())
                        .unwrap_or(field.default.as_bool().unwrap_or(false));
                    if let Err(e) = write_field(app, app.selected, Value::Bool(!current)) {
                        app.status = Some(e);
                    }
                } else {
                    let seed = edit_seed(field, current_of(&app.live, field));
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
            match parse_edit(buffer, &field.default) {
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
    let mut body = vec![tc::title("config", w, &p.accent)];
    let path_note = if app.exists {
        format!(" {}", app.path.display())
    } else {
        format!(" {} · file does not exist yet · will be created 0600", app.path.display())
    };
    body.push(tc::seg(&[(p.dim.as_str(), path_note)], w.saturating_sub(1)));
    let unset = app
        .fields
        .iter()
        .filter(|f| current_of(&app.live, f).is_none())
        .count();
    body.push(tc::seg(
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
        body.push(tc::seg(&[(p.warn.as_str(), format!(" {note}"))], w.saturating_sub(1)));
    }
    if let Some(status) = &app.status {
        let colour = if status.starts_with("wrote ") || status.starts_with("copied ") {
            p.ok.as_str()
        } else if status.starts_with("not copied") || status.contains("refusing") {
            p.bad.as_str()
        } else {
            p.dim.as_str()
        };
        body.push(tc::seg(&[(colour, format!(" {status}"))], w.saturating_sub(1)));
    }
    body.push(String::new());

    let hints = list_hints(p, app.reveal);
    let foot: Vec<String> = tc::pack_hints(&hints, w.saturating_sub(2), "  ")
        .into_iter()
        .map(|l| format!(" {l}"))
        .collect();

    let aside = 5usize;
    let room = h.saturating_sub(body.len() + foot.len() + aside).max(1);
    if !app.fields.is_empty() {
        app.selected = app.selected.min(app.fields.len() - 1);
        app.scroll = tc::follow(app.scroll, app.selected, room);
    }
    let key_w = app
        .fields
        .iter()
        .map(|f| f.path().chars().count())
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
            tc::bg(38, 56, 76)
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
        let current = current_of(&app.live, field);
        let set = current.is_some();
        let mark = if here { "▸ " } else { "  " };
        let mut parts: Vec<(String, String)> = vec![
            (
                c_of(if here { &p.accent } else { &p.dim }),
                mark.to_string(),
            ),
            (c_of(if here { &p.txt } else { &p.lbl }), tc::pad(&field.path(), key_w + 1)),
        ];
        if show_value {
            let shown = match current {
                Some(v) => summary(v, field.secret(), app.reveal),
                None => "—".into(),
            };
            parts.push((
                c_of(if set { &p.txt } else { &p.dim }),
                format!(" {}", tc::pad(&shown, value_w)),
            ));
        }
        if show_default {
            let shown = summary(&field.default, field.secret(), app.reveal);
            parts.push((
                c_of(&p.dim),
                format!(" {}", tc::pad(&shown, value_w)),
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
        body.push(tc::seg(&refs, w.saturating_sub(1)));
    }

    if let Some(field) = app.fields.get(app.selected) {
        body.push(String::new());
        body.push(tc::seg(
            &[(p.lbl.as_str(), format!(" ── {} ── ", field.path().to_uppercase()))],
            w.saturating_sub(1),
        ));
        let help = if field.help.is_empty() {
            format!("{} · restart {} for a change to take effect", field.kind(), field.widget())
        } else {
            field.help.clone()
        };
        for line in wrap_help(&help, w.saturating_sub(4), 2) {
            body.push(tc::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1)));
        }
        if !field.help.is_empty() {
            body.push(tc::seg(
                &[(
                    p.dim.as_str(),
                    format!("  restart {} for a change to take effect", field.widget()),
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
        return vec![tc::title("config", w, &p.accent)];
    };
    let field = &app.fields[*index];
    let mut body = vec![tc::title("config", w, &p.accent)];
    body.push(tc::seg(
        &[(p.dim.as_str(), format!(" {}", app.path.display()))],
        w.saturating_sub(1),
    ));
    body.push(String::new());
    body.push(tc::seg(
        &[(p.lbl.as_str(), format!(" ── {} ── ", field.path().to_uppercase()))],
        w.saturating_sub(1),
    ));
    for line in wrap_help(&field.help, w.saturating_sub(4), 3) {
        body.push(tc::seg(&[(p.dim.as_str(), format!("  {line}"))], w.saturating_sub(1)));
    }
    body.push(tc::seg(
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
        body.push(tc::seg(
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
    body.push(tc::seg(
        &[(p.txt.as_str(), format!("  {window}"))],
        w.saturating_sub(1),
    ));
    let caret_at = cursor.saturating_sub(start);
    body.push(tc::seg(
        &[(p.accent.as_str(), format!("  {}▲", " ".repeat(caret_at)))],
        w.saturating_sub(1),
    ));
    if let Some(err) = error {
        body.push(tc::seg(&[(p.bad.as_str(), format!("  {err}"))], w.saturating_sub(1)));
    }

    let hints: Vec<Vec<(&str, String)>> = vec![
        vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " write".into())],
        vec![(p.dim.as_str(), "esc cancel".into())],
    ];
    let foot: Vec<String> = tc::pack_hints(&hints, w.saturating_sub(2), "  ")
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
        vec![(p.dim.as_str(), "[c]opy".into())],
        vec![(p.dim.as_str(), "[q]uit".into())],
    ]
}

fn main() {
    tc::maybe_help(include_str!("config_help.txt"));
    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut app = load();

    loop {
        for key in keyboard.poll() {
            let quit = match app.mode {
                Mode::List => handle_list_key(&mut app, &key),
                Mode::Edit { .. } => handle_edit_key(&mut app, &key),
            };
            if quit {
                keyboard.restore();
                tc::restore_screen();
                return;
            }
        }
        let (w, h) = tc::size();
        let body = match app.mode {
            Mode::List => draw_list(&mut app, w, h, &p),
            Mode::Edit { .. } => draw_edit(&app, w, h, &p),
        };
        tc::draw(&body, w, h);
        std::thread::sleep(Duration::from_millis(80));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        // The trap: BTreeMap would emit clocks before deployments before
        // github, and _comment first inside each section. File order is
        // latency, clocks, … and comments stay attached to their keys.
        let fields = fields_from_example(EXAMPLE).unwrap();
        let sections: Vec<&str> = fields.iter().map(|f| f.section.as_str()).collect();
        let first_latency = sections.iter().position(|s| *s == "latency").unwrap();
        let first_clocks = sections.iter().position(|s| *s == "clocks").unwrap();
        assert!(
            first_latency < first_clocks,
            "latency must precede clocks, as it does in the example file"
        );
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
        assert!(token.help.contains("CLASSIC PAT"));
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
        assert_eq!(parse_edit("false", &expected).unwrap(), Value::Bool(false));
        assert_eq!(parse_edit("off", &expected).unwrap(), Value::Bool(false));
        assert!(parse_edit("\"nope\"", &expected).is_err());
        assert_eq!(
            parse_edit("hello", &Value::String(String::new())).unwrap(),
            Value::String("hello".into())
        );
        assert_eq!(
            parse_edit("\"hello\"", &Value::String(String::new())).unwrap(),
            Value::String("hello".into())
        );
        assert!(parse_edit("[1]", &Value::from(3)).is_err());
        assert_eq!(
            parse_edit("[1, 2]", &serde_json::json!([])).unwrap(),
            serde_json::json!([1, 2])
        );
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
        atomic_write(&path, "{\"a\":1}\n").unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        assert!(atomic_write(&path, "{").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "{\"a\":1}\n");
        let leftover: Vec<_> = std::fs::read_dir(&dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name())
            .collect();
        assert_eq!(leftover.len(), 1, "a refused write left a temp file: {leftover:?}");
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
        assert_eq!(tc::first_readable_config(&paths), Some(first));
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
    }
}
