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

//! Vendor readings shared between every pane on this machine.
//!
//! Each pane used to ask every endpoint on its own, so three panes asked
//! Claude three times per hold and Claude answered the third with a 429 -
//! which every pane then showed as "something else is polling the same
//! token". The something else was us. Now a reading lives in one SQLite
//! file, one pane fetches it when it goes stale, and the rest read it.
//! `lease.rs` decides who; this carries the decision out.
//!
//! **Small, and quiet on the disk.** One row per reading, replaced, never
//! appended; rows nobody has confirmed for a day are deleted; the file is
//! capped by SQLite itself, and a file that is somehow larger is emptied
//! where it stands. A file that is damaged or from another version is
//! deleted and started again rather than repaired - and only then: a file
//! another pane is busy writing is healthy, and deleting it would split the
//! panes across two files without either noticing.
//! Writes happen when a fetch claims or publishes and at no other time -
//! no heartbeat, no per-pane row. A pane that wrote nothing reads nothing
//! either: `PRAGMA data_version` says whether anyone else has written since
//! it last looked, and a row is read only when they have.
//!
//! **No fsync.** A rollback journal kept as one truncated file, and
//! `synchronous=OFF`: a crashed process cannot corrupt the file this way,
//! only a power cut can, and a damaged store is deleted and every reading
//! fetched again - which costs one request per vendor, the price of
//! starting a pane.
//!
//! **Readable by its owner only.** Readings are usage figures, but they are
//! nobody else's, and the keys of some carry a hash of a token. A reading
//! whose serialisation contains a secret this pane holds is not written at
//! all.

use std::collections::HashMap;
use std::time::Duration;

use rusqlite::{params, Connection, OptionalExtension};

use super::lease::{self, Claim, Lease};

/// Bumped only when the tables change in a way an older build cannot read.
/// A column that is added, nullable, never bumps it: older builds name
/// their columns and keep working. A store of another version is left
/// alone - panes on that build still have it open - and this pane fetches
/// on its own; a change that needs a bump takes a new file name instead.
pub const VERSION: i64 = 1;

/// The most SQLite will let the file grow to: 4096 pages of 4 KB.
const MAX_PAGES: i64 = 4096;
const PAGE: i64 = 4096;
/// Past this the writer vacuums, and if that does not bring it back under,
/// starts again. Half the cap, so it is acted on long before a write fails.
pub const SHRINK_AT: u64 = (MAX_PAGES * PAGE / 2) as u64;
/// A reading larger than this, serialised, is kept by the pane that fetched
/// it and not shared. Nothing read today comes near it.
pub const MAX_VALUE: usize = 2 * 1024 * 1024;
/// How long a write waits for another pane's transaction before giving up
/// for this pass. Every transaction here is a handful of rows.
const BUSY: Duration = Duration::from_millis(250);

/// How large one widget's store may grow, and the largest reading it
/// shares. agent-usage's figures are the default; a widget that shares
/// lists - pull requests, issues, runs - asks for more.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Caps {
    /// The most pages of `PAGE` bytes SQLite lets the file grow to.
    pub max_pages: i64,
    /// A reading larger than this, serialised, is kept by the pane that
    /// fetched it and not shared.
    pub max_value: usize,
    /// A reading nobody has published for this long is deleted. A widget
    /// that stores titles - pull requests, issues, runs - keeps them for
    /// hours, not the day agent-usage's figures are kept.
    pub forget_after: f64,
}

impl Default for Caps {
    fn default() -> Caps {
        Caps { max_pages: MAX_PAGES, max_value: MAX_VALUE, forget_after: lease::FORGET_AFTER }
    }
}

impl Caps {
    /// Past this the writer vacuums, and if that does not bring it back
    /// under, empties the file. Half the cap.
    pub fn shrink_at(&self) -> u64 {
        (self.max_pages * PAGE / 2) as u64
    }
}

/// What came of publishing a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    /// In the store for every pane.
    Written,
    /// A newer reading was already there, so this one was dropped.
    Older,
    /// Larger than the widget's cap: kept by this pane only.
    TooLarge,
    /// This pane's lease ran out and another pane holds it now: that
    /// pane's answer is the one shared, and this one stays here.
    TakenOver,
    /// It carried a token this pane holds: kept by this pane only.
    HeldSecret,
}

impl Publish {
    /// Why the reading stayed with this pane, when it did.
    pub fn unshared(self) -> Option<&'static str> {
        match self {
            Publish::TooLarge => Some("too large to share"),
            Publish::HeldSecret => Some("not shared: it carried a token"),
            Publish::TakenOver => Some("not shared: another pane took over"),
            Publish::Written | Publish::Older => None,
        }
    }
}

/// The store of the widget named `name`: one file per widget, under
/// [`crate::state_dir`], so widgets never share a file.
pub fn path(name: &str) -> String {
    crate::state_dir().join(format!("{name}.sqlite")).to_string_lossy().to_string()
}

/// One reading as it is stored.
#[derive(Clone, Debug, PartialEq)]
pub struct Shared {
    /// When the fetch finished - the reading's own time.
    pub taken_at: f64,
    /// How long it is good for: the TTL, or the backoff for a refusal.
    pub held: f64,
    /// The reading, or `None` for a refusal with no reason attached.
    pub value: Option<serde_json::Value>,
    /// Refusals in a row, which the backoff doubles on.
    pub fails: u32,
    /// The last good reading and when it was taken, for `kept`.
    pub good: Option<(f64, serde_json::Value)>,
    /// The process that fetched it.
    pub fetched_by: u32,
}

/// What came of asking for the lease.
#[derive(Debug, PartialEq)]
pub enum Got {
    Mine,
    Held(u32),
    Fresh(Shared),
}

pub struct Store {
    conn: Connection,
    path: String,
    caps: Caps,
    /// (device, inode) of the file when it was opened, so a file replaced
    /// under the same name - by a build that started it again - is noticed.
    file: (u64, u64),
    pub me: u32,
    /// `data_version` as of the start of this pass.
    version: i64,
    /// The version at which each key was last read.
    seen: HashMap<String, i64>,
    /// Rows read and transactions written, for the tests that hold the
    /// store to its budget.
    pub reads: u64,
    pub writes: u64,
    /// Why the store was started again, once, for the status line.
    pub notice: String,
}

fn remove(path: &str) {
    let _ = std::fs::remove_file(path);
    let _ = std::fs::remove_file(format!("{path}-journal"));
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

/// Why a store could not be opened, and whether that is the file's fault.
#[derive(Debug)]
struct OpenError {
    why: String,
    /// Damaged, not a database, or another version: delete it and start
    /// again. Anything else - another pane holding a lock past `BUSY`, a
    /// directory that cannot be written - leaves the file alone.
    unusable: bool,
}

impl OpenError {
    fn other(why: String) -> OpenError {
        OpenError { why, unusable: false }
    }

    fn unusable(why: String) -> OpenError {
        OpenError { why, unusable: true }
    }
}

/// Whether SQLite said the file itself is bad, as opposed to busy.
fn damaged(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt)
    )
}

fn db(e: rusqlite::Error) -> OpenError {
    OpenError { unusable: damaged(&e), why: e.to_string() }
}

/// Whether an error is another pane holding the file for longer than
/// `BUSY` - which passes, and is no reason to doubt the store.
pub fn busy(why: &str) -> bool {
    why.contains("database is locked") || why.contains("database table is locked")
}

/// How many times opening a busy store is tried before giving up for now.
const OPEN_TRIES: u32 = 3;

impl Store {
    /// Open the store, or start it again when the file cannot be used.
    ///
    /// Only a file SQLite calls damaged is deleted. One written by another
    /// version is left for the panes using it, and this pane goes on alone. A busy one is tried again a few times and then left for
    /// a later pass; the panes already using it carry on with it.
    pub fn open(path: &str) -> Result<Store, String> {
        Store::open_with(path, Caps::default())
    }

    /// [`open`](Self::open) with the size limits of this widget.
    pub fn open_with(path: &str, caps: Caps) -> Result<Store, String> {
        let mut tries = 0;
        let first = loop {
            match Store::open_once(path, caps) {
                Ok(s) => return Ok(s),
                Err(e) if e.unusable => break e.why,
                Err(e) => {
                    tries += 1;
                    if tries >= OPEN_TRIES || !busy(&e.why) {
                        return Err(e.why);
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
            }
        };
        remove(path);
        let mut s = Store::open_once(path, caps).map_err(|e| e.why)?;
        s.notice = format!("shared store started again: {first}");
        Ok(s)
    }

    fn open_once(path: &str, caps: Caps) -> Result<Store, OpenError> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(dir)
                .map_err(|e| OpenError::other(format!("cannot make {}: {e}", dir.display())))?;
        }
        // Made before SQLite opens it, so it is 0600 whatever the umask; the
        // journal SQLite makes beside it takes the same mode.
        match std::fs::OpenOptions::new().write(true).create_new(true).mode(0o600).open(path) {
            Ok(_) => {}
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(OpenError::other(format!("cannot create it: {e}"))),
        }
        let conn = Connection::open(path).map_err(db)?;
        conn.busy_timeout(BUSY).map_err(db)?;
        conn.pragma_update(None, "page_size", PAGE).map_err(db)?;
        let _: String = conn
            .query_row("PRAGMA journal_mode=TRUNCATE", [], |r| r.get(0))
            .map_err(db)?;
        conn.pragma_update(None, "synchronous", "OFF").map_err(db)?;
        let _: i64 = conn
            .query_row(&format!("PRAGMA max_page_count={}", caps.max_pages), [], |r| r.get(0))
            .map_err(db)?;
        let ok: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0)).map_err(db)?;
        if ok != "ok" {
            return Err(OpenError::unusable(format!("it was damaged ({ok})")));
        }
        conn.execute_batch(
            "BEGIN IMMEDIATE;
             CREATE TABLE IF NOT EXISTS meta(k TEXT PRIMARY KEY, v);
             CREATE TABLE IF NOT EXISTS readings(
                 key TEXT PRIMARY KEY, taken_at REAL NOT NULL, held REAL NOT NULL,
                 value TEXT, fails INTEGER NOT NULL, good_value TEXT, good_at REAL,
                 fetched_by INTEGER NOT NULL);
             CREATE TABLE IF NOT EXISTS leases(
                 key TEXT PRIMARY KEY, holder INTEGER NOT NULL, expires_at REAL NOT NULL);
             CREATE TABLE IF NOT EXISTS checks(key TEXT PRIMARY KEY, at REAL NOT NULL, by INTEGER);
             COMMIT;",
        )
        .map_err(|e| {
            // A failed batch can leave its BEGIN open on this connection.
            let _ = conn.execute_batch("ROLLBACK");
            db(e)
        })?;
        // `by` arrived with the check stamp. A store this branch opened
        // before that column existed still has the table.
        ensure_checks_by(&conn)?;
        conn.execute("INSERT OR IGNORE INTO meta(k, v) VALUES ('version', ?1)", [VERSION])
            .map_err(db)?;
        let v: i64 = conn
            .query_row("SELECT v FROM meta WHERE k = 'version'", [], |r| r.get(0))
            .map_err(db)?;
        if v != VERSION {
            return Err(OpenError::other(format!(
                "the shared store was written by another version of opscope ({v}, this is {VERSION})"
            )));
        }
        let file = identity(path).map_err(OpenError::other)?;
        let mut store = Store {
            conn,
            path: path.to_string(),
            caps,
            file,
            me: std::process::id(),
            version: 0,
            seen: HashMap::new(),
            reads: 0,
            writes: 0,
            notice: String::new(),
        };
        store.begin_pass().map_err(OpenError::other)?;
        let size = store.size();
        if size > caps.shrink_at() {
            store.keep_small().map_err(OpenError::other)?;
            if store.notice.is_empty() {
                store.notice = format!("shared store compacted: it had grown to {} KB", size / 1024);
            }
        }
        Ok(store)
    }

    /// Note whether anyone has written since the last pass. One page of
    /// the file's header, which the OS already has in memory.
    pub fn begin_pass(&mut self) -> Result<(), String> {
        self.version = self
            .conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .map_err(sql)?;
        Ok(())
    }

    /// The stored reading under `key`, when another pane may have changed
    /// it since this pane last looked; `None` when nothing could have.
    pub fn load(&mut self, key: &str) -> Result<Option<Shared>, String> {
        if self.seen.get(key) == Some(&self.version) {
            return Ok(None);
        }
        self.reads += 1;
        let found = row(&self.conn, key).map_err(sql)?;
        // Marked read only once the row was: a busy read is asked again
        // at this version, or a gate written before it would stay unseen
        // until somebody wrote something else.
        self.seen.insert(key.to_string(), self.version);
        Ok(found)
    }

    /// Ask to be the pane that fetches `key`. `known` is when the reading
    /// this pane holds was taken.
    pub fn claim(&mut self, key: &str, known: Option<f64>, now: f64) -> Result<Got, String> {
        self.claim_for(key, known, now, lease::LEASE)
    }

    /// [`claim`](Self::claim) with a lease of `lease_for` seconds, at most
    /// `MAX_LEASE`: long enough for one request of this widget's fetch.
    pub fn claim_for(&mut self, key: &str, known: Option<f64>, now: f64, lease_for: f64) -> Result<Got, String> {
        let lease_for = lease_for.clamp(1.0, lease::MAX_LEASE);
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(sql)?;
        let stored = stamp(&tx, key).map_err(sql)?;
        let held: Option<Lease> = tx
            .query_row("SELECT holder, expires_at FROM leases WHERE key = ?1", [key], |r| {
                Ok(Lease { holder: r.get(0)?, expires_at: r.get(1)? })
            })
            .optional()
            .map_err(sql)?;
        let got = match lease::claim(stored, known, held, self.me, now) {
            Claim::Mine => {
                tx.execute(
                    "INSERT INTO leases(key, holder, expires_at) VALUES (?1, ?2, ?3)
                     ON CONFLICT(key) DO UPDATE SET holder = ?2, expires_at = ?3",
                    params![key, self.me, now + lease_for],
                )
                .map_err(sql)?;
                self.writes += 1;
                Got::Mine
            }
            Claim::Held(by) => Got::Held(by),
            Claim::Fresh => {
                self.reads += 1;
                match row(&tx, key).map_err(sql)? {
                    Some(s) => Got::Fresh(s),
                    None => Got::Held(0),
                }
            }
        };
        tx.commit().map_err(sql)?;
        Ok(got)
    }

    /// Hold a lease this pane has for another `lease_for` seconds, before the
    /// next request of a long fetch. `false` when it is no longer this
    /// pane's: it ran out and another pane took it.
    pub fn renew(&mut self, key: &str, now: f64, lease_for: f64) -> Result<bool, String> {
        let lease_for = lease_for.clamp(1.0, lease::MAX_LEASE);
        let n = self
            .conn
            .execute(
                "UPDATE leases SET expires_at = ?3 WHERE key = ?1 AND holder = ?2",
                params![key, self.me, now + lease_for],
            )
            .map_err(sql)?;
        if n > 0 {
            self.writes += 1;
        }
        Ok(n > 0)
    }

    /// Extend every lease this pane still holds, and say whether `key` is
    /// one of them. One statement covers a pass that claimed many readings
    /// and is still fetching the later ones: their leases would otherwise
    /// run out while the early answers sit unpublished (OPS-298).
    pub fn renew_all(&mut self, now: f64, lease_for: f64, key: &str) -> Result<bool, String> {
        let lease_for = lease_for.clamp(1.0, lease::MAX_LEASE);
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(sql)?;
        tx.execute(
            "UPDATE leases SET expires_at = ?3 WHERE holder = ?1 AND expires_at > ?2",
            params![self.me, now, now + lease_for],
        )
        .map_err(sql)?;
        let mine: Option<i64> = tx
            .query_row(
                "SELECT 1 FROM leases WHERE key = ?1 AND holder = ?2 AND expires_at > ?3",
                params![key, self.me, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        tx.commit().map_err(sql)?;
        self.writes += 1;
        Ok(mine.is_some())
    }

    /// Who holds `key`'s lease now, if anyone does and it has not run out.
    pub fn holder(&self, key: &str, now: f64) -> Result<Option<u32>, String> {
        self.conn
            .query_row(
                "SELECT holder FROM leases WHERE key = ?1 AND expires_at > ?2",
                params![key, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)
    }

    /// Whether the file at this store's path is no longer the one it
    /// opened: another build deleted it and started again. The pane should
    /// open the store again, or it shares with nobody.
    pub fn replaced(&self) -> bool {
        !identity(&self.path).is_ok_and(|f| f == self.file)
    }

    /// Claim several readings in one transaction: a pass that fetches many
    /// pays for one commit, not one each (OPS-298).
    pub fn claim_many(
        &mut self,
        asks: &[(&str, Option<f64>)],
        now: f64,
        lease_for: f64,
    ) -> Result<Vec<Got>, String> {
        let lease_for = lease_for.clamp(1.0, lease::MAX_LEASE);
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(sql)?;
        let mut out = Vec::with_capacity(asks.len());
        for (key, known) in asks {
            let stored = stamp(&tx, key).map_err(sql)?;
            let held: Option<Lease> = tx
                .query_row("SELECT holder, expires_at FROM leases WHERE key = ?1", [key], |r| {
                    Ok(Lease { holder: r.get(0)?, expires_at: r.get(1)? })
                })
                .optional()
                .map_err(sql)?;
            out.push(match lease::claim(stored, *known, held, self.me, now) {
                Claim::Mine => {
                    tx.execute(
                        "INSERT INTO leases(key, holder, expires_at) VALUES (?1, ?2, ?3)
                         ON CONFLICT(key) DO UPDATE SET holder = ?2, expires_at = ?3",
                        params![key, self.me, now + lease_for],
                    )
                    .map_err(sql)?;
                    Got::Mine
                }
                Claim::Held(by) => Got::Held(by),
                Claim::Fresh => match row(&tx, key).map_err(sql)? {
                    Some(s) => Got::Fresh(s),
                    None => Got::Held(0),
                },
            });
        }
        tx.commit().map_err(sql)?;
        self.writes += 1;
        Ok(out)
    }

    /// Put the readings a pass fetched in the store in one transaction, and
    /// let their leases go. One per item, in order, saying what came of it.
    ///
    /// `claimed_at` is when this pane claimed the leases, when it did. A
    /// reading another pane published after that is left alone, even if
    /// this pane's lease has since run out and this answer is stamped
    /// later: the other pane fetched after this one started.
    pub fn publish_many(
        &mut self,
        items: &[(&str, Shared)],
        secrets: &[String],
        now: f64,
        claimed_at: Option<f64>,
    ) -> Result<Vec<Publish>, String> {
        self.in_immediate(|store| {
            let mut out = Vec::with_capacity(items.len());
            for (key, shared) in items {
                if store.superseded(key, claimed_at, now)? {
                    // The lease, if this pane still holds it, must not keep
                    // the successor from the next pass.
                    store
                        .conn
                        .execute(
                            "DELETE FROM leases WHERE key = ?1 AND holder = ?2",
                            params![key, store.me],
                        )
                        .map_err(sql)?;
                    out.push(Publish::TakenOver);
                    continue;
                }
                out.push(store.write_one(key, shared, secrets, now)?);
            }
            // Only the keys just answered. A lease this pane still holds
            // for a reading it has not answered yet stays until `release`.
            store
                .conn
                .execute("DELETE FROM leases WHERE expires_at < ?1", [now - 2.0 * lease::MAX_LEASE])
                .map_err(sql)?;
            store
                .conn
                .execute("DELETE FROM checks WHERE at < ?1", [now - store.caps.forget_after])
                .map_err(sql)?;
            Ok(out)
        })
        .map(|out| {
            let _ = self.keep_small();
            out
        })
    }

    /// Run `f` inside one `BEGIN IMMEDIATE`, rolling back if it fails.
    fn in_immediate<T>(&mut self, f: impl FnOnce(&Store) -> Result<T, String>) -> Result<T, String> {
        self.conn.execute_batch("BEGIN IMMEDIATE").map_err(sql)?;
        let out = match f(self) {
            Ok(v) => v,
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        };
        match self.conn.execute_batch("COMMIT") {
            Ok(()) => {
                self.writes += 1;
                Ok(out)
            }
            Err(e) => {
                let _ = self.conn.execute_batch("ROLLBACK");
                Err(sql(e))
            }
        }
    }

    /// Write one reading inside a transaction the caller has begun; the
    /// body of [`publish`](Self::publish) and of
    /// [`publish_many`](Self::publish_many).
    fn write_one(&self, key: &str, s: &Shared, secrets: &[String], now: f64) -> Result<Publish, String> {
        let caps = self.caps;
        let me = self.me;
        let value = s.value.as_ref().map(|v| v.to_string());
        let good = s
            .good
            .as_ref()
            .filter(|(_, g)| s.value.as_ref() != Some(g))
            .map(|(_, v)| v.to_string());
        let good_at = s.good.as_ref().map(|(at, _)| *at);
        if let Some(why) = unshareable(&[value.as_deref(), good.as_deref()], secrets, caps.max_value) {
            let _ = self.conn.execute(
                "DELETE FROM leases WHERE key = ?1 AND holder = ?2",
                params![key, me],
            );
            return Ok(why);
        }
        let stored: Option<(f64, Option<String>, Option<String>, u32, f64, Option<f64>)> = self.conn
            .query_row(
                "SELECT taken_at, value, good_value, fetched_by, held,
                        (SELECT at FROM checks WHERE key = ?1)
                 FROM readings WHERE key = ?1",
                [key],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?)),
            )
            .optional()
            .map_err(sql)?;
        // The check is the later fetch of this same text. An answer older
        // than that check is older than the reading, whatever the row's
        // own time still says.
        let effective = stored.as_ref().map(|r| r.5.map_or(r.0, |c| c.max(r.0)));
        let held_same = stored.as_ref().is_none_or(|r| r.4 == s.held);
        let other: Option<u32> = self.conn
            .query_row(
                "SELECT holder FROM leases WHERE key = ?1 AND holder != ?2 AND expires_at > ?3",
                params![key, self.me, now],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        if other.is_some() {
            return Ok(Publish::TakenOver);
        }
        let wrote = lease::supersedes(effective, s.taken_at, now);
        if wrote {
            let same = stored.as_ref().is_some_and(|r| r.1 == value && r.2 == good);
            if same && held_same {
                // The same answer again: a time, not the reading. Moving the
                // row's own time rewrites its record, and for a large reading
                // that was the whole cost of a quiet pass (OPS-298).
                note_check(&self.conn, key, s.taken_at, me).map_err(sql)?;
            } else if same {
                self.conn.execute(
                    "UPDATE readings SET taken_at = ?2, held = ?3, fails = ?4, good_at = ?5,
                     fetched_by = ?6 WHERE key = ?1",
                    params![key, s.taken_at, s.held, s.fails, good_at, self.me],
                )
                .map_err(sql)?;
            } else {
                // The old check described the previous text.
                self.conn.execute("DELETE FROM checks WHERE key = ?1", [key]).map_err(sql)?;
                self.conn.execute(
                    "INSERT INTO readings(key, taken_at, held, value, fails, good_value, good_at, fetched_by)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
                     ON CONFLICT(key) DO UPDATE SET taken_at = ?2, held = ?3, value = ?4, fails = ?5,
                     good_value = ?6, good_at = ?7, fetched_by = ?8",
                    params![key, s.taken_at, s.held, value, s.fails, good, good_at, self.me],
                )
                .map_err(sql)?;
            }
        }
        self.conn.execute("DELETE FROM leases WHERE key = ?1 AND holder = ?2", params![key, self.me])
            .map_err(sql)?;
        // Age is the later of the row and its check: a quiet pass moves
        // only the check, and that reading is still being confirmed.
        self.conn.execute(
            "DELETE FROM readings WHERE rowid IN (
                 SELECT r.rowid FROM readings r
                 LEFT JOIN checks c ON c.key = r.key
                 WHERE max(r.taken_at, coalesce(c.at, r.taken_at)) < ?1
                   AND max(r.taken_at, coalesce(c.at, r.taken_at)) + r.held <= ?2
             )",
            params![now - caps.forget_after, now],
        )
        .map_err(sql)?;
        Ok(if wrote { Publish::Written } else { Publish::Older })
        }

    /// Put a reading this pane fetched in the store and let its lease go.
    ///
    /// Not written when a newer one is already there, when it is larger
    /// than the widget's cap, or when it contains any of `secrets` - the
    /// lease is given back either way. Returns what came of it.
    pub fn publish(&mut self, key: &str, s: &Shared, secrets: &[String], now: f64) -> Result<Publish, String> {
        self.publish_since(key, s, secrets, now, None)
    }

    /// [`publish`](Self::publish) for a fetch this pane claimed at
    /// `claimed_at`: a reading another pane published since then - it took
    /// the lease over once this one ran out - is not written over.
    pub fn publish_since(
        &mut self,
        key: &str,
        s: &Shared,
        secrets: &[String],
        now: f64,
        claimed_at: Option<f64>,
    ) -> Result<Publish, String> {
        let out = self.in_immediate(|store| {
            // A fetch that outlived its lease does not write over the pane
            // that took it over, which is fetching the same reading now.
            let other: Option<u32> = store
                .conn
                .query_row(
                    "SELECT holder FROM leases WHERE key = ?1 AND holder != ?2 AND expires_at > ?3",
                    params![key, store.me, now],
                    |r| r.get(0),
                )
            .optional()
            .map_err(sql)?;
            // A row dated ahead of the clock is no successor's: `supersedes`
            // replaces it below. One published by another pane after this
            // claim is that pane's, and stays.
            if other.is_some() || store.superseded(key, claimed_at, now)? {
                return Ok(Publish::TakenOver);
            }
            store.write_one(key, s, secrets, now)
        })?;
        self.keep_small()?;
        Ok(out)
    }

    /// Give a lease back without publishing, so the next pane need not
    /// wait for it to run out.
    pub fn release(&mut self, key: &str) -> Result<(), String> {
        let n = self
            .conn
            .execute("DELETE FROM leases WHERE key = ?1 AND holder = ?2", params![key, self.me])
            .map_err(sql)?;
        if n > 0 {
            self.writes += 1;
        }
        Ok(())
    }

    /// Replace the value of a reading this pane just published with the
    /// reason it was refused, the way `remember_refusal` does in memory.
    ///
    /// Held to `publish`'s rules: a value that would carry any of `secrets`,
    /// or is larger than `MAX_VALUE`, is not written and stays with this
    /// pane. Returns whether it was written.
    pub fn annotate(
        &mut self,
        key: &str,
        taken_at: f64,
        value: &serde_json::Value,
        secrets: &[String],
    ) -> Result<bool, String> {
        let text = value.to_string();
        if unshareable(&[Some(&text)], secrets, self.caps.max_value).is_some() {
            return Ok(false);
        }
        // A row whose good value is a time alone - it was the `value` when
        // it was published - keeps the old value as its good reading before
        // the new one is written over it.
        // A quiet republish leaves `taken_at` where it was and records the
        // new time in `checks`. Match either, and the pane that checked.
        let n = self
            .conn
            .execute(
                "UPDATE readings
                 SET good_at = CASE
                       WHEN good_value IS NULL THEN max(
                         coalesce(good_at, taken_at),
                         coalesce((SELECT at FROM checks WHERE key = readings.key), coalesce(good_at, taken_at))
                       )
                       ELSE good_at
                     END,
                     good_value = coalesce(good_value, value),
                     value = ?3, taken_at = ?2, fetched_by = ?4
                 WHERE key = ?1 AND (
                     (
                       taken_at = ?2 AND fetched_by = ?4
                       AND NOT EXISTS (
                         SELECT 1 FROM checks c
                         WHERE c.key = readings.key AND c.at > readings.taken_at
                       )
                     )
                     OR EXISTS (
                       SELECT 1 FROM checks c
                       WHERE c.key = readings.key AND c.at = ?2 AND c.by = ?4
                     )
                 )",
                params![key, taken_at, text, self.me],
            )
            .map_err(sql)?;
        if n > 0 {
            // The value changed, so the check described the previous text.
            self.conn.execute("DELETE FROM checks WHERE key = ?1", [key]).map_err(sql)?;
            self.writes += 1;
        }
        Ok(n > 0)
    }

    /// Vacuum a file that has grown past `SHRINK_AT`; empty it if that was
    /// not enough. Never let it grow.
    ///
    /// Emptied where it stands rather than deleted: the other panes hold it
    /// open, and a new file under the same name is one they would never see
    /// - each would go on with its own, and ask every vendor itself again.
    fn keep_small(&mut self) -> Result<(), String> {
        if self.size() <= self.caps.shrink_at() {
            return Ok(());
        }
        self.conn.execute_batch("VACUUM").map_err(sql)?;
        if self.size() <= self.caps.shrink_at() {
            return Ok(());
        }
        // A gate still in force stays: it is a few bytes, and losing it
        // would let every pane ask a service that said to wait.
        let tx = self.conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate).map_err(sql)?;
        tx.execute(
            "DELETE FROM readings WHERE NOT (key LIKE 'gate/%' AND taken_at + held > ?1)",
            [crate::now()],
        )
        .map_err(sql)?;
        tx.execute("DELETE FROM leases", []).map_err(sql)?;
        tx.execute("DELETE FROM checks", []).map_err(sql)?;
        tx.commit().map_err(sql)?;
        self.conn.execute_batch("VACUUM").map_err(sql)?;
        self.writes += 1;
        self.seen.clear();
        self.notice = "shared store emptied: it had grown past its limit".into();
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn stored_text_for_tests(&self, key: &str) -> String {
        self.conn
            .query_row("SELECT coalesce(value, '') || '|' || coalesce(good_value, '') FROM readings WHERE key = ?1", [key], |r| r.get(0))
            .unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn lease_count_for_tests(&self, key: &str) -> i64 {
        self.conn
            .query_row("SELECT count(*) FROM leases WHERE key = ?1", [key], |r| r.get(0))
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(crate) fn path_for_tests(&self) -> String {
        self.path.clone()
    }

    pub fn size(&self) -> u64 {
        std::fs::metadata(&self.path).map(|m| m.len()).unwrap_or(0)
    }
}

/// Whether serialised readings are too large to share, or carry a token
/// this pane holds. Tokens shorter than eight bytes are not looked for: they
/// would match ordinary text and refuse readings that carry no secret.
fn unshareable(texts: &[Option<&str>], secrets: &[String], max_value: usize) -> Option<Publish> {
    let size: usize = texts.iter().flatten().map(|t| t.len()).sum();
    if size > max_value {
        return Some(Publish::TooLarge);
    }
    secrets
        .iter()
        .filter(|t| t.len() >= 8)
        .any(|t| {
            // The texts are JSON, where a `"` or `\` in a token is escaped.
            let json = serde_json::to_string(t).unwrap_or_default();
            let escaped = json.get(1..json.len().saturating_sub(1)).unwrap_or("");
            texts.iter().flatten().any(|v| v.contains(t.as_str()) || (!escaped.is_empty() && v.contains(escaped)))
        })
        .then_some(Publish::HeldSecret)
}

/// (device, inode) of the file at `path`.
fn identity(path: &str) -> Result<(u64, u64), String> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path)
        .map(|m| (m.dev(), m.ino()))
        .map_err(|e| format!("cannot read the store's file: {e}"))
}

/// When `key` was last fetched - the later of the reading's own time and
/// its check stamp - and how long it is held. `coalesce` rather than a bare
/// `max` with the check: scalar `max` is NULL when any argument is.
fn stamp(conn: &Connection, key: &str) -> rusqlite::Result<Option<(f64, f64)>> {
    conn.query_row(
        "SELECT max(r.taken_at, coalesce(c.at, r.taken_at)), r.held FROM readings r
         LEFT JOIN checks c ON c.key = r.key WHERE r.key = ?1",
        [key],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()
}

/// Note that `key` was fetched at `at` by `by`, without rewriting the reading.
fn note_check(conn: &rusqlite::Connection, key: &str, at: f64, by: u32) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO checks(key, at, by) VALUES (?1, ?2, ?3)
         ON CONFLICT(key) DO UPDATE SET at = ?2, by = ?3",
        params![key, at, by],
    )?;
    Ok(())
}

/// Another pane published `key` after this pane claimed it at `since`.
fn superseded_at(by: u32, at: f64, me: u32, since: f64, now: f64) -> bool {
    by != me && at >= since && at <= now + lease::SKEW
}

impl Store {
    /// A reading another pane published after this pane claimed `key`.
    fn superseded(&self, key: &str, claimed_at: Option<f64>, now: f64) -> Result<bool, String> {
        let Some(since) = claimed_at else {
            return Ok(false);
        };
        // A quiet confirmation lives in `checks`, not on the row. That is
        // a publication too: a fetch this pane started before it does not
        // write over it.
        self.conn
            .query_row(
                "SELECT r.taken_at, r.fetched_by, c.at, c.by
                 FROM readings r LEFT JOIN checks c ON c.key = r.key WHERE r.key = ?1",
                [key],
                |r| {
                    Ok((
                        r.get::<_, f64>(0)?,
                        r.get::<_, u32>(1)?,
                        r.get::<_, Option<f64>>(2)?,
                        r.get::<_, Option<u32>>(3)?,
                    ))
                },
            )
            .optional()
            .map(|row| {
                row.is_some_and(|(taken_at, fetched_by, checked, checked_by)| {
                    let (at, by) = match checked {
                        Some(c) if c >= taken_at => (c, checked_by.unwrap_or(fetched_by)),
                        _ => (taken_at, fetched_by),
                    };
                    superseded_at(by, at, self.me, since, now)
                })
            })
            .map_err(sql)
    }
}

fn row(conn: &Connection, key: &str) -> rusqlite::Result<Option<Shared>> {
    conn.query_row(
        "SELECT r.taken_at, r.held, r.value, r.fails, r.good_value, r.good_at, r.fetched_by, c.at, c.by
         FROM readings r LEFT JOIN checks c ON c.key = r.key WHERE r.key = ?1",
        [key],
        |r| {
            let value: Option<String> = r.get(2)?;
            let good_text: Option<String> = r.get(4)?;
            let good_at: Option<f64> = r.get(5)?;
            let value: Option<serde_json::Value> = value.and_then(|v| serde_json::from_str(&v).ok());
            let good_value: Option<serde_json::Value> = good_text.and_then(|v| serde_json::from_str(&v).ok());
            let taken_at: f64 = r.get(0)?;
            let checked: Option<f64> = r.get(7)?;
            let checked_by: Option<u32> = r.get(8)?;
            let fetched_by: u32 = r.get(6)?;
            // A check of the value itself is a later confirmation that it
            // is still the good reading. A separate good copy is the
            // previous success, and a check of a refusal does not refresh it.
            let good_time = good_at.map(|g| {
                if good_value.is_none() {
                    checked.map_or(g, |c| c.max(g))
                } else {
                    g
                }
            });
            let by = if checked.is_some_and(|c| c >= taken_at) {
                checked_by.unwrap_or(fetched_by)
            } else {
                fetched_by
            };
            Ok(Shared {
                taken_at: checked.map_or(taken_at, |c| c.max(taken_at)),
                held: r.get(1)?,
                // A time without a good value of its own: it was the value
                // itself when it was published (see `publish_since`).
                good: match (good_time, good_value) {
                    (Some(at), None) => value.as_ref().map(|v| (at, v.clone())),
                    (Some(at), Some(v)) => Some((at, v)),
                    _ => None,
                },
                value,
                fails: r.get(3)?,
                fetched_by: by,
            })
        },
    )
    .optional()
}

/// `checks.by` on a store created before that column existed.
fn ensure_checks_by(conn: &Connection) -> Result<(), OpenError> {
    let has_by = {
        let mut stmt = conn.prepare("PRAGMA table_info(checks)").map_err(db)?;
        let mut rows = stmt.query([]).map_err(db)?;
        let mut found = false;
        while let Some(row) = rows.next().map_err(db)? {
            let name: String = row.get(1).map_err(db)?;
            found |= name == "by";
        }
        found
    };
    if !has_by {
        conn.execute("ALTER TABLE checks ADD COLUMN by INTEGER", []).map_err(db)?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests_support {
    /// A fresh store path in a directory of its own.
    pub fn temp(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "opscope-readings-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("w.sqlite").to_string_lossy().to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::tests_support::temp as temp_store;
    use super::*;

    fn temp(name: &str) -> String {
        let dir = std::env::temp_dir().join(format!(
            "opscope-sharing-{}-{}-{}",
            name,
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos())
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("agent-usage.sqlite").to_string_lossy().to_string()
    }

    #[test]
    fn a_widgets_store_is_its_own_file_in_the_state_directory() {
        let p = path("agent-usage");
        assert!(p.ends_with("/agent-usage.sqlite"), "{p}");
        assert_eq!(std::path::Path::new(&p).parent(), Some(crate::state_dir().as_path()));
        assert_ne!(path("agent-usage"), path("github-actions"));
    }

    /// The file agent-usage wrote before the store moved to core, built
    /// with the statements that code ran. An agent-usage from before the
    /// move and one from after share it while panes of both are open, so
    /// the new code must open it as it is - not start it again - and read
    /// what the old one published.
    #[test]
    fn a_store_written_before_the_move_is_opened_and_read_as_it_is() {
        let p = temp("before-the-move");
        {
            let conn = Connection::open(&p).unwrap();
            conn.execute_batch(
                "CREATE TABLE meta(k TEXT PRIMARY KEY, v);
                 CREATE TABLE readings(
                     key TEXT PRIMARY KEY, taken_at REAL NOT NULL, held REAL NOT NULL,
                     value TEXT, fails INTEGER NOT NULL, good_value TEXT, good_at REAL,
                     fetched_by INTEGER NOT NULL);
                 CREATE TABLE leases(
                     key TEXT PRIMARY KEY, holder INTEGER NOT NULL, expires_at REAL NOT NULL);
                 INSERT INTO meta(k, v) VALUES ('version', 1);",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO readings VALUES ('claude:/home/u/.claude:0123456789abcdef', ?1, 120.0,
                 '{\"five_hour\":{\"utilization\":12}}', 0, '{\"five_hour\":{\"utilization\":12}}', ?1, 4242)",
                [crate::now() - 10.0],
            )
            .unwrap();
        }
        let mut s = Store::open(&p).unwrap();
        assert_eq!(s.notice, "", "the old file was started again");
        let got = s.load("claude:/home/u/.claude:0123456789abcdef").unwrap().unwrap();
        assert_eq!(got.fetched_by, 4242);
        assert_eq!(got.value, Some(serde_json::json!({"five_hour": {"utilization": 12}})));
        assert_eq!(
            s.claim("claude:/home/u/.claude:0123456789abcdef", Some(got.taken_at), crate::now()).unwrap(),
            Got::Mine,
            "the same reading this pane holds is not news, so a claim on it fetches"
        );
    }

    fn reading(at: f64, v: serde_json::Value) -> Shared {
        Shared {
            taken_at: at,
            held: 300.0,
            value: Some(v.clone()),
            fails: 0,
            good: Some((at, v)),
            fetched_by: 0,
        }
    }

    const NOW: f64 = 2_000_000_000.0;

    #[test]
    fn the_file_is_its_owners_alone() {
        use std::os::unix::fs::PermissionsExt;
        let path = temp("mode");
        Store::open(&path).unwrap();
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn six_panes_crossing_a_ttl_together_make_one_claim() {
        let path = temp("race");
        Store::open(&path).unwrap();
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(6));
        let handles: Vec<_> = (0..6)
            .map(|i| {
                let path = path.clone();
                let barrier = barrier.clone();
                std::thread::spawn(move || {
                    let mut s = Store::open(&path).unwrap();
                    s.me = 1000 + i;
                    barrier.wait();
                    // A busy store this pass is a pass that did not claim.
                    s.claim("claude:/home", None, NOW).unwrap_or(Got::Held(0))
                })
            })
            .collect();
        let got: Vec<Got> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        assert_eq!(got.iter().filter(|g| **g == Got::Mine).count(), 1, "{got:?}");
    }

    #[test]
    fn the_others_see_what_was_published_and_do_not_ask() {
        let path = temp("follow");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        assert_eq!(a.claim("k", None, NOW).unwrap(), Got::Mine);
        assert_eq!(b.claim("k", None, NOW).unwrap(), Got::Held(a.me));
        assert_eq!(a.publish("k", &reading(NOW, serde_json::json!({"u": 1})), &[], NOW).unwrap(), Publish::Written);
        match b.claim("k", None, NOW + 1.0).unwrap() {
            Got::Fresh(s) => {
                assert_eq!(s.value, Some(serde_json::json!({"u": 1})));
                assert_eq!(s.fetched_by, a.me);
            }
            other => panic!("expected the published reading, got {other:?}"),
        }
    }

    #[test]
    fn a_late_older_publish_is_discarded() {
        let path = temp("late");
        let mut a = Store::open(&path).unwrap();
        assert_eq!(a.publish("k", &reading(NOW, serde_json::json!(2)), &[], NOW).unwrap(), Publish::Written);
        assert_eq!(a.publish("k", &reading(NOW - 60.0, serde_json::json!(1)), &[], NOW).unwrap(), Publish::Older);
        let mut b = Store::open(&path).unwrap();
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(serde_json::json!(2)));
    }

    #[test]
    fn nothing_written_means_nothing_read() {
        let path = temp("quiet");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        a.publish("k", &reading(NOW, serde_json::json!(1)), &[], NOW).unwrap();
        b.begin_pass().unwrap();
        assert!(b.load("k").unwrap().is_some());
        let reads = b.reads;
        for _ in 0..100 {
            b.begin_pass().unwrap();
            assert!(b.load("k").unwrap().is_none());
        }
        assert_eq!(b.reads, reads, "a quiet store must cost no row reads");
        a.publish("k", &reading(NOW + 1.0, serde_json::json!(2)), &[], NOW).unwrap();
        b.begin_pass().unwrap();
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(serde_json::json!(2)));
    }

    #[test]
    fn a_damaged_file_is_started_again() {
        let path = temp("garbage");
        std::fs::write(&path, b"this is not a database, and never was one").unwrap();
        let s = Store::open(&path).unwrap();
        assert!(s.notice.contains("started again"), "{}", s.notice);
    }

    #[test]
    fn a_store_from_another_version_is_left_to_the_panes_using_it() {
        // Panes on the build that wrote it still have it open. Deleting it
        // would split them from it without either side noticing, and the
        // next of them to start would delete this one's in turn.
        let path = temp("version");
        {
            let s = Store::open(&path).unwrap();
            s.conn.execute("UPDATE meta SET v = 99 WHERE k = 'version'", []).unwrap();
            s.conn
                .execute(
                    "INSERT INTO readings VALUES ('theirs', 1.0, 1.0, '1', 0, NULL, NULL, 7)",
                    [],
                )
                .unwrap();
        }
        let why = Store::open(&path).err().expect("opened another version's store");
        assert!(why.contains("another version"), "{why}");
        let conn = Connection::open(&path).unwrap();
        let kept: i64 = conn.query_row("SELECT count(*) FROM readings", [], |r| r.get(0)).unwrap();
        assert_eq!(kept, 1, "the other version's readings were deleted");
    }

    #[test]
    fn a_store_replaced_under_its_name_is_noticed() {
        let path = temp("replaced");
        let s = Store::open(&path).unwrap();
        assert!(!s.replaced());
        remove(&path);
        let _other = Store::open(&path).unwrap();
        assert!(s.replaced());
    }

    #[test]
    fn a_long_fetch_renews_its_lease_and_another_pane_waits_for_it() {
        let path = temp("renew");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        a.me = 1;
        b.me = 2;
        assert_eq!(a.claim_for("k", None, NOW, 20.0).unwrap(), Got::Mine);
        assert_eq!(b.holder("k", NOW + 1.0).unwrap(), Some(1));
        // Twenty seconds in, the first request is done; the next renews.
        assert!(a.renew("k", NOW + 19.0, 20.0).unwrap());
        b.begin_pass().unwrap();
        assert_eq!(b.claim_for("k", None, NOW + 30.0, 20.0).unwrap(), Got::Held(1));
        // A pane that died stops renewing, and the lease runs out after one
        // request's length, not after the whole pass would have.
        assert_eq!(b.claim_for("k", None, NOW + 40.0, 20.0).unwrap(), Got::Mine);
        assert!(!a.renew("k", NOW + 41.0, 20.0).unwrap(), "renewed a lease another pane took");
        // No claim asks for longer than MAX_LEASE.
        assert_eq!(a.claim_for("j", None, NOW, 1e9).unwrap(), Got::Mine);
        let until: f64 = a
            .conn
            .query_row("SELECT expires_at FROM leases WHERE key = 'j'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(until, NOW + lease::MAX_LEASE);
    }

    #[test]
    fn a_widget_that_stores_titles_forgets_them_sooner() {
        let path = temp("forget");
        let mut s = Store::open_with(&path, Caps { forget_after: 2.0 * 3600.0, ..Caps::default() }).unwrap();
        s.publish("old", &reading(NOW - 3.0 * 3600.0, serde_json::json!({"title": "x"})), &[], NOW - 3.0 * 3600.0).unwrap();
        s.publish("new", &reading(NOW, serde_json::json!(1)), &[], NOW).unwrap();
        let n: i64 = s.conn.query_row("SELECT count(*) FROM readings WHERE key = 'old'", [], |r| r.get(0)).unwrap();
        assert_eq!(n, 0, "a three-hour-old title outlived a two-hour limit");
    }

    #[test]
    fn a_reading_over_the_widgets_cap_is_kept_and_said_so() {
        let path = temp("caps");
        let mut s = Store::open_with(&path, Caps { max_value: 64, ..Caps::default() }).unwrap();
        let big = reading(NOW, serde_json::json!({"titles": "x".repeat(100)}));
        assert_eq!(s.publish("k", &big, &[], NOW).unwrap(), Publish::TooLarge);
        assert_eq!(Publish::TooLarge.unshared(), Some("too large to share"));
        assert!(s.load("k").unwrap().is_none());
    }

    #[test]
    fn an_oversized_file_is_started_again() {
        let path = temp("big");
        Store::open(&path).unwrap();
        let f = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
        f.set_len(SHRINK_AT + 1).unwrap();
        let s = Store::open(&path).unwrap();
        assert!(s.notice.contains("grown"), "{}", s.notice);
        assert!(s.size() < SHRINK_AT);
    }

    #[test]
    fn a_store_another_pane_holds_busy_is_left_alone() {
        use std::os::unix::fs::MetadataExt;
        let path = temp("busy");
        let mut a = Store::open(&path).unwrap();
        assert_eq!(a.publish("k", &reading(NOW, serde_json::json!({"u": 1})), &[], NOW).unwrap(), Publish::Written);
        let inode = std::fs::metadata(&path).unwrap().ino();
        // Another pane in the middle of a write that outlasts `BUSY`.
        a.conn.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let opened = Store::open(&path);
        assert!(opened.is_err(), "a locked store must not open as a fresh one");
        assert!(busy(&opened.err().unwrap()));
        a.conn.execute_batch("COMMIT").unwrap();
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode, "the file was replaced");
        let mut b = Store::open(&path).unwrap();
        assert!(b.notice.is_empty(), "{}", b.notice);
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(serde_json::json!({"u": 1})));
    }

    #[test]
    fn a_store_emptied_for_size_keeps_a_gate_still_in_force() {
        let path = temp("empty-keeps-gate");
        let mut a = Store::open(&path).unwrap();
        a.publish("gate/core", &reading(NOW, serde_json::json!({"opscope_refusal": {}})), &[], NOW).unwrap();
        let body = serde_json::json!({ "u": "x".repeat(1024 * 1024) });
        for i in 0..9 {
            let mut r = reading(NOW, body.clone());
            r.good = None;
            a.publish(&format!("k{i}"), &r, &[], NOW).unwrap();
        }
        assert!(a.notice.contains("emptied"), "{}", a.notice);
        let mut other = Store::open(&path).unwrap();
        assert!(other.load("gate/core").unwrap().is_some(), "emptying the store dropped a gate still in force");
        assert!(other.load("k0").unwrap().is_none());
    }

    #[test]
    fn a_store_emptied_for_size_stays_the_file_every_pane_has_open() {
        use std::os::unix::fs::MetadataExt;
        let path = temp("empty-in-place");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        // More live readings than the file may hold past `SHRINK_AT`, so
        // vacuuming alone cannot bring it back under.
        let inode = std::fs::metadata(&path).unwrap().ino();
        let body = serde_json::json!({ "u": "x".repeat(1024 * 1024) });
        for i in 0..9 {
            let mut r = reading(NOW, body.clone());
            r.good = None;
            a.publish(&format!("k{i}"), &r, &[], NOW).unwrap();
        }
        assert!(a.size() <= SHRINK_AT, "{}", a.size());
        assert!(a.notice.contains("emptied"), "{}", a.notice);
        assert_eq!(std::fs::metadata(&path).unwrap().ino(), inode);
        // The other pane still shares with this one, through the same file.
        assert_eq!(a.publish("after", &reading(NOW, serde_json::json!(7)), &[], NOW).unwrap(), Publish::Written);
        b.begin_pass().unwrap();
        assert_eq!(b.load("after").unwrap().unwrap().value, Some(serde_json::json!(7)));
    }

    #[test]
    fn a_late_answer_does_not_write_over_the_pane_that_took_over() {
        let path = temp("late");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        a.me = 1;
        b.me = 2;
        assert_eq!(a.claim_for("k", None, NOW, 20.0).unwrap(), Got::Mine);
        // a hung past its lease; b took the reading over and is asking.
        assert_eq!(b.claim_for("k", None, NOW + 30.0, 20.0).unwrap(), Got::Mine);
        assert_eq!(a.publish("k", &reading(NOW + 35.0, serde_json::json!(1)), &[], NOW + 35.0).unwrap(), Publish::TakenOver);
        assert_eq!(b.holder("k", NOW + 36.0).unwrap(), Some(2), "the late answer took b's lease");
        assert!(b.load("k").unwrap().is_none(), "the late answer was written");
        assert_eq!(b.publish("k", &reading(NOW + 40.0, serde_json::json!(2)), &[], NOW + 40.0).unwrap(), Publish::Written);
    }

    #[test]
    fn a_late_answer_does_not_write_over_what_the_pane_that_took_over_published() {
        let path = temp("late-after");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        a.me = 1;
        b.me = 2;
        assert_eq!(a.claim_for("k", None, NOW, 20.0).unwrap(), Got::Mine);
        assert_eq!(b.claim_for("k", None, NOW + 30.0, 20.0).unwrap(), Got::Mine);
        // b answers and lets its lease go before a's late answer lands.
        assert_eq!(b.publish("k", &reading(NOW + 32.0, serde_json::json!(2)), &[], NOW + 32.0).unwrap(), Publish::Written);
        let late = reading(NOW + 35.0, serde_json::json!(1));
        assert_eq!(a.publish_since("k", &late, &[], NOW + 35.0, Some(NOW)).unwrap(), Publish::TakenOver);
        let mut other = Store::open(&path).unwrap();
        assert_eq!(other.load("k").unwrap().unwrap().value, Some(serde_json::json!(2)), "the late answer was written over b's");
        // A pane that claimed after b published still writes.
        assert_eq!(a.claim_for("k", None, NOW + 400.0, 20.0).unwrap(), Got::Mine);
        let next = reading(NOW + 401.0, serde_json::json!(3));
        assert_eq!(a.publish_since("k", &next, &[], NOW + 401.0, Some(NOW + 400.0)).unwrap(), Publish::Written);
    }

    #[test]
    fn a_row_dated_ahead_of_the_clock_is_not_a_successor() {
        let path = temp("late-future");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        a.me = 1;
        b.me = 2;
        // b wrote before the clock went back an hour.
        b.publish("k", &reading(NOW + 3600.0, serde_json::json!(2)), &[], NOW + 3600.0).unwrap();
        assert_eq!(a.claim_for("k", None, NOW, 20.0).unwrap(), Got::Mine);
        let mine = reading(NOW + 5.0, serde_json::json!(1));
        assert_eq!(a.publish_since("k", &mine, &[], NOW + 5.0, Some(NOW)).unwrap(), Publish::Written);
    }

    #[test]
    fn a_gate_outlasting_the_retention_stays_until_it_lifts() {
        let path = temp("gate-kept");
        let mut s = Store::open(&path).unwrap();
        let mut gate = reading(NOW - lease::FORGET_AFTER - 1.0, serde_json::json!({"opscope_refusal": {}}));
        gate.held = lease::FORGET_AFTER + 60.0;
        s.publish("gate/core", &gate, &[], NOW - lease::FORGET_AFTER - 1.0).unwrap();
        s.publish("new", &reading(NOW, serde_json::json!(1)), &[], NOW).unwrap();
        let mut other = Store::open(&path).unwrap();
        assert!(other.load("gate/core").unwrap().is_some(), "a gate still in force was forgotten");
        s.publish("later", &reading(NOW + 120.0, serde_json::json!(1)), &[], NOW + 120.0).unwrap();
        other.begin_pass().unwrap();
        assert!(other.load("gate/core").unwrap().is_none(), "a lifted gate was kept past the retention");
    }

    #[test]
    fn a_reading_nobody_has_published_for_a_day_is_forgotten() {
        let path = temp("forget");
        let mut s = Store::open(&path).unwrap();
        s.publish("old", &reading(NOW - lease::FORGET_AFTER - 1.0, serde_json::json!(1)), &[], NOW).unwrap();
        s.publish("new", &reading(NOW, serde_json::json!(1)), &[], NOW).unwrap();
        let mut other = Store::open(&path).unwrap();
        assert!(other.load("old").unwrap().is_none());
        assert!(other.load("new").unwrap().is_some());
    }

    #[test]
    fn a_reading_holding_a_secret_is_never_written() {
        let path = temp("secret");
        let token = "sk-fixture-token-0123456789".to_string();
        let mut s = Store::open(&path).unwrap();
        let leaky = reading(NOW, serde_json::json!({"echo": token.clone()}));
        assert_eq!(s.publish("k", &leaky, std::slice::from_ref(&token), NOW).unwrap(), Publish::HeldSecret);
        assert_eq!(s.publish("j", &reading(NOW, serde_json::json!({"u": 1})), std::slice::from_ref(&token), NOW).unwrap(), Publish::Written);
        drop(s);
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()));
    }

    #[test]
    fn a_token_escaped_in_the_reading_is_still_found() {
        // The reading is stored as JSON, so a `"` in the token is written `\"`.
        let path = temp("secret-escaped");
        let token = r#"fixture"token\0123456789"#.to_string();
        let mut s = Store::open(&path).unwrap();
        let leaky = reading(NOW, serde_json::json!({"echo": token.clone()}));
        assert_eq!(s.publish("k", &leaky, std::slice::from_ref(&token), NOW).unwrap(), Publish::HeldSecret);
    }

    /// A reason written in after the publish - `set_live` - is held to the
    /// same rule as the publish itself.
    #[test]
    fn an_annotation_holding_a_secret_is_never_written() {
        let path = temp("annotate-secret");
        let token = "sk-fixture-token-0123456789".to_string();
        let secrets = std::slice::from_ref(&token);
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.publish("k", &reading(NOW, serde_json::json!({"u": 1})), secrets, NOW).unwrap(), Publish::Written);
        let leaky = serde_json::json!({"why": format!("refused for {token}")});
        assert!(!s.annotate("k", NOW, &leaky, secrets).unwrap());
        let plain = serde_json::json!({"why": "too many requests"});
        assert!(s.annotate("k", NOW, &plain, secrets).unwrap());
        drop(s);
        let bytes = std::fs::read(&path).unwrap();
        assert!(!bytes.windows(token.len()).any(|w| w == token.as_bytes()));
    }

    #[test]
    fn a_day_of_six_panes_stays_small_and_writes_only_on_fetches() {
        // Thirty readings, the number agent-usage keeps with every agent
        // on, each a few kilobytes, fetched on a two-minute hold for a day
        // by whichever of six panes gets there first.
        let path = temp("budget");
        let mut panes: Vec<Store> = (0..6)
            .map(|i| {
                let mut s = Store::open(&path).unwrap();
                s.me = 5000 + i;
                s
            })
            .collect();
        let body = serde_json::json!({ "u": "x".repeat(3000) });
        // What each pane holds, as `held_for` keeps it in `Caches.live`.
        let mut held: Vec<HashMap<String, f64>> = vec![HashMap::new(); panes.len()];
        let mut fetches = 0u64;
        let mut t = NOW;
        while t < NOW + 24.0 * 3600.0 {
            for (p, mine) in panes.iter_mut().zip(held.iter_mut()) {
                p.begin_pass().unwrap();
                for k in 0..30 {
                    let key = format!("vendor:{k}");
                    if let Some(s) = p.load(&key).unwrap() {
                        mine.insert(key.clone(), s.taken_at);
                    }
                    let known = mine.get(&key).copied();
                    // Still good here: served without asking anyone.
                    if known.is_some_and(|k| lease::good_at(k, 120.0, t)) {
                        continue;
                    }
                    match p.claim(&key, known, t).unwrap() {
                        Got::Mine => {
                            fetches += 1;
                            let mut r = reading(t, body.clone());
                            r.held = 120.0;
                            p.publish(&key, &r, &[], t).unwrap();
                            mine.insert(key, t);
                        }
                        Got::Fresh(s) => {
                            mine.insert(key, s.taken_at);
                        }
                        Got::Held(_) => {}
                    }
                }
            }
            t += 30.0;
        }
        let writes: u64 = panes.iter().map(|p| p.writes).sum();
        // One claim and one publish per fetch, nothing else.
        assert_eq!(writes, 2 * fetches);
        // A day at a two-minute hold is 720 fetches a reading; six panes
        // must not have multiplied that.
        assert!(fetches <= 30 * 721, "{fetches} fetches");
        let size = panes[0].size();
        assert!(size < 1024 * 1024, "{size} bytes");
    }

    #[test]
    fn a_good_reading_that_is_the_value_is_stored_once_and_read_back_whole() {
        let path = temp_store("good-is-value");
        let mut s = Store::open(&path).unwrap();
        let reading = serde_json::json!({"u": 1});
        let mut r = Shared {
            taken_at: NOW,
            held: 300.0,
            value: Some(reading.clone()),
            fails: 0,
            good: Some((NOW, reading.clone())),
            fetched_by: 0,
        };
        assert_eq!(s.publish("k", &r, &[], NOW).unwrap(), Publish::Written);
        let good_text: Option<String> = s
            .conn
            .query_row("SELECT good_value FROM readings WHERE key = 'k'", [], |x| x.get(0))
            .unwrap();
        assert_eq!(good_text, None, "the value was written twice");
        let back = s.load("k").unwrap().unwrap();
        assert_eq!(back.value, Some(reading.clone()));
        assert_eq!(back.good, Some((NOW, reading)), "the good reading was lost");
        // A value that differs from the good one is still stored twice.
        r.value = Some(serde_json::json!({"u": 2}));
        s.publish("j", &r, &[], NOW).unwrap();
        let both: Option<String> = s
            .conn
            .query_row("SELECT good_value FROM readings WHERE key = 'j'", [], |x| x.get(0))
            .unwrap();
        assert!(both.is_some());
    }

    #[test]
    fn the_same_answer_again_keeps_the_stored_text_identical() {
        // What OPS-298 is for: an unchanged reading must not be written
        // again. Two publishes of the same answer leave the row's text as
        // it was, with only its times moved.
        let path = temp_store("same-again");
        let mut s = Store::open(&path).unwrap();
        let reading = serde_json::json!({"u": 1});
        let r = |at: f64| Shared {
            taken_at: at,
            held: 300.0,
            value: Some(reading.clone()),
            fails: 0,
            good: Some((at, reading.clone())),
            fetched_by: 0,
        };
        s.publish("k", &r(NOW), &[], NOW).unwrap();
        let text = |s: &Store| -> (Option<String>, Option<String>, f64) {
            s.conn
                .query_row("SELECT value, good_value, taken_at FROM readings WHERE key = 'k'", [], |x| {
                    Ok((x.get(0)?, x.get(1)?, x.get(2)?))
                })
                .unwrap()
        };
        let first = text(&s);
        s.publish("k", &r(NOW + 60.0), &[], NOW + 60.0).unwrap();
        let second = text(&s);
        assert_eq!((second.0.clone(), second.1.clone()), (first.0, first.1));
        // The time moves without the reading being rewritten: it is kept
        // beside it, in the check stamp.
        let at = s.load("k").unwrap().unwrap().taken_at;
        assert_eq!(at, NOW + 60.0, "the reading's time did not move");
        s.seen.clear();
        let back = s.load("k").unwrap().unwrap();
        assert_eq!(back.taken_at, NOW + 60.0);
        assert_eq!(back.good.unwrap().0, NOW + 60.0, "the last good time stayed at the first publish");
    }

    #[test]
    fn a_check_keeps_an_older_answer_from_replacing_the_reading() {
        let path = temp("check-older");
        let mut s = Store::open(&path).unwrap();
        let v = serde_json::json!({"u": 1});
        s.publish("k", &reading(NOW, v.clone()), &[], NOW).unwrap();
        s.publish("k", &reading(NOW + 60.0, v.clone()), &[], NOW + 60.0).unwrap();
        let text = s.stored_text_for_tests("k");
        let got = s.publish("k", &reading(NOW + 40.0, serde_json::json!({"u": 2})), &[], NOW + 40.0).unwrap();
        assert_eq!(got, Publish::Older);
        assert_eq!(s.stored_text_for_tests("k"), text, "an answer older than the check replaced the reading");
        s.seen.clear();
        assert_eq!(s.load("k").unwrap().unwrap().taken_at, NOW + 60.0);
    }

    #[test]
    fn a_recent_check_is_not_forgotten_with_the_original_time() {
        let path = temp("check-keep");
        let mut s = Store::open_with(&path, Caps { forget_after: 100.0, ..Default::default() }).unwrap();
        let v = serde_json::json!(1);
        let mut old = reading(NOW - 200.0, v.clone());
        old.held = 30.0;
        old.good = Some((NOW - 200.0, v.clone()));
        s.publish("old", &old, &[], NOW - 200.0).unwrap();
        let mut again = reading(NOW, v);
        again.held = 30.0;
        s.publish("old", &again, &[], NOW).unwrap();
        s.publish("other", &reading(NOW, serde_json::json!(2)), &[], NOW).unwrap();
        s.seen.clear();
        assert!(s.load("old").unwrap().is_some(), "a fresh check was forgotten with the original time");
    }

    #[test]
    fn a_replaced_reading_is_not_hidden_behind_its_old_check() {
        let path = temp("check-stale");
        let mut s = Store::open(&path).unwrap();
        let v = serde_json::json!(1);
        s.publish("k", &reading(NOW, v.clone()), &[], NOW).unwrap();
        s.publish("k", &reading(NOW + 60.0, v), &[], NOW + 60.0).unwrap();
        let mut next = reading(NOW + 120.0, serde_json::json!(2));
        next.held = 60.0;
        s.publish("k", &next, &[], NOW + 120.0).unwrap();
        match s.claim("k", None, NOW + 130.0).unwrap() {
            Got::Fresh(back) => {
                assert_eq!(back.value, Some(serde_json::json!(2)));
                assert_eq!(back.taken_at, NOW + 120.0, "the old check hid the new reading");
            }
            other => panic!("a fresh replacement was treated as stale: {other:?}"),
        }
    }

    #[test]
    fn a_batch_answer_does_not_replace_a_successor() {
        let path = temp("successor");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        let newer = serde_json::json!({"n": 2});
        b.publish("k", &reading(NOW + 30.0, newer.clone()), &[], NOW + 30.0).unwrap();
        let late = reading(NOW + 40.0, serde_json::json!({"n": 1}));
        let got = a.publish_many(&[("k", late)], &[], NOW + 40.0, Some(NOW)).unwrap();
        assert_eq!(got, vec![Publish::TakenOver]);
        b.seen.clear();
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(newer));
    }

    #[test]
    fn publishing_one_answer_keeps_the_other_lease() {
        let path = temp("one-lease");
        let mut s = Store::open(&path).unwrap();
        assert_eq!(s.claim("a", None, NOW).unwrap(), Got::Mine);
        assert_eq!(s.claim("b", None, NOW).unwrap(), Got::Mine);
        s.publish_many(&[("a", reading(NOW, serde_json::json!(1)))], &[], NOW, Some(NOW)).unwrap();
        assert_eq!(s.lease_count_for_tests("a"), 0, "the answered lease was kept");
        assert_eq!(s.lease_count_for_tests("b"), 1, "an unanswered lease was released");
    }

    #[test]
    fn an_annotation_finds_a_reading_by_its_check() {
        let path = temp("annotate-check");
        let mut s = Store::open(&path).unwrap();
        let v = serde_json::json!({"u": 1});
        s.publish("k", &reading(NOW, v.clone()), &[], NOW).unwrap();
        s.publish("k", &reading(NOW + 60.0, v), &[], NOW + 60.0).unwrap();
        assert!(s.annotate("k", NOW + 60.0, &serde_json::json!({"why": "later"}), &[]).unwrap());
        s.seen.clear();
        assert_eq!(s.load("k").unwrap().unwrap().value, Some(serde_json::json!({"why": "later"})));
    }

    #[test]
    fn an_older_annotation_does_not_replace_a_later_check() {
        let path = temp("annotate-later");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        let v = serde_json::json!({"u": 1});
        a.publish("k", &reading(NOW, v.clone()), &[], NOW).unwrap();
        b.publish("k", &reading(NOW + 60.0, v.clone()), &[], NOW + 60.0).unwrap();
        assert!(!a.annotate("k", NOW, &serde_json::json!({"why": "stale"}), &[]).unwrap());
        b.seen.clear();
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(v));
        assert!(b.annotate("k", NOW + 60.0, &serde_json::json!({"why": "later"}), &[]).unwrap());
        b.seen.clear();
        let back = b.load("k").unwrap().unwrap();
        assert_eq!(back.value, Some(serde_json::json!({"why": "later"})));
        assert_eq!(back.good.unwrap().0, NOW + 60.0, "the check's time was dropped when the good value was materialised");
    }

    #[test]
    fn a_later_check_is_a_successor() {
        let path = temp("check-successor");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        let kept = serde_json::json!({"n": 1});
        a.publish("k", &reading(NOW, kept.clone()), &[], NOW).unwrap();
        b.publish("k", &reading(NOW + 50.0, kept.clone()), &[], NOW + 50.0).unwrap();
        let late = reading(NOW + 80.0, serde_json::json!({"n": 9}));
        let got = a.publish_many(&[("k", late)], &[], NOW + 80.0, Some(NOW + 10.0)).unwrap();
        assert_eq!(got, vec![Publish::TakenOver]);
        b.seen.clear();
        assert_eq!(b.load("k").unwrap().unwrap().value, Some(kept));
    }

    #[test]
    fn a_quiet_republish_names_the_pane_that_checked() {
        let path = temp("check-by");
        let mut a = Store::open(&path).unwrap();
        let mut b = Store::open(&path).unwrap();
        b.me = a.me + 1;
        let v = serde_json::json!({"u": 1});
        a.publish("k", &reading(NOW, v.clone()), &[], NOW).unwrap();
        b.publish("k", &reading(NOW + 60.0, v), &[], NOW + 60.0).unwrap();
        b.seen.clear();
        assert_eq!(b.load("k").unwrap().unwrap().fetched_by, b.me);
    }

    #[test]
    fn an_annotation_keeps_the_value_it_replaces_as_the_good_reading() {
        let path = temp_store("annotate-good");
        let mut s = Store::open(&path).unwrap();
        let good = serde_json::json!({"u": 1});
        s.publish(
            "k",
            &Shared { taken_at: NOW, held: 300.0, value: Some(good.clone()), fails: 0, good: Some((NOW, good.clone())), fetched_by: 0 },
            &[],
            NOW,
        )
        .unwrap();
        s.annotate("k", NOW, &serde_json::json!({"opscope_refusal": {"why": "busy", "until": NOW + 60.0}}), &[])
            .unwrap();
        let back = s.load("k").unwrap().unwrap();
        assert_eq!(back.good, Some((NOW, good)), "the refusal became the kept reading");
    }
}

#[cfg(test)]
mod write_bench {
    use super::tests_support::temp;
    use super::*;
    use crate::readings::{answer_many, claim_many, Answer, Cache, Claimed, Policy};

    const NOW: f64 = 1_000_000.0;

    /// Force the store's dirty pages out, since the store runs with no
    /// fsync and `/proc/self/io` only counts what reached the disk.
    fn flush(path: &str) {
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.sync_all();
        }
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(format!("{path}-journal")) {
            let _ = f.sync_all();
        }
    }

    fn written() -> u64 {
        std::fs::read_to_string("/proc/self/io")
            .ok()
            .and_then(|t| {
                t.lines()
                    .find(|l| l.starts_with("write_bytes:"))
                    .and_then(|l| l.split_whitespace().nth(1).map(|n| n.parse().ok()))
                    .flatten()
            })
            .unwrap_or(0)
    }

    fn value(i: usize, at: f64) -> serde_json::Value {
        if i == 0 {
            let runs: Vec<serde_json::Value> = (0..400)
                .map(|n| serde_json::json!({"id": n, "note": "x".repeat(400), "at": at}))
                .collect();
            serde_json::json!({"runs": runs})
        } else {
            serde_json::json!({"i": i, "at": at})
        }
    }

    /// Not a check: a measurement. 21 readings, one of them large, then one
    /// quiet pass and one in which every answer changed - what a minute of
    /// github-actions costs, now that a pass claims and publishes in one
    /// commit each. `cargo test -p opscope-core --lib write_bench -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn measure_a_pass() {
        let path = temp("bench");
        let mut cache = Cache::default();
        cache.store = Some(Store::open(&path).unwrap());
        let keys: Vec<String> = (0..21).map(|i| format!("k{i}")).collect();
        let asks: Vec<(&str, Policy)> = keys.iter().map(|k| (k.as_str(), Policy::new(60.0, 30.0))).collect();
        let mut pass = |cache: &mut Cache, at: f64, changing: bool| {
            let claimed = claim_many(cache, &asks, at);
            let mut done = Vec::new();
            for (i, (key, got)) in keys.iter().zip(claimed).enumerate() {
                if matches!(got, Claimed::Fetch) {
                    let v = if changing { value(i, at) } else { value(i, NOW) };
                    done.push((key.as_str(), Answer::Value(v), Policy::new(60.0, 30.0), at));
                }
            }
            answer_many(cache, &done, at, at);
        };
        pass(&mut cache, NOW, false);
        let size = cache.store.as_ref().unwrap().size();
        flush(&path);
        let start = written();
        pass(&mut cache, NOW + 100.0, false);
        flush(&path);
        let quiet = written() - start;
        let start = written();
        pass(&mut cache, NOW + 200.0, true);
        flush(&path);
        let changed = written() - start;
        println!(
            "store {size} B; quiet pass {quiet} B; changed pass {changed} B; per hour at 60 s: {} KiB quiet, {} KiB changed",
            quiet * 60 / 1024,
            changed * 60 / 1024
        );
    }
}
