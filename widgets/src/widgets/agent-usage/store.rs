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

//! How far the log readers got, kept across restarts.
//!
//! Without it every start read every agent's logs from the first byte -
//! gigabytes, on a machine that has used Codex for a while - to arrive at
//! the counts the last run already had. So each reader's tails are written
//! down: which file (device, inode, size, mtime), how far it was read, a
//! hash of its first kilobyte, and the counts its lines added up to. The
//! next start checks each against the file exactly as a tail in memory is
//! checked, and reads only what is new.
//!
//! **Counters only.** Day, model, token counts, and the keys that stop a
//! replayed turn being counted twice - uuids, session ids, timestamps.
//! Never a line, never the first kilobyte itself: the files are other
//! people's conversations, and this one sits in a state directory.

use std::io::Write;

use crate::shared::*;

/// Bumped whenever what is written changes shape. A file of any other
/// version is ignored rather than half-understood, and the logs are read
/// again from the start - slow once, never wrong.
pub const VERSION: u64 = 1;

/// How often the progress is written while the logs keep changing. A
/// restart then re-reads at most this much history, which at the budget's
/// pace is a few passes.
pub const SAVE_EVERY: f64 = 300.0;

pub fn path() -> String {
    format!("{}/opscope/agent-usage-logs.jsonl", crate::claude::snapshot_state_home())
}

/// The file is JSON Lines: a version line, then for each reader a line
/// naming it, each of its tails on a line, and each tail's records one to
/// a line after it. Written and read a line at a time, so neither a save
/// nor a restore ever holds the whole of it as one document - fifteen
/// megabytes of counts here, which parsed whole cost more than the counts.
#[derive(Clone, Debug, PartialEq)]
enum Set {
    Claude(String),
    Codex,
    Grok,
    Antigravity,
}

fn write_set<P: Persist>(
    out: &mut impl Write,
    name: serde_json::Value,
    logs: &Logs<P>,
) -> std::io::Result<()> {
    writeln!(out, "{}", serde_json::json!({ "set": name }))?;
    for (path, tail) in &logs.tails {
        // How many records follow, so a file cut off between two of them
        // is not taken for a tail that had only those.
        let count = tail.parser.records().map_or(0, |r| r.len());
        writeln!(out, "{}", serde_json::json!({ "tail": tail.meta(path), "records": count }))?;
        for (key, record) in tail.parser.records().into_iter().flatten() {
            writeln!(out, "{}", record_line(key, record))?;
        }
    }
    Ok(())
}

/// Write every reader's progress to `out`.
fn write_all(caches: &Caches, out: &mut impl Write) -> std::io::Result<()> {
    writeln!(out, "{}", serde_json::json!({ "version": VERSION }))?;
    for (dir, logs) in &caches.transcripts {
        write_set(out, serde_json::json!(["claude", dir]), logs)?;
    }
    write_set(out, serde_json::json!(["codex"]), &caches.rollouts)?;
    write_set(out, serde_json::json!(["grok"]), &caches.grok)?;
    write_set(out, serde_json::json!(["antigravity"]), &caches.history)?;
    Ok(())
}

fn set_of(v: &serde_json::Value) -> Option<Set> {
    let name = v.as_array()?;
    Some(match name.first()?.as_str()? {
        "claude" => Set::Claude(name.get(1)?.as_str()?.to_string()),
        "codex" => Set::Codex,
        "grok" => Set::Grok,
        "antigravity" => Set::Antigravity,
        _ => return None,
    })
}

/// Hand one tail, read whole from the file, to the reader its set names.
fn place(caches: &mut Caches, set: &Set, meta: &serde_json::Value, records: Records) {
    match set {
        Set::Claude(dir) => {
            if let Some((path, t)) = Tail::load(meta, records) {
                caches.transcripts.entry(dir.clone()).or_default().tails.insert(path, t);
            }
        }
        Set::Codex => {
            if let Some((path, t)) = Tail::load(meta, records) {
                caches.rollouts.tails.insert(path, t);
            }
        }
        Set::Grok => {
            if let Some((path, t)) = Tail::load(meta, records) {
                caches.grok.tails.insert(path, t);
            }
        }
        Set::Antigravity => {
            if let Some((path, t)) = Tail::load(meta, records) {
                caches.history.tails.insert(path, t);
            }
        }
    }
}

/// Put back what `write_all` wrote. A file of another version, or one
/// whose first line is not a version, leaves the caches as they were; a
/// line that does not parse ends the restore at the last whole tail, and
/// whatever is missing is read from the logs again. A tail is whole only
/// when every record its line counted arrived: a file cut off between two
/// lines parses cleanly, and would otherwise restore a tail missing some of
/// its records at the full offset, so they would never be read again.
fn read_all(caches: &mut Caches, input: impl std::io::BufRead) -> bool {
    let mut lines = input.lines();
    let first = lines.next().and_then(|l| l.ok());
    let version = first
        .and_then(|l| serde_json::from_str::<serde_json::Value>(&l).ok())
        .and_then(|v| v["version"].as_u64());
    if version != Some(VERSION) {
        return false;
    }
    let mut set: Option<Set> = None;
    let mut pending: Option<(serde_json::Value, u64, Records)> = None;
    let flush = |caches: &mut Caches, set: &Option<Set>, pending: &mut Option<(serde_json::Value, u64, Records)>| {
        if let (Some(set), Some((meta, count, records))) = (set, pending.take()) {
            if records.len() as u64 == count {
                place(caches, set, &meta, records);
            }
        }
    };
    for line in lines {
        let Ok(line) = line else {
            break;
        };
        let Ok(v) = serde_json::from_str::<serde_json::Value>(&line) else {
            pending = None;
            break;
        };
        if let Some(name) = v.get("set") {
            flush(caches, &set, &mut pending);
            set = set_of(name);
        } else if let Some(meta) = v.get("tail") {
            flush(caches, &set, &mut pending);
            let Some(count) = v["records"].as_u64() else {
                pending = None;
                break;
            };
            pending = Some((meta.clone(), count, Records::new()));
        } else if let (Some((_, _, records)), Some((key, record))) = (pending.as_mut(), record_from(&v)) {
            records.insert(key, record);
        }
    }
    flush(caches, &set, &mut pending);
    true
}

pub fn restore(caches: &mut Caches, path: &str) -> bool {
    let Ok(file) = std::fs::File::open(path) else {
        return false;
    };
    read_all(caches, std::io::BufReader::new(file))
}

/// Write the progress beside the file and move it into place, so a crash
/// mid-write leaves the last good one rather than half of a new one.
/// Readable by its owner only: counts and session ids are not secrets, but
/// they are nobody else's business either.
pub fn save(caches: &Caches, path: &str) -> std::io::Result<()> {
    use std::os::unix::fs::OpenOptionsExt;
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let next = format!("{path}.next");
    let file = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(true)
        .mode(0o600)
        .open(&next)?;
    let mut out = std::io::BufWriter::new(file);
    write_all(caches, &mut out)?;
    out.flush()?;
    drop(out);
    std::fs::rename(&next, path)
}

/// Whether anything was read since the last save, clearing the marks.
fn take_dirty(caches: &mut Caches) -> bool {
    let mut dirty = false;
    for logs in caches.transcripts.values_mut() {
        dirty |= std::mem::take(&mut logs.dirty);
    }
    dirty |= std::mem::take(&mut caches.rollouts.dirty);
    dirty |= std::mem::take(&mut caches.grok.dirty);
    dirty |= std::mem::take(&mut caches.history.dirty);
    dirty
}

/// When the poller should write the progress down: something was read, and
/// either `SAVE_EVERY` has passed or a first read has just finished - the
/// moment the most work would otherwise be lost.
pub struct Saver {
    saved_at: f64,
    owed: bool,
    was_catching_up: bool,
}

impl Saver {
    pub fn new(at: f64) -> Saver {
        Saver { saved_at: at, owed: false, was_catching_up: false }
    }

    /// True when this pass should save.
    pub fn due(&mut self, caches: &mut Caches, at: f64) -> bool {
        self.owed |= take_dirty(caches);
        let catching_up = caches.catching_up();
        let finished = self.was_catching_up && !catching_up;
        self.was_catching_up = catching_up;
        if self.owed && (finished || at - self.saved_at >= SAVE_EVERY) {
            self.owed = false;
            self.saved_at = at;
            return true;
        }
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{empty_tokens, now};
    use crate::shared::growing::Log;

    fn transcript_line(uid: &str, said: &str) -> String {
        serde_json::json!({"type": "assistant", "uuid": uid,
            "timestamp": "2026-10-20T10:00:00Z",
            "message": {"model": "claude-a", "content": said,
                "usage": {"input_tokens": 10, "output_tokens": 5}}})
        .to_string()
    }

    #[test]
    fn a_restart_resumes_from_what_was_saved_and_reads_only_what_is_new() {
        let log = Log::new("store-resume");
        let dir = std::path::Path::new(&log.path).parent().unwrap().to_string_lossy().to_string();
        let words = "a private sentence nobody else should read";
        for i in 0..20 {
            log.append(&format!("{}\n", transcript_line(&format!("u-{i}"), words)));
        }
        let at = now();
        let mut first = Caches::default();
        let (listed, all) = list_logs(&dir, ".jsonl", None, at);
        let logs = first.transcripts.entry(dir.clone()).or_default();
        logs.pass((listed, all), false, at, &mut Budget::unlimited(), |_| crate::claude::TranscriptLines::default());
        let saved = dir.clone() + "/store.json";
        save(&first, &saved).unwrap();
        // Counters only: not a word of what was said, nor the start of the
        // file it was said in.
        let body = std::fs::read(&saved).unwrap();
        let body = String::from_utf8_lossy(&body);
        assert!(!body.contains("private sentence"), "{body}");
        assert!(!body.contains("\"content\""), "{body}");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&saved).unwrap().permissions().mode() & 0o777;
            assert_eq!(mode, 0o600);
        }
        // A new process: restored, then one more line is appended.
        log.append(&format!("{}\n", transcript_line("u-20", words)));
        let mut second = Caches::default();
        assert!(restore(&mut second, &saved));
        let logs = second.transcripts.get_mut(&dir).unwrap();
        let (listed, all) = list_logs(&dir, ".jsonl", None, at + 1.0);
        logs.pass((listed, all), false, at + 1.0, &mut Budget::unlimited(), |_| {
            crate::claude::TranscriptLines::default()
        });
        let t = &logs.tails[&log.path];
        assert_eq!(t.parser.records.len(), 21);
        let line = transcript_line("u-20", words).len() as u64 + 1;
        assert_eq!(t.read, line, "only the appended line came off the disk");
    }

    #[test]
    fn a_log_replaced_while_the_widget_was_not_running_is_read_again() {
        let log = Log::new("store-replaced");
        let dir = std::path::Path::new(&log.path).parent().unwrap().to_string_lossy().to_string();
        log.append(&format!("{}\n{}\n", transcript_line("a", ""), transcript_line("b", "")));
        let at = now();
        let mut first = Caches::default();
        let (listed, all) = list_logs(&dir, ".jsonl", None, at);
        first.transcripts.entry(dir.clone()).or_default().pass(
            (listed, all),
            false,
            at,
            &mut Budget::unlimited(),
            |_| crate::claude::TranscriptLines::default(),
        );
        let saved = dir.clone() + "/store.json";
        save(&first, &saved).unwrap();
        // Replaced by a longer, different log under the same name.
        log.replace(&format!(
            "{}\n{}\n{}\n",
            transcript_line("x", "longer"),
            transcript_line("y", "longer"),
            transcript_line("z", "longer")
        ));
        let mut second = Caches::default();
        assert!(restore(&mut second, &saved));
        let logs = second.transcripts.get_mut(&dir).unwrap();
        let (listed, all) = list_logs(&dir, ".jsonl", None, at + 1.0);
        logs.pass((listed, all), false, at + 1.0, &mut Budget::unlimited(), |_| {
            crate::claude::TranscriptLines::default()
        });
        let mut ids: Vec<&String> = logs.tails[&log.path].parser.records.keys().collect();
        ids.sort();
        assert_eq!(ids, vec!["x", "y", "z"], "nothing of the old log survives");
    }

    #[test]
    fn a_store_of_another_version_is_ignored_and_a_torn_one_keeps_its_whole_tails() {
        let mut caches = Caches::default();
        let mut out = Vec::new();
        let other = format!("{}\n", serde_json::json!({"version": VERSION + 1}));
        assert!(!read_all(&mut caches, other.as_bytes()));
        assert!(!read_all(&mut caches, &b"not json\n"[..]));
        assert!(!restore(&mut caches, "/nonexistent/opscope-store.json"));
        // Two tails written, the file cut off inside the second's records.
        for (i, path) in ["/x/a.jsonl", "/x/b.jsonl"].iter().enumerate() {
            let mut lines = crate::claude::TranscriptLines::default();
            for n in 0..3 {
                lines.records.insert(format!("{i}-{n}"), ("2026-10-20".into(), "m".into(), empty_tokens()));
            }
            let tail = Tail { stamp: (1, 2), file: (3, 4), offset: 2, head_len: 0, head_hash: 0, read: 0, widest: 0, parser: lines };
            caches.transcripts.entry("/x".into()).or_default().tails.insert(path.to_string(), tail);
        }
        write_all(&caches, &mut out).unwrap();
        let text = String::from_utf8(out).unwrap();
        // Inside the last record of the second tail, before the next set.
        let cut = &text[..text.find("{\"set\":[\"codex\"]}").unwrap() - 5];
        let mut back = Caches::default();
        assert!(read_all(&mut back, cut.as_bytes()));
        let held = &back.transcripts["/x"].tails;
        assert_eq!(held.len(), 1, "the torn tail is dropped, to be read again");
        assert_eq!(held.values().next().unwrap().parser.records.len(), 3);
        // Cut between two whole record lines: every line parses, but the
        // tail is still missing a record, so it is dropped too.
        let last = text[..text.find("{\"set\":[\"codex\"]}").unwrap()].trim_end_matches('\n');
        let between = &last[..=last.rfind('\n').unwrap()];
        let mut back = Caches::default();
        assert!(read_all(&mut back, between.as_bytes()));
        let held = &back.transcripts["/x"].tails;
        assert_eq!(held.len(), 1, "a tail short of a record is read again");
        assert_eq!(held.values().next().unwrap().parser.records.len(), 3);
        // Whole, everything comes back.
        let mut whole = Caches::default();
        assert!(read_all(&mut whole, text.as_bytes()));
        assert_eq!(whole.transcripts["/x"].tails.len(), 2);
    }

    #[test]
    fn progress_is_saved_after_a_read_on_its_cadence_or_when_a_first_read_ends() {
        let mut caches = Caches::default();
        let mut saver = Saver::new(1000.0);
        assert!(!saver.due(&mut caches, 1001.0), "nothing read, nothing to save");
        caches.rollouts.dirty = true;
        assert!(!saver.due(&mut caches, 1002.0), "read, but inside the cadence");
        assert!(saver.due(&mut caches, 1000.0 + SAVE_EVERY), "owed, and the cadence is up");
        assert!(!saver.due(&mut caches, 2000.0), "and not again until something is read");
    }
}
