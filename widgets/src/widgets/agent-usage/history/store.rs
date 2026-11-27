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

//! `agent-usage-history.sqlite`: open, migrate, write, re-fold.
//!
//! **Not the readings cache.** `agent-usage.sqlite` keeps the latest reading
//! only, caps itself and is deleted when damaged - right for a cache, wrong
//! for a history nobody can fetch again. So this file is its own (§3.1):
//!
//! * `0600`, made before SQLite opens it;
//! * `journal_mode=WAL`, `synchronous=NORMAL` - a lost write is lost for
//!   ever, so it is worth an fsync at checkpoints;
//! * `busy_timeout` as the readings store has it;
//! * `PRAGMA user_version` and numbered, forward-only [`MIGRATIONS`], each
//!   in one `BEGIN IMMEDIATE` transaction, so racing panes migrate once;
//! * a file with a **newer** version is left alone ([`OpenError::Newer`]);
//! * a file SQLite calls damaged is **renamed** `.damaged-<epoch>`, never
//!   deleted, and [`History::notice`] says so.
//!
//! Every write ([`History::record`]) is one `BEGIN IMMEDIATE` transaction
//! that reads the previous state from the file, not from this pane's
//! memory, so two panes cannot both open a cycle; the unique keys back
//! that up.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::fold::{
    self, Cycle, CycleModel, EndReason, FoldInput, PlanChange, Role, Sample, Segment, Source, StartSource, Thresholds,
};
use super::{BankReading, Observation, WatchReading, backfill, overage, resets};
use crate::shared::{Lane, Spacexsi};

/// The file's name under `tc::state_dir()`.
pub const FILE: &str = "agent-usage-history.sqlite";

/// The schema this build writes: the length of [`MIGRATIONS`].
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// How long a write waits for another pane's transaction. The readings
/// store's figure; a transaction here is a handful of rows.
const BUSY: Duration = Duration::from_millis(250);

/// How many times opening a busy file is tried before giving up for now.
const OPEN_TRIES: u32 = 3;

/// The heading the unified plan's own account carries.
const SPACEXSI_HEADING: &str = crate::shared::SPACEXSI;

/// Migration `n` (from 0) takes the file from `user_version = n` to `n + 1`.
/// Forward only: never edit one that has shipped, add the next.
pub const MIGRATIONS: &[&str] = &[V1];

/// Schema v1, spec §3.6: every table, the derived ones phases 2 to 5 fill
/// included, so the schema does not change between phases.
const V1: &str = "
CREATE TABLE meta (k TEXT PRIMARY KEY, v);

CREATE TABLE account (
  id          INTEGER PRIMARY KEY,
  provider    TEXT NOT NULL,
  account_key TEXT NOT NULL,
  heading     TEXT NOT NULL,
  first_seen  REAL NOT NULL, last_seen REAL NOT NULL,
  UNIQUE (provider, account_key)
);

CREATE TABLE pool (
  id          INTEGER PRIMARY KEY,
  account_id  INTEGER NOT NULL REFERENCES account(id),
  pool_key    TEXT NOT NULL,
  label       TEXT NOT NULL,
  role        TEXT NOT NULL,
  unit        TEXT NOT NULL,
  currency    TEXT, exponent INTEGER,
  first_seen  REAL NOT NULL, last_seen REAL NOT NULL,
  UNIQUE (account_id, pool_key)
);

CREATE TABLE pool_link (
  overage_id  INTEGER NOT NULL REFERENCES pool(id),
  included_id INTEGER NOT NULL REFERENCES pool(id),
  PRIMARY KEY (overage_id, included_id)
);

CREATE TABLE sample (
  pool_id      INTEGER NOT NULL REFERENCES pool(id),
  at           REAL NOT NULL,
  pct          REAL,
  used_amount  REAL, limit_amount REAL,
  window_secs  REAL,
  reset_at     REAL,
  source       TEXT NOT NULL,
  spacexsi     TEXT,
  PRIMARY KEY (pool_id, at)
) WITHOUT ROWID;

CREATE TABLE bank_sample (
  account_id  INTEGER NOT NULL REFERENCES account(id),
  at          REAL NOT NULL,
  left        INTEGER NOT NULL,
  credits     TEXT,
  PRIMARY KEY (account_id, at)
) WITHOUT ROWID;

CREATE TABLE plan_sample (
  account_id  INTEGER NOT NULL REFERENCES account(id),
  at          REAL NOT NULL,
  plan        TEXT NOT NULL,
  PRIMARY KEY (account_id, at)
) WITHOUT ROWID;

CREATE TABLE watch_state (
  account_id  INTEGER NOT NULL REFERENCES account(id),
  at          REAL NOT NULL,
  state       TEXT NOT NULL,
  reason      TEXT,
  PRIMARY KEY (account_id, at)
) WITHOUT ROWID;

CREATE TABLE cycle (
  id            INTEGER PRIMARY KEY,
  pool_id       INTEGER NOT NULL REFERENCES pool(id),
  ordinal       INTEGER NOT NULL,
  starts_at     REAL NOT NULL,
  ends_at       REAL,
  closed_at     REAL,
  window_secs   REAL,
  cycle_model   TEXT NOT NULL,
  start_source  TEXT NOT NULL,
  end_reason    TEXT,
  plan          TEXT,
  peak_pct      REAL,
  consumed_pct  REAL,
  allowances    INTEGER NOT NULL DEFAULT 1,
  available_pct REAL,
  end_is_lower_bound INTEGER NOT NULL,
  used_amount   REAL, limit_amount REAL,
  exhausted_at  REAL,
  first_obs_at  REAL, last_obs_at REAL,
  coverage      REAL,
  UNIQUE (pool_id, ordinal)
);
CREATE UNIQUE INDEX one_open_cycle ON cycle(pool_id) WHERE closed_at IS NULL;

CREATE TABLE segment (
  cycle_id  INTEGER NOT NULL REFERENCES cycle(id),
  n         INTEGER NOT NULL,
  starts_at REAL NOT NULL, ends_at REAL,
  peak_pct  REAL,
  PRIMARY KEY (cycle_id, n)
);

CREATE TABLE reset_event (
  id              INTEGER PRIMARY KEY,
  pool_id         INTEGER NOT NULL REFERENCES pool(id),
  cycle_id        INTEGER REFERENCES cycle(id),
  at_lo REAL NOT NULL, at_hi REAL NOT NULL,
  kind            TEXT NOT NULL,
  cycle_effect    TEXT NOT NULL,
  signals         TEXT NOT NULL,
  pct_before REAL, pct_after REAL,
  reset_at_before REAL, reset_at_after REAL,
  group_id        INTEGER,
  evidence        TEXT NOT NULL,
  UNIQUE (pool_id, at_hi)
);

CREATE TABLE bank_event (
  id         INTEGER PRIMARY KEY,
  account_id INTEGER NOT NULL REFERENCES account(id),
  at_lo REAL NOT NULL, at_hi REAL NOT NULL,
  kind       TEXT NOT NULL,
  count      INTEGER NOT NULL,
  expires_at REAL,
  UNIQUE (account_id, at_hi, kind)
);

CREATE TABLE overage_event (
  overage_id  INTEGER NOT NULL REFERENCES pool(id),
  event_key   TEXT NOT NULL,
  at          REAL NOT NULL,
  amount      REAL NOT NULL,
  model       TEXT,
  PRIMARY KEY (overage_id, event_key)
) WITHOUT ROWID;

CREATE TABLE overage_span (
  overage_id  INTEGER NOT NULL REFERENCES pool(id),
  at_lo REAL NOT NULL, at_hi REAL NOT NULL,
  amount      REAL NOT NULL,
  exhausted   TEXT NOT NULL,
  PRIMARY KEY (overage_id, at_hi)
);
";

/// Where the history lives.
pub fn path() -> PathBuf {
    opscope_core::state_dir().join(FILE)
}

/// Why the file could not be opened.
#[derive(Debug, PartialEq)]
pub enum OpenError {
    /// Written by a newer opscope: `user_version` past [`SCHEMA_VERSION`].
    /// Left exactly as it is.
    Newer(i64),
    Failed(String),
}

/// Whether an error is another pane holding the file, which passes.
pub fn busy(why: &str) -> bool {
    why.contains("database is locked") || why.contains("database table is locked") || why.contains("busy")
}

fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

fn damaged(e: &rusqlite::Error) -> bool {
    matches!(
        e.sqlite_error_code(),
        Some(rusqlite::ErrorCode::NotADatabase | rusqlite::ErrorCode::DatabaseCorrupt)
    )
}

/// What went wrong opening, and whether it was the file itself.
struct Failure {
    error: OpenError,
    damaged: bool,
    /// The device and inode of the file that failed, so it is that file,
    /// and no other, that is set aside.
    identity: Option<(u64, u64)>,
}

impl Failure {
    fn from_sql(e: rusqlite::Error) -> Failure {
        Failure {
            damaged: damaged(&e),
            error: OpenError::Failed(e.to_string()),
            identity: None,
        }
    }

    fn other(why: String) -> Failure {
        Failure {
            error: OpenError::Failed(why),
            damaged: false,
            identity: None,
        }
    }
}

/// The open history file.
pub struct History {
    /// `overage` reads it for phase 3's overage helpers.
    pub(super) conn: Connection,
    pub path: PathBuf,
    /// Set when opening set a damaged file aside, to be said once.
    pub notice: Option<String>,
    /// The readings this pane held back as unchanged ([`bridge`]).
    seen: Seen,
}

/// The device and inode of the file at `path`, or `None` when there is none.
fn identity(path: &Path) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    std::fs::metadata(path).ok().map(|m| (m.dev(), m.ino()))
}

/// Move `from` to `to` without ever replacing what `to` holds: a hard link
/// that fails when `to` exists, then the old name removed. Where links are
/// not supported, a rename to a name checked free under the lock.
fn move_new(from: &Path, to: &Path) -> std::io::Result<()> {
    match std::fs::hard_link(from, to) {
        Ok(()) => std::fs::remove_file(from),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Err(e),
        Err(_) if !to.exists() => std::fs::rename(from, to),
        Err(e) => Err(e),
    }
}

/// The damaged file moved out of the way: `<name>.damaged-<epoch>-<pid>-<n>`,
/// with its WAL and shared-memory files beside it under the same stem, so
/// none of them is replayed into the new file. Never overwrites an earlier
/// set-aside.
///
/// Two panes can find the same file damaged at once, so it is done under an
/// exclusive lock on `<name>.lock`, and only while the file at `path` is still
/// the one that failed (`failed`, its device and inode when it was opened):
/// otherwise another pane has already set it aside and started a new file,
/// and `None` says to open that one instead of moving it.
fn set_aside(path: &Path, at: i64, failed: Option<(u64, u64)>) -> Result<Option<PathBuf>, String> {
    let lock = recovery_lock(path).map_err(|e| format!("cannot lock the damaged file: {e}"))?;
    lock.lock().map_err(|e| format!("cannot lock the damaged file: {e}"))?;
    let now = identity(path);
    if now.is_none() || (failed.is_some() && now != failed) {
        return Ok(None);
    }
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.subsec_nanos());
    let mut n = 0u32;
    let aside = loop {
        let name = PathBuf::from(format!(
            "{}.damaged-{at}-{}-{nanos}{}",
            path.display(),
            std::process::id(),
            if n == 0 { String::new() } else { format!("-{n}") }
        ));
        match move_new(path, &name) {
            Ok(()) => break name,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && n < 100 => n += 1,
            Err(e) => return Err(format!("cannot set the damaged file aside: {e}")),
        }
    };
    move_sides(path, &aside)?;
    Ok(Some(aside))
}

/// `<name>.lock`, which recovery holds exclusively while it moves a damaged
/// file and its side files, and opening holds shared while it looks for
/// the main file and makes a new one: so no pane starts a new file while
/// the old one's WAL is still at the live path.
fn recovery_lock(path: &Path) -> std::io::Result<std::fs::File> {
    use std::os::unix::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(format!("{}.lock", path.display()))
}

/// Move the WAL and shared-memory files after the main file set aside as
/// `aside`. One that cannot follow is left exactly where it is - a WAL can
/// hold committed history not yet in the main file, so it is never deleted -
/// and the recovery is incomplete: no new file is started beside it (see
/// [`History::open_once`]).
fn move_sides(path: &Path, aside: &Path) -> Result<(), String> {
    for side in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{side}", path.display()));
        if from.exists() {
            let to = PathBuf::from(format!("{}{side}", aside.display()));
            move_new(&from, &to).map_err(|e| {
                format!(
                    "the damaged file was kept as {}, but its {side} file could not follow ({e}); it is left in place and no new file is started until it is moved",
                    aside.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
                )
            })?;
        }
    }
    Ok(())
}

impl History {
    /// Open the history with the default thresholds: see [`History::open_with`].
    pub fn open(path: &Path) -> Result<History, OpenError> {
        History::open_with(path, &Thresholds::default())
    }

    /// Open the history, making and migrating it as needed. A damaged file
    /// is renamed and a new one started; a newer one is left alone. A file
    /// folded by another detector, or with other thresholds than `t`, is
    /// folded again with `t` - the configured ones, never the defaults.
    pub fn open_with(path: &Path, t: &Thresholds) -> Result<History, OpenError> {
        let mut tries = 0;
        loop {
            match History::open_once(path, t) {
                Ok(h) => return Ok(h),
                Err(f) if f.damaged => {
                    let first = match &f.error {
                        OpenError::Failed(why) => why.clone(),
                        OpenError::Newer(v) => format!("version {v}"),
                    };
                    let Some(aside) =
                        set_aside(path, opscope_core::now() as i64, f.identity).map_err(OpenError::Failed)?
                    else {
                        // Another pane set it aside first: open what it made.
                        tries += 1;
                        if tries >= OPEN_TRIES {
                            return Err(f.error);
                        }
                        continue;
                    };
                    let mut h = History::open_once(path, t).map_err(|f| f.error)?;
                    let name = aside
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    h.notice = Some(format!(
                        "history file was damaged ({first}); kept as {name}, started again"
                    ));
                    return Ok(h);
                }
                Err(Failure {
                    error: OpenError::Failed(why),
                    ..
                }) if busy(&why) && tries + 1 < OPEN_TRIES => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(f) => return Err(f.error),
            }
        }
    }

    fn open_once(path: &Path, t: &Thresholds) -> Result<History, Failure> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Failure::other(format!("cannot make {}: {e}", dir.display())))?;
        }
        // Shared against recovery's exclusive hold, so a damaged file's side
        // files have either all moved or not started to.
        let lock = recovery_lock(path).map_err(|e| Failure::other(format!("cannot lock it: {e}")))?;
        lock.lock_shared().map_err(|e| Failure::other(format!("cannot lock it: {e}")))?;
        // A WAL with no main file is a set-aside file's log that could not
        // follow it: a new file beside it would have that log replayed
        // into it. Nothing is created until it is moved.
        let wal = PathBuf::from(format!("{}-wal", path.display()));
        if !path.exists() && wal.exists() {
            return Err(Failure::other(format!(
                "{} is still beside a missing history file; move it away to start a new one",
                wal.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default()
            )));
        }
        // Made before SQLite opens it, so it is 0600 whatever the umask; the
        // WAL and shared-memory files SQLite makes take the same mode.
        let existed = match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)
        {
            Ok(_) => false,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => true,
            Err(e) => return Err(Failure::other(format!("cannot create it: {e}"))),
        };
        // The file as it is now, before SQLite opens it: what a damaged
        // verdict below is about.
        let id = identity(path);
        drop(lock);
        History::open_conn(path, existed, t).map_err(|mut f| {
            f.identity = id;
            f
        })
    }

    fn open_conn(path: &Path, existed: bool, t: &Thresholds) -> Result<History, Failure> {
        let conn = Connection::open(path).map_err(Failure::from_sql)?;
        conn.busy_timeout(BUSY).map_err(Failure::from_sql)?;
        // The version first: a newer file is left exactly as it is, which
        // includes not switching its journal mode or its permissions.
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(Failure::from_sql)?;
        if version > SCHEMA_VERSION {
            return Err(Failure {
                error: OpenError::Newer(version),
                damaged: false,
                identity: None,
            });
        }
        // A file copied in, or left by an older build, keeps whatever mode
        // it was given; it is restricted before anything is written to it.
        if existed {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
                .map_err(|e| Failure::other(format!("cannot restrict it: {e}")))?;
        }
        let ok: String = conn
            .query_row("PRAGMA quick_check", [], |r| r.get(0))
            .map_err(Failure::from_sql)?;
        if ok != "ok" {
            return Err(Failure {
                error: OpenError::Failed(format!("quick_check said {ok}")),
                damaged: true,
                identity: None,
            });
        }
        let _: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(Failure::from_sql)?;
        conn.pragma_update(None, "synchronous", "NORMAL")
            .map_err(Failure::from_sql)?;
        migrate(&conn)?;
        let mut h = History {
            conn,
            path: path.to_path_buf(),
            notice: None,
            seen: Seen::new(),
        };
        h.refold_if_detector_changed(t).map_err(Failure::other)?;
        Ok(h)
    }

    /// Fold every pool again when the file was folded by another detector
    /// or with other thresholds, in a transaction of its own.
    fn refold_if_detector_changed(&mut self, t: &Thresholds) -> Result<(), String> {
        if !stale(&self.conn, t)? {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        // Another pane may have done it while this one waited.
        refold_if_stale(&tx, t)?;
        tx.commit().map_err(sql)
    }

    /// The key the unified plan's copy of `o` is kept under. See
    /// [`spacexsi_key`].
    pub fn spacexsi_key_of(&self, o: &Observation) -> Option<String> {
        let own = self.account_key_of(o);
        spacexsi_key(&self.conn, o, &own).ok().flatten()
    }

    /// The key `o` is stored under. See [`key_of`].
    pub fn account_key_of(&self, o: &Observation) -> String {
        key_of(&self.conn, o).unwrap_or_else(|_| o.account_key())
    }

    /// The first sample of any pool of one account, or `None` when it has
    /// none yet.
    pub fn since(&self, provider: &str, account_key: &str) -> Option<f64> {
        self.conn
            .query_row(
                "SELECT MIN((SELECT at FROM sample s WHERE s.pool_id = p.id ORDER BY at LIMIT 1))
                   FROM pool p JOIN account a ON a.id = p.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2",
                params![provider, account_key],
                |r| r.get::<_, Option<f64>>(0),
            )
            .ok()
            .flatten()
    }

    /// Write one pass and fold what it changed, in one transaction.
    ///
    /// The held-back readings are part of what the transaction changes: a
    /// pass that writes a bridge forgets the readings it came from. So a
    /// pass that fails puts them back as they were, or the rolled-back
    /// file would come back without its bridge and the pane without the
    /// readings to write it again, and the fall it showed was watched would
    /// read as an offline reset.
    pub fn record(&mut self, obs: &[Observation], t: &Thresholds, now: f64) -> Result<(), String> {
        let held = self.seen.clone();
        let out = self.record_pass(obs, t, now);
        if out.is_err() {
            self.seen = held;
        }
        out
    }

    fn record_pass(&mut self, obs: &[Observation], t: &Thresholds, now: f64) -> Result<(), String> {
        let seen = &mut self.seen;
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        // Thresholds changed in the config since the file was folded.
        refold_if_stale(&tx, t)?;
        let mut refold: Vec<(i64, f64)> = Vec::new();
        let mut account_refold: Vec<(i64, f64)> = Vec::new();
        let mut banks_changed: Vec<i64> = Vec::new();
        let mut unchanged_banks: Vec<(i64, BankReading)> = Vec::new();
        // Each account with a live lane this pass, and the pool keys that
        // answered live: the evidence `pool_gone` needs.
        let mut answered: Vec<(i64, Vec<String>)> = Vec::new();
        for o in obs {
            let key = key_of(&tx, o)?;
            if o.has_stable_id() && meta_get(&tx, &last_account(o))?.as_deref() != Some(&*key) {
                meta_set(&tx, &last_account(o), &key)?;
            }
            let account = upsert_account(&tx, &o.provider, &key, &o.heading, now)?;
            // Where a shared plan was keyed this pass, remember it for a
            // pass that cannot key it (`spacexsi_key`).
            if let (Some(Spacexsi::Shared), Some(sk)) = (o.spacexsi, &o.spacexsi_account) {
                if meta_get(&tx, &partner(o, &key))?.as_deref() != Some(sk.as_str()) {
                    meta_set(&tx, &partner(o, &key), sk)?;
                }
            }
            let spacexsi_account = spacexsi_key(&tx, o, &key)?;
            let sx = o.spacexsi.map(super::spacexsi_name);
            write_lanes(&tx, account, &o.lanes, sx, seen, t.watch_gap_secs, &mut refold)?;
            let live: Vec<String> = o
                .lanes
                .iter()
                .filter(|l| !l.stale && !l.projected && l.fetched_at > 0.0 && !l.pool_key.is_empty())
                .map(|l| l.pool_key.clone())
                .collect();
            if !live.is_empty() {
                answered.push((account, live));
            }
            // On the unified plan the lanes are kept twice (§3.4): under the
            // vendor, and under SpaceXSI, where its cycles are derived.
            let copy = spacexsi_account.as_ref().map(|_| o.for_spacexsi());
            if let (Some(sk), Some(u)) = (&spacexsi_account, &copy) {
                let unified = upsert_account(&tx, "spacexsi", sk, SPACEXSI_HEADING, now)?;
                write_lanes(&tx, unified, &u.lanes, sx, seen, t.watch_gap_secs, &mut refold)?;
            }
            if let Some(b) = &o.bank {
                // A bank event runs from the reading before this one, and
                // can be attributed to any pair of samples it overlaps.
                let opened: Option<f64> = tx
                    .query_row(
                        "SELECT MAX(at) FROM bank_sample WHERE account_id = ?1 AND at < ?2",
                        params![account, b.at],
                        |r| r.get(0),
                    )
                    .map_err(sql)?;
                match write_bank(&tx, account, b, false)? {
                    BankWrite::Written => {
                        banks_changed.push(account);
                        account_refold.push((account, opened.unwrap_or(b.at)));
                    }
                    // The same inventory as the last stored: kept back, but
                    // looked at again once this pass's resets are folded.
                    BankWrite::Unchanged => unchanged_banks.push((account, b.clone())),
                    BankWrite::Older => {}
                }
            }
            if let Some(p) = &o.plan {
                if let Some(at) = write_plan(&tx, account, p)? {
                    account_refold.push((account, at));
                }
                // The unified account's cycles are the ones drawn, so its
                // fold needs the plan too, or a tier change that leaves the
                // window alone reads there as a reset.
                if let Some(sk) = &spacexsi_account {
                    let unified = upsert_account(&tx, "spacexsi", sk, SPACEXSI_HEADING, now)?;
                    if let Some(at) = write_unified_plan(&tx, unified, &o.provider, p)? {
                        account_refold.push((unified, at));
                    }
                }
            }
            if let Some(w) = &o.watch {
                write_watch(&tx, account, w)?;
            }
            // Phase 4's seams: which included pools an overage pool serves,
            // and per-request charges - under the vendor and, on the unified
            // plan, under SpaceXSI as its lanes are. A link that is new folds
            // the overage pool's spans again from the start.
            overage::write_links_and_charges(&tx, account, o, &mut refold)?;
            if let (Some(sk), Some(u)) = (&spacexsi_account, &copy) {
                let unified = upsert_account(&tx, "spacexsi", sk, SPACEXSI_HEADING, now)?;
                overage::write_links_and_charges(&tx, unified, u, &mut refold)?;
            }
            // The Codex backfill is not written here: its rollouts can run to
            // gigabytes, so they are read off this transaction and written
            // by [`History::replay`] when they land (`backfill::Runner`).
        }
        for account in banks_changed {
            rebuild_bank_events(&tx, account)?;
        }
        for (account, at) in account_refold {
            for pool in pools_of(&tx, account)? {
                refold.push((pool, at));
            }
        }
        // A pool missing from this pass while its siblings answered live
        // may have been gone a whole window by now (§4): folded again, so
        // its open cycle closes as `pool_gone`.
        for (account, present) in answered {
            for (pool, last) in gone_candidates(&tx, account, &present)? {
                if gone_at(&tx, pool, t)?.is_some() {
                    refold.push((pool, last));
                }
            }
        }
        // An included pool folded again moves what its overage pools'
        // spans were exhausted against.
        overage::refold_linked(&tx, &mut refold)?;
        // One re-fold per pool, from its earliest point.
        refold.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        refold.dedup_by_key(|x| x.0);
        for (pool, from) in refold {
            refold_pool(&tx, pool, Some(from), t)?;
        }
        // An unchanged bank reading is the evidence an `issued` reset needs
        // (§5.6: the bank read on both sides with nothing spent), so one
        // that closes the bracket of a reset still `unknown` is kept, and
        // the account folded again from the reading before it. Otherwise
        // banks are written only on change (§3.7).
        for (account, b) in unchanged_banks {
            let last: Option<f64> = tx
                .query_row(
                    "SELECT MAX(at) FROM bank_sample WHERE account_id = ?1",
                    [account],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            let Some(last) = last.filter(|l| *l < b.at) else {
                continue;
            };
            let waiting: bool = tx
                .query_row(
                    "SELECT EXISTS (SELECT 1 FROM reset_event e JOIN pool p ON p.id = e.pool_id
                      WHERE p.account_id = ?1 AND e.kind = 'unknown' AND e.at_hi > ?2 AND e.at_hi <= ?3)",
                    params![account, last, b.at],
                    |r| r.get(0),
                )
                .map_err(sql)?;
            if waiting && write_bank(&tx, account, &b, true)? == BankWrite::Written {
                rebuild_bank_events(&tx, account)?;
                let mut again: Vec<(i64, f64)> = pools_of(&tx, account)?.into_iter().map(|p| (p, last)).collect();
                overage::refold_linked(&tx, &mut again)?;
                again.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
                again.dedup_by_key(|x| x.0);
                for (pool, from) in again {
                    refold_pool(&tx, pool, Some(from), t)?;
                }
            }
        }
        tx.commit().map_err(sql)
    }

    /// What `meta` holds under `k`.
    pub fn meta(&self, k: &str) -> Result<Option<String>, String> {
        self.conn
            .query_row("SELECT CAST(v AS TEXT) FROM meta WHERE k = ?1", [k], |r| {
                r.get::<_, Option<String>>(0)
            })
            .optional()
            .map(Option::flatten)
            .map_err(sql)
    }

    /// Replace what `meta` holds under `k` with what `f` makes of it, in one
    /// transaction, so two panes cannot both act on the same value. `f`
    /// returning `None` leaves it alone. True when it was replaced.
    pub fn meta_update(&mut self, k: &str, f: impl FnOnce(Option<&str>) -> Option<String>) -> Result<bool, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let Some(v) = f(meta_get(&tx, k)?.as_deref()) else {
            return Ok(false);
        };
        meta_set(&tx, k, &v)?;
        tx.commit().map_err(sql)?;
        Ok(true)
    }

    /// Write a backfill (§8) for one account: each replayed reading under
    /// the same write rule as a live one, so nothing already stored is
    /// written twice; [`backfill::done`] under its `meta` key; and a re-fold
    /// of every pool it wrote to, from its earliest new sample - all in one
    /// transaction. Returns how many backfilled samples the account holds
    /// now, which is what `meta` records: a re-run that finds nothing new
    /// still says what the history came from.
    pub fn replay(
        &mut self,
        provider: &str,
        account_key: &str,
        heading: &str,
        rows: &[backfill::Replayed],
        t: &Thresholds,
        now: f64,
    ) -> Result<u64, String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        refold_if_stale(&tx, t)?;
        let account = upsert_account(&tx, provider, account_key, heading, now)?;
        let mut refold: Vec<(i64, f64)> = Vec::new();
        for r in rows {
            let lane = Lane {
                label: r.label.clone(),
                pool_key: r.pool_key.clone(),
                unit: r.unit.clone(),
                role: r.role,
                ..Lane::default()
            };
            let pool = upsert_pool(&tx, account, &lane, r.sample.at)?;
            let mut sample = r.sample.clone();
            sample.source = Source::Backfill;
            if write_sample(&tx, pool, &sample)? {
                refold.push((pool, sample.at));
            }
        }
        let held: i64 = tx
            .query_row(
                "SELECT COUNT(*) FROM sample s JOIN pool p ON p.id = s.pool_id
                  WHERE p.account_id = ?1 AND s.source = 'backfill'",
                [account],
                |r| r.get(0),
            )
            .map_err(sql)?;
        let held = held.max(0) as u64;
        meta_set(&tx, &backfill::meta_key(account_key), &backfill::done(held))?;
        // As in `record`: an included pool folded again moves what its
        // overage pools' spans were exhausted against.
        overage::refold_linked(&tx, &mut refold)?;
        refold.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        refold.dedup_by_key(|x| x.0);
        for (pool, from) in refold {
            refold_pool(&tx, pool, Some(from), t)?;
        }
        tx.commit().map_err(sql)?;
        Ok(held)
    }

    /// The cycles stored for one pool, oldest first. For the tab and tests.
    pub fn cycles(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Vec<Cycle>, String> {
        let pool: Option<i64> = self
            .conn
            .query_row(
                "SELECT p.id FROM pool p JOIN account a ON a.id = p.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2 AND p.pool_key = ?3",
                params![provider, account_key, pool_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        match pool {
            Some(pool) => Ok(load_cycles(&self.conn, pool)?.into_iter().map(|(_, c)| c).collect()),
            None => Ok(Vec::new()),
        }
    }

    /// The samples stored for one pool, oldest first. For the tab and tests.
    pub fn samples(&self, provider: &str, account_key: &str, pool_key: &str) -> Result<Vec<Sample>, String> {
        let pool: Option<i64> = self
            .conn
            .query_row(
                "SELECT p.id FROM pool p JOIN account a ON a.id = p.account_id
                  WHERE a.provider = ?1 AND a.account_key = ?2 AND p.pool_key = ?3",
                params![provider, account_key, pool_key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        match pool {
            Some(pool) => load_samples(&self.conn, pool, None),
            None => Ok(Vec::new()),
        }
    }

    /// One `SELECT COUNT(*)`, for tests.
    pub fn count(&self, table: &str) -> i64 {
        self.conn
            .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap_or(-1)
    }
}

/// The `meta` key holding the account key a provider's profile was last
/// seen under with a stable id.
fn last_account(o: &Observation) -> String {
    let profile = if o.profile.is_empty() { "default" } else { o.profile.as_str() };
    format!("last_account:{}:{profile}", o.provider)
}

/// The key `o` is stored under: its own stable id where it has one. Where it
/// has none - a provider whose id comes in the live body, during an outage -
/// the account this provider's profile was last seen under, so the outage's
/// watch state lands on that account rather than on a `profile:` account
/// nobody has. Never invented: a provider never seen with an id keeps the
/// profile key it always had.
fn key_of(c: &Connection, o: &Observation) -> Result<String, String> {
    if o.has_stable_id() {
        return Ok(o.account_key());
    }
    let last: Option<String> = c
        .query_row("SELECT CAST(v AS TEXT) FROM meta WHERE k = ?1", [last_account(o)], |r| r.get(0))
        .optional()
        .map(Option::flatten)
        .map_err(sql)?;
    Ok(last.unwrap_or_else(|| o.account_key()))
}

/// The `meta` key holding the unified plan's key a vendor account shared
/// it under, the last time both vendors were read.
fn partner(o: &Observation, own: &str) -> String {
    format!("spacexsi_partner:{}:{own}", o.provider)
}

/// The key the unified plan's copy of `o` is written under: the one the
/// pass named, or for a shared account read without its partner this pass,
/// the one the two last shared. `None` when there is none to be had: the
/// copy is then not written, rather than started under a key of its own.
fn spacexsi_key(c: &Connection, o: &Observation, own: &str) -> Result<Option<String>, String> {
    if o.spacexsi_account.is_some() || o.spacexsi != Some(Spacexsi::Shared) {
        return Ok(o.spacexsi_account.clone());
    }
    c.query_row("SELECT CAST(v AS TEXT) FROM meta WHERE k = ?1", [partner(o, own)], |r| r.get(0))
        .optional()
        .map(Option::flatten)
        .map_err(sql)
}

/// What the derived tables were folded with: the thresholds, as text.
fn fingerprint(t: &Thresholds) -> String {
    format!("{t:?}")
}

/// The local zone's name, where this machine says one, and its offset now
/// (§3.6 `meta`): the history's times are epoch seconds, and a report that
/// groups them by day needs to know whose days.
fn local_zone() -> (String, String) {
    let name = std::env::var("TZ")
        .ok()
        .map(|z| z.trim_start_matches(':').to_string())
        .filter(|z| !z.is_empty())
        .or_else(|| {
            std::fs::read_link("/etc/localtime").ok().and_then(|p| {
                let p = p.to_string_lossy().into_owned();
                p.split_once("zoneinfo/").map(|(_, z)| z.to_string())
            })
        })
        .unwrap_or_else(|| "unknown".into());
    let offset = chrono::Local::now().format("%:z").to_string();
    (name, offset)
}

/// Whether the file was folded by another detector or other thresholds.
fn stale(c: &Connection, t: &Thresholds) -> Result<bool, String> {
    // Read as text, as `meta_set` writes it; anything unreadable is simply
    // not this detector.
    let get = |k: &str| -> Result<Option<String>, String> {
        c.query_row("SELECT CAST(v AS TEXT) FROM meta WHERE k = ?1", [k], |r| r.get(0))
            .optional()
            .map(Option::flatten)
            .map_err(sql)
    };
    let detector = get("detector_version")?.and_then(|v| v.parse::<i64>().ok());
    Ok(detector != Some(fold::DETECTOR_VERSION) || get("fold_thresholds")?.as_deref() != Some(&*fingerprint(t)))
}

/// Inside `tx`: fold every pool again with `t` when the file is [`stale`],
/// and stamp what it was folded with. Also keeps the local zone current.
fn refold_if_stale(tx: &Transaction, t: &Thresholds) -> Result<(), String> {
    let (zone, offset) = local_zone();
    if meta_get(tx, "tz")?.as_deref() != Some(&*zone) || meta_get(tx, "tz_offset")?.as_deref() != Some(&*offset) {
        meta_set(tx, "tz", &zone)?;
        meta_set(tx, "tz_offset", &offset)?;
    }
    if !stale(tx, t)? {
        return Ok(());
    }
    let pools: Vec<i64> = {
        let mut st = tx.prepare("SELECT id FROM pool ORDER BY id").map_err(sql)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };
    // The bank events first: they are the detector's reading of the stored
    // inventories too, and the write path rebuilds them only when an
    // inventory changes.
    let accounts: Vec<i64> = {
        let mut st = tx.prepare("SELECT DISTINCT account_id FROM bank_sample ORDER BY account_id").map_err(sql)?;
        let rows = st.query_map([], |r| r.get(0)).map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };
    for account in accounts {
        rebuild_bank_events(tx, account)?;
    }
    for pool in pools {
        refold_pool(tx, pool, None, t)?;
    }
    meta_set(tx, "detector_version", &fold::DETECTOR_VERSION.to_string())?;
    meta_set(tx, "fold_thresholds", &fingerprint(t))?;
    meta_set(tx, "writer", env!("CARGO_PKG_VERSION"))?;
    meta_set(
        tx,
        "clocks",
        "sample.at, bank_sample.at, plan_sample.at, watch_state.at: this machine's wall clock at fetch; reset_at: the server's",
    )
}

/// Apply every migration past the file's `user_version`, each in its own
/// `BEGIN IMMEDIATE`, reading the version again inside it so a pane that
/// lost the race does nothing.
fn migrate(conn: &Connection) -> Result<(), Failure> {
    loop {
        let v: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .map_err(Failure::from_sql)?;
        if v > SCHEMA_VERSION {
            return Err(Failure {
                error: OpenError::Newer(v),
                damaged: false,
                identity: None,
            });
        }
        if v == SCHEMA_VERSION {
            return Ok(());
        }
        conn.execute_batch("BEGIN IMMEDIATE").map_err(Failure::from_sql)?;
        let step = (|| -> Result<(), rusqlite::Error> {
            let v: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
            if v < SCHEMA_VERSION {
                conn.execute_batch(MIGRATIONS[v as usize])?;
                conn.execute_batch(&format!("PRAGMA user_version = {}", v + 1))?;
            }
            conn.execute_batch("COMMIT")
        })();
        if let Err(e) = step {
            let _ = conn.execute_batch("ROLLBACK");
            return Err(Failure::from_sql(e));
        }
    }
}

fn meta_get(tx: &Transaction, k: &str) -> Result<Option<String>, String> {
    tx.query_row("SELECT v FROM meta WHERE k = ?1", [k], |r| {
        r.get::<_, Option<String>>(0)
    })
    .optional()
    .map(Option::flatten)
    .map_err(sql)
}

fn meta_set(tx: &Transaction, k: &str, v: &str) -> Result<(), String> {
    tx.execute(
        "INSERT INTO meta(k, v) VALUES (?1, ?2) ON CONFLICT(k) DO UPDATE SET v = excluded.v",
        params![k, v],
    )
    .map(|_| ())
    .map_err(sql)
}

fn upsert_account(tx: &Transaction, provider: &str, key: &str, heading: &str, now: f64) -> Result<i64, String> {
    tx.query_row(
        "INSERT INTO account(provider, account_key, heading, first_seen, last_seen) VALUES (?1, ?2, ?3, ?4, ?4)
         ON CONFLICT(provider, account_key) DO UPDATE SET heading = excluded.heading,
             last_seen = MAX(last_seen, excluded.last_seen)
         RETURNING id",
        params![provider, key, heading, now],
        |r| r.get(0),
    )
    .map_err(sql)
}

/// The pool row for `lane`, made or brought up to date. `last_seen` moves
/// only on a live answer, and on every one, written or held back as
/// unchanged: it is the liveness [`gone_at`] measures absence from, which
/// the samples alone understate by up to the heartbeat.
fn upsert_pool(tx: &Transaction, account: i64, lane: &Lane, at: f64) -> Result<i64, String> {
    let (currency, exponent) = match &lane.unit {
        crate::shared::Unit::Money { currency, exponent } => (Some(currency.clone()), Some(*exponent)),
        _ => (None, None),
    };
    tx.query_row(
        "INSERT INTO pool(account_id, pool_key, label, role, unit, currency, exponent, first_seen, last_seen)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)
         ON CONFLICT(account_id, pool_key) DO UPDATE SET label = excluded.label, role = excluded.role,
             unit = excluded.unit, currency = excluded.currency, exponent = excluded.exponent,
             first_seen = MIN(first_seen, excluded.first_seen),
             last_seen = CASE WHEN ?9 THEN MAX(last_seen, excluded.last_seen) ELSE last_seen END
         RETURNING id",
        params![
            account,
            lane.pool_key,
            lane.label,
            lane.role.name(),
            lane.unit.name(),
            currency,
            exponent,
            at,
            !lane.stale
        ],
        |r| r.get(0),
    )
    .map_err(sql)
}

fn pool_id(tx: &Transaction, account: i64, key: &str) -> Result<Option<i64>, String> {
    tx.query_row(
        "SELECT id FROM pool WHERE account_id = ?1 AND pool_key = ?2",
        params![account, key],
        |r| r.get(0),
    )
    .optional()
    .map_err(sql)
}

fn pools_of(tx: &Transaction, account: i64) -> Result<Vec<i64>, String> {
    let mut st = tx.prepare("SELECT id FROM pool WHERE account_id = ?1").map_err(sql)?;
    let rows = st.query_map([account], |r| r.get(0)).map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

const SAMPLE_COLS: &str = "at, pct, used_amount, limit_amount, window_secs, reset_at, source, spacexsi";

fn sample_row(r: &rusqlite::Row) -> rusqlite::Result<Sample> {
    Ok(Sample {
        at: r.get(0)?,
        pct: r.get(1)?,
        used: r.get(2)?,
        limit: r.get(3)?,
        window_secs: r.get(4)?,
        reset_at: r.get(5)?,
        source: Source::from_name(&r.get::<_, String>(6)?),
        spacexsi: r.get(7)?,
    })
}

#[cfg(test)]
thread_local! {
    /// How many samples [`load_samples`] has read on this thread, so a test
    /// can bound what one write costs.
    static SAMPLES_READ: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// A pool's samples after `after` (all of them for `None`), oldest first.
fn load_samples(conn: &Connection, pool: i64, after: Option<f64>) -> Result<Vec<Sample>, String> {
    let mut st = conn
        .prepare(&format!(
            "SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at > ?2 ORDER BY at"
        ))
        .map_err(sql)?;
    let rows = st
        .query_map(params![pool, after.unwrap_or(f64::NEG_INFINITY)], sample_row)
        .map_err(sql)?;
    let out: Vec<Sample> = rows.collect::<Result<_, _>>().map_err(sql)?;
    #[cfg(test)]
    SAMPLES_READ.with(|n| n.set(n.get() + out.len()));
    Ok(out)
}

/// Write one lane per pool, under the five-minute and change-or-heartbeat
/// rules, noting which pools need folding from where.
///
/// `seen` is this pane's memory of the live readings it was handed and did
/// not write because nothing had changed; see [`bridge`].
fn write_lanes(
    tx: &Transaction,
    account: i64,
    lanes: &[Lane],
    spacexsi: Option<&str>,
    seen: &mut Seen,
    watch_gap: f64,
    refold: &mut Vec<(i64, f64)>,
) -> Result<(), String> {
    for lane in lanes {
        // A projected window's figure belongs to a window that has closed
        // (§4); a lane with no fetch time or no key cannot be placed.
        if lane.projected || lane.fetched_at <= 0.0 || lane.pool_key.is_empty() {
            continue;
        }
        let pool = upsert_pool(tx, account, lane, lane.fetched_at)?;
        let sample = Sample {
            at: lane.fetched_at,
            pct: lane.pct.is_finite().then_some(lane.pct),
            used: lane.used,
            limit: lane.limit,
            window_secs: lane.window_secs,
            reset_at: lane.reset,
            source: if lane.stale { Source::Cached } else { Source::Live },
            spacexsi: spacexsi.map(str::to_string),
        };
        let (before, after) = neighbours(tx, pool, sample.at)?;
        if super::should_write(before.as_ref(), after.as_ref(), &sample) {
            if let Some(r) = bridge(seen.get(&pool), before.as_ref(), after.as_ref(), &sample, watch_gap) {
                if insert_sample(tx, pool, &r)? {
                    refold.push((pool, r.at));
                }
            }
            seen.remove(&pool);
            if insert_sample(tx, pool, &sample)? {
                refold.push((pool, sample.at));
            }
        } else if let (Some(b), None) = (&before, &after) {
            // Held back as unchanged: remembered, in case the next change
            // needs it to show the pane was watching.
            if sample.source == Source::Live && sample.at > b.at && !super::changed(b, &sample) {
                let kept = seen.entry(pool).or_default();
                kept.retain(|r| sample.at - r.at <= watch_gap);
                if kept.len() >= SEEN_MOST {
                    kept.remove(0);
                }
                kept.push(sample);
            }
        }
    }
    Ok(())
}

/// Per pool, the live readings this pane held back as unchanged since the
/// last one written, newest last.
pub(super) type Seen = HashMap<i64, Vec<Sample>>;

/// At most this many held-back readings are remembered per pool: enough
/// for a watch gap's worth of polls, whatever the poll.
const SEEN_MOST: usize = 64;

/// The held-back reading to write before `new`, when there is one.
///
/// Unchanged readings are not written until the hourly heartbeat (§3.7), so
/// a pool watched through a flat stretch stores samples further apart than
/// the watch gap, and the change that ends the stretch would be compared
/// with the stretch's first reading as if nothing had been watching -
/// offline - where a 20-point fall counts as a reset (§5.3). The pane did
/// see the readings in between, so the latest of them that keeps the
/// five-minute spacing from both neighbours, and is within the watch gap of
/// `new`, is written first: a real reading, at its own fetch time, with the
/// figures it had. A pane that saw nothing in between writes nothing, and
/// the pair stays offline.
fn bridge(seen: Option<&Vec<Sample>>, before: Option<&Sample>, after: Option<&Sample>, new: &Sample, watch_gap: f64) -> Option<Sample> {
    let b = before?;
    if after.is_some() || new.source != Source::Live || new.at - b.at <= watch_gap {
        return None;
    }
    seen?
        .iter()
        .filter(|r| r.at >= b.at + super::MIN_GAP && r.at <= new.at - super::MIN_GAP && new.at - r.at <= watch_gap)
        .max_by(|x, y| x.at.total_cmp(&y.at))
        .cloned()
}

/// Insert `s` when [`super::should_write`] says so against the stored
/// samples either side of it. True when it was written.
fn write_sample(tx: &Transaction, pool: i64, s: &Sample) -> Result<bool, String> {
    let (before, after) = neighbours(tx, pool, s.at)?;
    if !super::should_write(before.as_ref(), after.as_ref(), s) {
        return Ok(false);
    }
    insert_sample(tx, pool, s)
}

/// The stored samples of `pool` at or before `at`, and after it.
fn neighbours(tx: &Transaction, pool: i64, at: f64) -> Result<(Option<Sample>, Option<Sample>), String> {
    let before: Option<Sample> = tx
        .query_row(
            &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at <= ?2 ORDER BY at DESC LIMIT 1"),
            params![pool, at],
            sample_row,
        )
        .optional()
        .map_err(sql)?;
    let after: Option<Sample> = tx
        .query_row(
            &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at > ?2 ORDER BY at LIMIT 1"),
            params![pool, at],
            sample_row,
        )
        .optional()
        .map_err(sql)?;
    Ok((before, after))
}

/// Insert `s` as it is. True when it was new.
fn insert_sample(tx: &Transaction, pool: i64, s: &Sample) -> Result<bool, String> {
    let n = tx
        .execute(
            &format!(
                "INSERT OR IGNORE INTO sample(pool_id, {SAMPLE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
            ),
            params![
                pool,
                s.at,
                s.pct,
                s.used,
                s.limit,
                s.window_secs,
                s.reset_at,
                s.source.name(),
                s.spacexsi
            ],
        )
        .map_err(sql)?;
    Ok(n > 0)
}

/// What [`write_bank`] did with a reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum BankWrite {
    Written,
    /// Newer than the latest stored, and the same inventory.
    Unchanged,
    /// At or before the latest stored.
    Older,
}

/// Write a bank reading when it is newer than the latest one stored and
/// differs from it - or, with `unchanged_too`, when it is newer at all.
fn write_bank(tx: &Transaction, account: i64, b: &BankReading, unchanged_too: bool) -> Result<BankWrite, String> {
    let last: Option<(f64, i64, Option<String>)> = tx
        .query_row(
            "SELECT at, left, credits FROM bank_sample WHERE account_id = ?1 ORDER BY at DESC LIMIT 1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    let credits = b.credits_json();
    if let Some((at, left, was)) = &last {
        if b.at <= *at {
            return Ok(BankWrite::Older);
        }
        if !unchanged_too && *left as u64 == b.left && *was == credits {
            return Ok(BankWrite::Unchanged);
        }
    }
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO bank_sample(account_id, at, left, credits) VALUES (?1, ?2, ?3, ?4)",
            params![account, b.at, b.left as i64, credits],
        )
        .map_err(sql)?;
    Ok(if n > 0 { BankWrite::Written } else { BankWrite::Older })
}

/// Write the plan when it differs from the latest one stored; the time it
/// was written at, when it was.
fn write_plan(tx: &Transaction, account: i64, p: &super::PlanReading) -> Result<Option<f64>, String> {
    let last: Option<(f64, String)> = tx
        .query_row(
            "SELECT at, plan FROM plan_sample WHERE account_id = ?1 ORDER BY at DESC LIMIT 1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    if p.plan.is_empty() || last.as_ref().is_some_and(|(at, plan)| *plan == p.plan || p.at <= *at) {
        return Ok(None);
    }
    tx.execute(
        "INSERT OR IGNORE INTO plan_sample(account_id, at, plan) VALUES (?1, ?2, ?3)",
        params![account, p.at, p.plan],
    )
    .map_err(sql)?;
    Ok(Some(p.at))
}

/// Write a vendor's plan reading under the unified SpaceXSI account, as
/// `vendor:plan`. Cursor and Grok both report there, each its own tier, so
/// a reading is compared with the latest of the same vendor only: compared
/// with the latest row of either, the two would take turns and every pass
/// would look like a plan change. [`load_plans`] puts them back together.
fn write_unified_plan(
    tx: &Transaction,
    account: i64,
    vendor: &str,
    p: &super::PlanReading,
) -> Result<Option<f64>, String> {
    if p.plan.is_empty() {
        return Ok(None);
    }
    let prefix = format!("{vendor}:");
    let last: Option<(f64, String)> = tx
        .query_row(
            "SELECT at, plan FROM plan_sample WHERE account_id = ?1 AND substr(plan, 1, length(?2)) = ?2
             ORDER BY at DESC LIMIT 1",
            params![account, prefix],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    let plan = format!("{prefix}{}", p.plan);
    if last.as_ref().is_some_and(|(at, was)| *was == plan || p.at <= *at) {
        return Ok(None);
    }
    // Both vendors can be read in the same instant, and the table holds one
    // row per moment: the later of the two goes in the next representable
    // instant, which orders it and moves nothing a reading can see.
    let mut at = p.at;
    for _ in 0..4 {
        let wrote = tx
            .execute(
                "INSERT OR IGNORE INTO plan_sample(account_id, at, plan) VALUES (?1, ?2, ?3)",
                params![account, at, plan],
            )
            .map_err(sql)?;
        if wrote > 0 {
            return Ok(Some(at));
        }
        at = f64::from_bits(at.to_bits() + 1);
    }
    Ok(None)
}

/// Compose the unified account's `vendor:plan` rows into one plan per
/// moment: every vendor's latest tier so far, so a row from one vendor that
/// leaves its own tier alone is no change, and either vendor's change is.
fn compose_unified_plans(rows: Vec<PlanChange>) -> Vec<PlanChange> {
    // Each vendor starts at its first tier, so its first reading is the
    // start of what is known about it rather than a change.
    let mut now: std::collections::BTreeMap<String, String> = Default::default();
    for r in &rows {
        let (vendor, plan) = r.plan.split_once(':').unwrap_or(("", r.plan.as_str()));
        now.entry(vendor.to_string()).or_insert_with(|| plan.to_string());
    }
    let mut out: Vec<PlanChange> = Vec::new();
    // Both vendors read in one instant are stored a representable instant
    // apart (`write_unified_plan`): they are one moment, composed once and
    // dated by the true instant. Otherwise the sample at that instant saw
    // only the first vendor's change, and the next closed the cycle again
    // a few ulps later.
    let same = |a: f64, b: f64| (b - a).abs() <= SAME_PLAN_INSTANT;
    let mut i = 0;
    while i < rows.len() {
        let at = rows[i].at;
        while i < rows.len() && same(at, rows[i].at) {
            let (vendor, plan) = rows[i].plan.split_once(':').unwrap_or(("", rows[i].plan.as_str()));
            now.insert(vendor.to_string(), plan.to_string());
            i += 1;
        }
        let joined = now.iter().map(|(v, p)| format!("{v}:{p}")).collect::<Vec<_>>().join(" ");
        if out.last().is_none_or(|l| l.plan != joined) {
            out.push(PlanChange { at, plan: joined });
        }
    }
    out
}

/// How far apart two vendors' plan rows of one instant can be stored: the
/// few representable steps `write_unified_plan` moves the later one by.
/// Far under any poll, so two real readings are never merged by it.
const SAME_PLAN_INSTANT: f64 = 1e-3;

/// Write the watch state when it differs from the latest one stored.
fn write_watch(tx: &Transaction, account: i64, w: &WatchReading) -> Result<(), String> {
    let last: Option<(f64, String, Option<String>)> = tx
        .query_row(
            "SELECT at, state, reason FROM watch_state WHERE account_id = ?1 ORDER BY at DESC LIMIT 1",
            [account],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()
        .map_err(sql)?;
    let reason = (!w.reason.is_empty()).then(|| w.reason.clone());
    if let Some((at, state, was)) = &last {
        if w.at <= *at || (*state == w.state && *was == reason) {
            return Ok(());
        }
    }
    tx.execute(
        "INSERT OR IGNORE INTO watch_state(account_id, at, state, reason) VALUES (?1, ?2, ?3, ?4)",
        params![account, w.at, w.state, reason],
    )
    .map_err(sql)?;
    Ok(())
}

fn load_plans(conn: &Connection, account: i64) -> Result<Vec<PlanChange>, String> {
    let mut st = conn
        .prepare("SELECT at, plan FROM plan_sample WHERE account_id = ?1 ORDER BY at")
        .map_err(sql)?;
    let rows = st
        .query_map([account], |r| {
            Ok(PlanChange {
                at: r.get(0)?,
                plan: r.get(1)?,
            })
        })
        .map_err(sql)?;
    let rows: Vec<PlanChange> = rows.collect::<Result<_, _>>().map_err(sql)?;
    let provider: Option<String> = conn
        .query_row("SELECT provider FROM account WHERE id = ?1", [account], |r| r.get(0))
        .optional()
        .map_err(sql)?;
    Ok(if provider.as_deref() == Some("spacexsi") { compose_unified_plans(rows) } else { rows })
}

fn load_banks(conn: &Connection, account: i64) -> Result<Vec<BankReading>, String> {
    let mut st = conn
        .prepare("SELECT at, left, credits FROM bank_sample WHERE account_id = ?1 ORDER BY at")
        .map_err(sql)?;
    let rows = st
        .query_map([account], |r| {
            let at: f64 = r.get(0)?;
            let left: i64 = r.get(1)?;
            let credits: Option<String> = r.get(2)?;
            Ok(BankReading::from_row(at, left.max(0) as u64, credits.as_deref()))
        })
        .map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// Replace an account's bank events with what [`resets::bank_events`] finds
/// in all its bank readings.
fn rebuild_bank_events(tx: &Transaction, account: i64) -> Result<(), String> {
    let banks = load_banks(tx, account)?;
    tx.execute("DELETE FROM bank_event WHERE account_id = ?1", [account])
        .map_err(sql)?;
    for e in resets::bank_events(&banks) {
        tx.execute(
            "INSERT OR IGNORE INTO bank_event(account_id, at_lo, at_hi, kind, count, expires_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![account, e.at_lo, e.at_hi, e.kind.name(), e.count, e.expires_at],
        )
        .map_err(sql)?;
    }
    Ok(())
}

fn model_of(name: &str) -> CycleModel {
    match name {
        "use_anchored" => CycleModel::UseAnchored,
        "rolling" => CycleModel::Rolling,
        "undated" => CycleModel::Undated,
        _ => CycleModel::Contiguous,
    }
}

fn source_of(name: &str) -> StartSource {
    match name {
        "derived" => StartSource::Derived,
        "inferred" => StartSource::Inferred,
        "reset" => StartSource::Reset,
        _ => StartSource::Server,
    }
}

fn reason_of(name: &str) -> Option<EndReason> {
    Some(match name {
        "scheduled" => EndReason::Scheduled,
        "reset" => EndReason::Reset,
        "window_changed" => EndReason::WindowChanged,
        "plan_changed" => EndReason::PlanChanged,
        "limit_changed" => EndReason::LimitChanged,
        "pool_gone" => EndReason::PoolGone,
        _ => return None,
    })
}

/// A pool's cycles with their row ids, oldest first. Segments are not
/// loaded: nothing that reads cycles back needs them yet.
fn load_cycles(conn: &Connection, pool: i64) -> Result<Vec<(i64, Cycle)>, String> {
    load_cycles_from(conn, pool, 0)
}

/// A pool's cycles from ordinal `from` on, oldest first.
fn load_cycles_from(conn: &Connection, pool: i64, from: i64) -> Result<Vec<(i64, Cycle)>, String> {
    let mut st = conn
        .prepare(
            "SELECT id, ordinal, starts_at, ends_at, closed_at, window_secs, cycle_model, start_source, end_reason,
                    plan, peak_pct, consumed_pct, allowances, available_pct, end_is_lower_bound, used_amount,
                    limit_amount, exhausted_at, first_obs_at, last_obs_at, coverage,
                    -- Derived, not a column (§5.7): a reset filed under this
                    -- cycle with no known figure before it - or, filed under
                    -- none, one that began inside it.
                    EXISTS (SELECT 1 FROM reset_event e
                             WHERE e.pool_id = cycle.pool_id
                               AND (e.cycle_id = cycle.id
                                    OR (e.cycle_id IS NULL AND e.at_lo >= cycle.starts_at
                                        AND (cycle.closed_at IS NULL OR e.at_lo < cycle.closed_at)))
                               AND json_extract(e.evidence, '$.before_known') = 0)
               FROM cycle WHERE pool_id = ?1 AND ordinal >= ?2 ORDER BY ordinal",
        )
        .map_err(sql)?;
    let rows = st
        .query_map(params![pool, from], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                Cycle {
                    ordinal: r.get::<_, i64>(1)? as u32,
                    starts_at: r.get(2)?,
                    ends_at: r.get(3)?,
                    closed_at: r.get(4)?,
                    window_secs: r.get(5)?,
                    model: model_of(&r.get::<_, String>(6)?),
                    start_source: source_of(&r.get::<_, String>(7)?),
                    end_reason: r.get::<_, Option<String>>(8)?.as_deref().and_then(reason_of),
                    plan: r.get(9)?,
                    peak_pct: r.get(10)?,
                    consumed_pct: r.get(11)?,
                    allowances: r.get::<_, i64>(12)? as u32,
                    available_pct: r.get(13)?,
                    end_is_lower_bound: r.get::<_, i64>(14)? != 0,
                    used_amount: r.get(15)?,
                    limit_amount: r.get(16)?,
                    exhausted_at: r.get(17)?,
                    first_obs_at: r.get(18)?,
                    last_obs_at: r.get(19)?,
                    coverage: r.get::<_, Option<f64>>(20)?.unwrap_or(0.0),
                    consumed_is_lower_bound: r.get::<_, i64>(21)? != 0,
                    segments: Vec::new(),
                },
            ))
        })
        .map_err(sql)?;
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// The included pools of `account` with an open cycle that did not answer
/// live this pass (`present` is the keys that did), and whose last
/// observation is at least a window old: each with that observation.
fn gone_candidates(tx: &Transaction, account: i64, present: &[String]) -> Result<Vec<(i64, f64)>, String> {
    let mut st = tx
        .prepare(
            "SELECT p.id, p.pool_key, c.last_obs_at, c.window_secs FROM pool p JOIN cycle c ON c.pool_id = p.id
              WHERE p.account_id = ?1 AND p.role = 'included' AND c.closed_at IS NULL
                AND c.window_secs > 0 AND c.last_obs_at IS NOT NULL",
        )
        .map_err(sql)?;
    let rows = st
        .query_map([account], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?)))
        .map_err(sql)?;
    let mut out = Vec::new();
    for row in rows {
        let (pool, key, last, _) = row.map_err(sql)?;
        if !present.contains(&key) {
            out.push((pool, last));
        }
    }
    Ok(out)
}

/// When `pool`'s open cycle closes as `pool_gone` (§4), if it does: the
/// pool missing from live readings for a whole window - its cycle's - while
/// a sibling included pool of the same account went on answering live, with
/// no gap in the siblings' live readings longer than [`fold::COVERAGE_GAP`]
/// from the pool's last live reading to a window after it. So a pane that
/// was asleep, fetches that failed, or readings that were only cached never
/// make one: none of them leaves live sibling readings through the window.
/// A pool last read at about zero is idle, not gone - Claude leaves an
/// unused limit out of its reading - and an undated pool has no window to
/// be missing for. It closes at its stated end where that comes first,
/// else a window after its last live reading. Worked out from the stored
/// readings alone, so a full re-fold reaches the same answer - and a pool
/// that answers live again is judged again with that reading, so its cycle
/// then closes as the fold reads the readings either side, on schedule.
///
/// The last live answer is the later of the last live sample and the
/// pool's `last_seen`: an answer that changed nothing is held back from
/// the samples until the hourly heartbeat, and measuring from the sample
/// alone closed the cycle up to an hour early. `last_seen` is stored, not
/// derived, so a full re-fold reads the same.
fn gone_at(tx: &Transaction, pool: i64, t: &Thresholds) -> Result<Option<f64>, String> {
    // From the samples only, never from the cycles this fold is about to
    // replace: the pool's last live reading, its window and stated end.
    let (account, role): (i64, String) = tx
        .query_row("SELECT account_id, role FROM pool WHERE id = ?1", [pool], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(sql)?;
    if Role::from_name(&role) != Role::Included {
        return Ok(None);
    }
    let last: Option<(f64, Option<f64>, Option<f64>, Option<f64>)> = tx
        .query_row(
            "SELECT at, pct, window_secs, reset_at FROM sample WHERE pool_id = ?1 AND source != 'cached'
              ORDER BY at DESC LIMIT 1",
            [pool],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((sampled, pct, window, ends_at)) = last else {
        return Ok(None);
    };
    let seen: f64 = tx
        .query_row("SELECT last_seen FROM pool WHERE id = ?1", [pool], |r| r.get(0))
        .map_err(sql)?;
    let last = sampled.max(seen);
    let Some(window) = window.filter(|w| *w > 0.0) else {
        return Ok(None);
    };
    if pct.is_none_or(|p| p <= t.near_zero_pct) {
        return Ok(None);
    }
    let until = last + window;
    // The siblings' live readings from the last one through a window after
    // it, as the largest gap between them and the latest of them.
    let (gap, latest): (Option<f64>, Option<f64>) = tx
        .query_row(
            "SELECT MAX(gap), MAX(at) FROM (
                SELECT at, at - LAG(at, 1, ?2) OVER (ORDER BY at) AS gap FROM (
                    SELECT DISTINCT s.at FROM sample s JOIN pool p ON p.id = s.pool_id
                     WHERE p.account_id = ?1 AND p.id != ?3 AND p.role = 'included' AND s.source != 'cached'
                       AND s.at > ?2 AND s.at <= ?4))",
            params![account, last, pool, until + fold::COVERAGE_GAP],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(sql)?;
    let watched = matches!((gap, latest), (Some(g), Some(l)) if g <= fold::COVERAGE_GAP && l >= until);
    if !watched {
        return Ok(None);
    }
    Ok(Some(ends_at.filter(|e| *e > last && *e < until).unwrap_or(until)))
}

/// Whether the reading that closed `c` - the first after its last
/// observation - is in a rolling stretch, as [`fold::rolling_mask`] reads
/// the readings from it on. A run is established, if ever, by the first
/// reading more than two tolerances after its start, so only the readings
/// up to that one are read.
fn closed_by_rolling(tx: &Transaction, pool: i64, c: &Cycle, t: &Thresholds) -> Result<bool, String> {
    let Some(last) = c.last_obs_at else {
        return Ok(false);
    };
    let closing: Option<Sample> = tx
        .query_row(
            &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at > ?2 ORDER BY at LIMIT 1"),
            params![pool, last],
            sample_row,
        )
        .optional()
        .map_err(sql)?;
    let Some(closing) = closing else {
        return Ok(false);
    };
    if closing.reset_at.is_none() {
        return Ok(false);
    }
    let reach = closing.at + 2.0 * fold::tolerance(closing.window_secs);
    let mut st = tx
        .prepare(&format!(
            "SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at >= ?2
               AND at <= (SELECT COALESCE(MIN(at), ?3) FROM sample WHERE pool_id = ?1 AND at > ?3)
             ORDER BY at"
        ))
        .map_err(sql)?;
    let rows = st.query_map(params![pool, closing.at, reach], sample_row).map_err(sql)?;
    let run: Vec<Sample> = rows.collect::<Result<_, _>>().map_err(sql)?;
    Ok(fold::rolling_mask(&run, t.near_zero_pct, None).first().copied().unwrap_or(false))
}

/// The pool's latest cycle, when it is undated and still open and nothing
/// after its last observation states an instant, as a [`fold::Resume`] to
/// fold on from: what an append to it needs, without its earlier readings.
fn open_undated(tx: &Transaction, pool: i64) -> Result<Option<fold::Resume>, String> {
    let open: Option<(i64, i64)> = tx
        .query_row(
            "SELECT id, ordinal FROM cycle
              WHERE pool_id = ?1 AND closed_at IS NULL AND cycle_model = 'undated' AND last_obs_at IS NOT NULL
                AND ordinal = (SELECT MAX(ordinal) FROM cycle WHERE pool_id = ?1)",
            [pool],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()
        .map_err(sql)?;
    let Some((id, ordinal)) = open else {
        return Ok(None);
    };
    let Some((_, mut cycle)) = load_cycles_from(tx, pool, ordinal)?.into_iter().next() else {
        return Ok(None);
    };
    let Some(last) = cycle.last_obs_at else {
        return Ok(None);
    };
    // A reading that states an instant turns the pool dated: folded whole.
    let dated: bool = tx
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM sample WHERE pool_id = ?1 AND at > ?2 AND reset_at IS NOT NULL)",
            params![pool, last],
            |r| r.get(0),
        )
        .map_err(sql)?;
    if dated {
        return Ok(None);
    }
    cycle.segments = {
        let mut st = tx
            .prepare("SELECT n, starts_at, ends_at, peak_pct FROM segment WHERE cycle_id = ?1 ORDER BY n")
            .map_err(sql)?;
        let rows = st
            .query_map([id], |r| {
                Ok(Segment {
                    n: r.get::<_, i64>(0)? as u32,
                    starts_at: r.get(1)?,
                    ends_at: r.get(2)?,
                    peak_pct: r.get(3)?,
                })
            })
            .map_err(sql)?;
        rows.collect::<Result<_, _>>().map_err(sql)?
    };
    let span = last - cycle.starts_at;
    let covered = if span > 0.0 { cycle.coverage * span } else { 0.0 };
    let latest = |col: &str| -> Result<Option<f64>, String> {
        tx.query_row(
            &format!(
                "SELECT {col} FROM sample WHERE pool_id = ?1 AND at <= ?2 AND source != 'cached' AND {col} IS NOT NULL
                  ORDER BY at DESC LIMIT 1"
            ),
            params![pool, last],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)
    };
    Ok(Some(fold::Resume {
        last_pct: latest("pct")?,
        last_used: latest("used_amount")?,
        covered,
        cycle,
    }))
}

fn insert_cycle(tx: &Transaction, pool: i64, c: &Cycle) -> Result<i64, String> {
    let id: i64 = tx
        .query_row(
            "INSERT INTO cycle(pool_id, ordinal, starts_at, ends_at, closed_at, window_secs, cycle_model, start_source,
                    end_reason, plan, peak_pct, consumed_pct, allowances, available_pct, end_is_lower_bound,
                    used_amount, limit_amount, exhausted_at, first_obs_at, last_obs_at, coverage)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)
             RETURNING id",
            params![
                pool,
                c.ordinal,
                c.starts_at,
                c.ends_at,
                c.closed_at,
                c.window_secs,
                c.model.name(),
                c.start_source.name(),
                c.end_reason.map(EndReason::name),
                c.plan,
                c.peak_pct,
                c.consumed_pct,
                c.allowances,
                c.available_pct,
                c.end_is_lower_bound as i64,
                c.used_amount,
                c.limit_amount,
                c.exhausted_at,
                c.first_obs_at,
                c.last_obs_at,
                c.coverage,
            ],
            |r| r.get(0),
        )
        .map_err(sql)?;
    for s in &c.segments {
        tx.execute(
            "INSERT INTO segment(cycle_id, n, starts_at, ends_at, peak_pct) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id, s.n, s.starts_at, s.ends_at, s.peak_pct],
        )
        .map_err(sql)?;
    }
    Ok(id)
}

/// Fold one pool again from the cycle before `from` (from the start for
/// `None`), replacing every derived row after the cycles it keeps (§3.5).
///
/// Kept: the closed, observed cycles whose last observation is before the
/// second-last sample before `from` - so the sample that closed each of
/// them is already behind the point, and a sample just before a boundary,
/// which can move it, is folded again - and the hollow cycles between them.
/// Everything after is deleted and rebuilt from the samples after the last
/// kept observation.
///
/// Bounded, so a write does not re-read a pool's whole history: a balance
/// has no cycle, and folds on from the sample before `from`; a pool that has
/// only ever been rolling has no cycle either, and folds on from its last
/// reading as a seed of its rolling stretch; and an overage or balance
/// pool's linked included pools are read only over the stretch re-folded;
/// and an undated pool's open cycle - which may be its only one, and never
/// close - is carried on from its last observation ([`open_undated`]).
fn refold_pool(tx: &Transaction, pool: i64, from: Option<f64>, t: &Thresholds) -> Result<(), String> {
    let (account, role, provider): (i64, String, String) = tx
        .query_row(
            "SELECT p.account_id, p.role, a.provider FROM pool p JOIN account a ON a.id = p.account_id WHERE p.id = ?1",
            [pool],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql)?;
    let role = Role::from_name(&role);
    // The last sample before `from`: what the point is folded on from.
    let s_prev: Option<f64> = match from {
        None => None,
        Some(from) => tx
            .query_row(
                "SELECT MAX(at) FROM sample WHERE pool_id = ?1 AND at < ?2",
                params![pool, from],
                |r| r.get(0),
            )
            .map_err(sql)?,
    };
    // The latest closed, observed cycle whose last observation is before
    // the second-last sample before `from`: one query, so a pool with years
    // of five-hour cycles does not load them all on every write. Two
    // samples, not one: the sample that closed it is behind the point, and
    // a new reading can make the two before it the end of a rolling stretch
    // (`fold::rolling_mask`), which a kept cycle must not have observed.
    let s_prev2: Option<f64> = match from {
        None => None,
        Some(from) => tx
            .query_row(
                "SELECT at FROM sample WHERE pool_id = ?1 AND at < ?2 ORDER BY at DESC LIMIT 1 OFFSET 1",
                params![pool, from],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?,
    };
    let kept: Option<i64> = match s_prev2 {
        Some(at) if role != Role::Balance => tx
            .query_row(
                "SELECT ordinal FROM cycle
                  WHERE pool_id = ?1 AND closed_at IS NOT NULL AND last_obs_at IS NOT NULL AND last_obs_at < ?2
                  ORDER BY ordinal DESC LIMIT 1",
                params![pool, at],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?,
        _ => None,
    };
    let prev: Option<Cycle> = match kept {
        Some(ordinal) => load_cycles_from(tx, pool, ordinal)?.into_iter().next().map(|(_, c)| c),
        None => None,
    };
    // A kept cycle is kept for the reading that closed it. When that
    // reading has since become part of a rolling stretch - its run only
    // confirmed by readings that came later - a fresh fold would not see
    // it, and the cycle it closed may not close at all: fold it again, from
    // the closed cycle before it. One step back, not the whole history: a
    // pool that went rolling for good after its last fixed cycle would
    // otherwise fold from its first reading on every write.
    let prev = match prev {
        Some(c) if closed_by_rolling(tx, pool, &c, t)? => {
            let before: Option<i64> = tx
                .query_row(
                    "SELECT ordinal FROM cycle
                      WHERE pool_id = ?1 AND closed_at IS NOT NULL AND last_obs_at IS NOT NULL AND ordinal < ?2
                      ORDER BY ordinal DESC LIMIT 1",
                    params![pool, c.ordinal],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?;
            let earlier = match before {
                Some(o) => load_cycles_from(tx, pool, o)?.into_iter().next().map(|(_, c)| c),
                None => None,
            };
            match earlier {
                Some(e) if !closed_by_rolling(tx, pool, &e, t)? => Some(e),
                _ => None,
            }
        }
        other => other,
    };
    let keep_ordinal = prev.as_ref().map_or(0, |c| c.ordinal as i64);
    let kept_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM cycle WHERE pool_id = ?1 AND ordinal = ?2",
            params![pool, keep_ordinal],
            |r| r.get(0),
        )
        .optional()
        .map_err(sql)?;
    let any_cycle: bool = tx
        .query_row("SELECT EXISTS (SELECT 1 FROM cycle WHERE pool_id = ?1)", [pool], |r| r.get(0))
        .map_err(sql)?;
    let at_prev: Option<Sample> = match s_prev {
        Some(at) => tx
            .query_row(
                &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at = ?2"),
                params![pool, at],
                sample_row,
            )
            .optional()
            .map_err(sql)?,
        None => None,
    };
    // A dated pool folded before with no cycle at all has only ever been
    // rolling: every reading so far is in its rolling stretch. Only for an
    // append - a reading that lands inside the stretch can break it there,
    // and then the readings before it are not the stretch's after all.
    let appended: bool = match from {
        Some(from) => !tx
            .query_row(
                "SELECT EXISTS (SELECT 1 FROM sample WHERE pool_id = ?1 AND at > ?2)",
                params![pool, from],
                |r| r.get::<_, bool>(0),
            )
            .map_err(sql)?,
        None => false,
    };
    let seed: Option<Sample> = at_prev
        .filter(|s| appended && prev.is_none() && !any_cycle && role != Role::Balance && s.reset_at.is_some());
    // An undated pool's open cycle, carried on from rather than folded
    // again from its first reading, when this write only adds readings
    // after it.
    let resume: Option<fold::Resume> = match appended && role != Role::Balance {
        true => open_undated(tx, pool)?,
        false => None,
    };
    let (prev, keep_ordinal, kept_id) = match &resume {
        Some(r) => (None, r.cycle.ordinal as i64 - 1, None),
        None => (prev, keep_ordinal, kept_id),
    };
    let after = match (&prev, role) {
        _ if resume.is_some() => resume.as_ref().and_then(|r| r.cycle.last_obs_at),
        (Some(c), _) => c.last_obs_at,
        // A balance's moves are between consecutive readings: the one
        // before the point opens the first.
        (None, Role::Balance) => s_prev,
        (None, _) => seed.as_ref().map(|s| s.at),
    };
    // Before the open cycle is deleted below: it is what this reads.
    let gone_close = gone_at(tx, pool, t)?;
    let gone = "SELECT id FROM cycle WHERE pool_id = ?1 AND ordinal > ?2";
    tx.execute(
        &format!("DELETE FROM segment WHERE cycle_id IN ({gone})"),
        params![pool, keep_ordinal],
    )
    .map_err(sql)?;
    tx.execute(
        &format!("UPDATE reset_event SET cycle_id = NULL WHERE cycle_id IN ({gone})"),
        params![pool, keep_ordinal],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM cycle WHERE pool_id = ?1 AND ordinal > ?2",
        params![pool, keep_ordinal],
    )
    .map_err(sql)?;
    let floor = after.unwrap_or(f64::NEG_INFINITY);
    tx.execute(
        "DELETE FROM reset_event WHERE pool_id = ?1 AND at_hi > ?2",
        params![pool, floor],
    )
    .map_err(sql)?;
    tx.execute(
        "DELETE FROM overage_span WHERE overage_id = ?1 AND at_hi > ?2",
        params![pool, floor],
    )
    .map_err(sql)?;

    let samples = load_samples(tx, pool, after)?;
    // What reset detection pairs the first new reading with: the last live
    // reading at or before the point, not the point itself. The point is
    // where coverage resumes and can be a cached reading, which detection
    // never pairs with, so the first live reading went unjudged - while the
    // fold, resuming with the last live figure, closed the cycle on it. A
    // full fold pairs the two live readings either side of the cached one.
    let before: Option<Sample> = match after {
        Some(at) => tx
            .query_row(
                &format!(
                    "SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at <= ?2 AND source != 'cached'
                      ORDER BY at DESC LIMIT 1"
                ),
                params![pool, at],
                sample_row,
            )
            .optional()
            .map_err(sql)?,
        None => None,
    };
    let plans = load_plans(tx, account)?;
    let banks = load_banks(tx, account)?;
    // Spent credits already given to a reset this fold keeps: never handed
    // to a pair of this fold as well.
    let claimed: Vec<(f64, f64)> = {
        let mut st = tx
            .prepare(
                "SELECT json_extract(evidence, '$.bank.at_lo'), json_extract(evidence, '$.bank.at_hi') FROM reset_event
                  WHERE pool_id = ?1 AND at_hi <= ?2 AND json_extract(evidence, '$.bank') IS NOT NULL",
            )
            .map_err(sql)?;
        let rows = st
            .query_map(params![pool, floor], |r| Ok((r.get::<_, Option<f64>>(0)?, r.get::<_, Option<f64>>(1)?)))
            .map_err(sql)?;
        let mut out = Vec::new();
        for row in rows {
            if let (Some(lo), Some(hi)) = row.map_err(sql)? {
                out.push((lo, hi));
            }
        }
        out
    };
    // Phase 2's seam: the resets in these samples.
    let found = resets::detect(&resets::DetectInput {
        // A rolling seed is never compared with what follows it.
        before: before.as_ref().filter(|_| seed.is_none()),
        samples: &samples,
        banks: &banks,
        plans: &plans,
        thresholds: *t,
        role,
        effect_override: t.reset_effect.get(&provider).map(resets::EffectOverride::name),
        rolling_seed: seed.as_ref(),
        claimed: &claimed,
    });
    let out = fold::fold(&FoldInput {
        role,
        samples: &samples,
        plans: &plans,
        resets: &found,
        prev: prev.as_ref(),
        next_ordinal: keep_ordinal as u32 + 1,
        rolling_seed: seed.as_ref(),
        resume: resume.as_ref(),
        gone_at: gone_close,
        thresholds: *t,
    });
    let mut ids: Vec<(i64, &Cycle)> = Vec::new();
    for c in &out.cycles {
        ids.push((insert_cycle(tx, pool, c)?, c));
    }
    // The resets the resumed cycle already held lost their cycle when its
    // row was replaced: they are its again.
    if let (Some(r), Some((id, _))) = (&resume, ids.first()) {
        tx.execute(
            "UPDATE reset_event SET cycle_id = ?1
              WHERE pool_id = ?2 AND cycle_id IS NULL AND at_hi <= ?3 AND at_lo >= ?4",
            params![id, pool, floor, r.cycle.starts_at],
        )
        .map_err(sql)?;
    }
    for e in &found {
        // The cycle it happened in: the one that observed its earlier
        // reading, which a restarted reset closed (§3.6) - even when the new
        // cycle was clamped to start at that very reading - and the kept one
        // when that reading is the kept cycle's last.
        let owned = ids
            .iter()
            .find(|(_, c)| c.first_obs_at.is_some_and(|f| f <= e.at_lo) && c.last_obs_at.is_some_and(|l| l >= e.at_lo))
            .map(|(id, _)| *id);
        let by_start = || ids.iter().rev().find(|(_, c)| c.starts_at <= e.at_lo).map(|(id, _)| *id);
        let cycle = owned.or(kept_id.filter(|_| prev.as_ref().and_then(|c| c.last_obs_at) == Some(e.at_lo))).or_else(by_start).or(kept_id);
        tx.execute(
            "INSERT OR IGNORE INTO reset_event(pool_id, cycle_id, at_lo, at_hi, kind, cycle_effect, signals, pct_before,
                    pct_after, reset_at_before, reset_at_after, group_id, evidence)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                pool,
                cycle,
                e.at_lo,
                e.at_hi,
                e.kind.name(),
                e.effect.name(),
                e.signals,
                e.pct_before,
                e.pct_after,
                e.reset_at_before,
                e.reset_at_after,
                e.group_id,
                e.evidence
            ],
        )
        .map_err(sql)?;
    }
    // Phase 4's seam: spend spans on an overage pool's own clock, and a
    // balance's spend.
    if matches!(role, Role::Overage | Role::Balance) {
        let linked: Vec<i64> = {
            let mut st = tx
                .prepare("SELECT included_id FROM pool_link WHERE overage_id = ?1")
                .map_err(sql)?;
            let rows = st.query_map([pool], |r| r.get(0)).map_err(sql)?;
            rows.collect::<Result<_, _>>().map_err(sql)?
        };
        let mut loaded: Vec<(i64, Vec<Sample>)> = Vec::new();
        // Only the stretch being re-folded: from the reading in force when
        // its first span opens.
        let open_at = after.or_else(|| samples.first().map(|s| s.at));
        for inc in linked {
            // From the reading in force at the re-fold point, not after it.
            let from: Option<f64> = match open_at {
                Some(at) => tx
                    .query_row(
                        "SELECT MAX(at) FROM sample WHERE pool_id = ?1 AND at <= ?2",
                        params![inc, at],
                        |r| r.get(0),
                    )
                    .map_err(sql)?,
                None => None,
            };
            loaded.push((inc, load_samples(tx, inc, from.map(|f| f - 1e-3).or(open_at.map(|a| a - 1e-3)))?));
        }
        let included: Vec<overage::Included> = loaded
            .iter()
            .map(|(id, s)| overage::Included {
                pool_id: *id,
                samples: s,
            })
            .collect();
        // The last sample already folded opens the first delta.
        let mut own: Vec<Sample> = before.iter().cloned().collect();
        own.extend(samples.iter().cloned());
        // A balance's grant expiries stated by readings before these and
        // still ahead of them: a fall across one is expiry, as the full
        // replay has it, whichever reading stated it.
        let stated: Vec<f64> = match (role, own.first()) {
            (Role::Balance, Some(first)) => {
                let mut st = tx
                    .prepare(
                        "SELECT DISTINCT reset_at FROM sample
                          WHERE pool_id = ?1 AND at < ?2 AND reset_at IS NOT NULL AND reset_at > ?2",
                    )
                    .map_err(sql)?;
                let rows = st.query_map(params![pool, first.at], |r| r.get::<_, f64>(0)).map_err(sql)?;
                rows.collect::<Result<_, _>>().map_err(sql)?
            }
            _ => Vec::new(),
        };
        for span in overage::spans_with(role, &own, &included, t, &stated) {
            let exhausted = serde_json::to_string(&span.exhausted).unwrap_or_else(|_| "[]".into());
            tx.execute(
                "INSERT OR REPLACE INTO overage_span(overage_id, at_lo, at_hi, amount, exhausted) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![pool, span.at_lo, span.at_hi, span.amount, exhausted],
            )
            .map_err(sql)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::shared::{PoolRole, Unit};

    const H: f64 = 3600.0;
    const WEEK: f64 = 7.0 * 24.0 * H;

    fn temp(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "opscope-history-{name}-{}-{}",
            std::process::id(),
            opscope_core::now().to_bits()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(FILE)
    }

    fn lane(key: &str, pct: f64, at: f64, reset: f64, window: f64) -> Lane {
        Lane {
            label: key.into(),
            pct,
            window_secs: Some(window),
            reset: Some(reset),
            pool_key: key.into(),
            fetched_at: at,
            ..Lane::default()
        }
    }

    fn obs(lanes: Vec<Lane>) -> Observation {
        Observation {
            provider: "codex".into(),
            account_id: "acct-1".into(),
            heading: "CODEX".into(),
            tab: "codex".into(),
            lanes,
            ..Observation::default()
        }
    }

    #[test]
    fn a_new_file_is_private_in_wal_mode_at_the_current_version() {
        use std::os::unix::fs::PermissionsExt;
        let p = temp("new");
        let h = History::open(&p).unwrap();
        let mode = std::fs::metadata(&p).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let v: i64 = h.conn.query_row("PRAGMA user_version", [], |r| r.get(0)).unwrap();
        assert_eq!(v, SCHEMA_VERSION);
        let j: String = h.conn.query_row("PRAGMA journal_mode", [], |r| r.get(0)).unwrap();
        assert_eq!(j, "wal");
        // synchronous NORMAL is 1.
        let s: i64 = h.conn.query_row("PRAGMA synchronous", [], |r| r.get(0)).unwrap();
        assert_eq!(s, 1);
        for table in [
            "meta",
            "account",
            "pool",
            "pool_link",
            "sample",
            "bank_sample",
            "plan_sample",
            "watch_state",
            "cycle",
            "segment",
            "reset_event",
            "bank_event",
            "overage_event",
            "overage_span",
        ] {
            assert!(h.count(table) >= 0, "{table} missing");
        }
        // Opened again, nothing is migrated twice.
        drop(h);
        History::open(&p).unwrap();
    }

    #[test]
    fn a_file_from_a_newer_opscope_is_left_alone() {
        let p = temp("newer");
        {
            let c = Connection::open(&p).unwrap();
            c.execute_batch("CREATE TABLE future(x); PRAGMA user_version = 99;")
                .unwrap();
        }
        let before = std::fs::read(&p).unwrap();
        assert_eq!(History::open(&p).err(), Some(OpenError::Newer(99)));
        assert_eq!(std::fs::read(&p).unwrap(), before, "a newer file must not be touched");
        // And the recorder says so on the tab.
        let mut r = super::super::Recorder::new(p.clone());
        let st = r.pass(&[], &Thresholds::default(), 1.0);
        assert!(
            st.note.as_deref().is_some_and(|n| n.contains("newer opscope")),
            "{:?}",
            st.note
        );
    }

    #[test]
    fn a_damaged_file_is_renamed_not_deleted_and_said_once() {
        let p = temp("damaged");
        std::fs::write(&p, b"this is not a sqlite database, not even slightly, it is text").unwrap();
        let h = History::open(&p).unwrap();
        let note = h.notice.clone().unwrap();
        assert!(note.contains("damaged"), "{note}");
        let dir = p.parent().unwrap();
        let aside: Vec<_> = std::fs::read_dir(dir)
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".damaged-"))
            .collect();
        assert_eq!(aside.len(), 1, "{aside:?}");
        assert_eq!(
            std::fs::read(dir.join(&aside[0])).unwrap(),
            b"this is not a sqlite database, not even slightly, it is text"
        );
        // The new file works.
        assert_eq!(h.count("sample"), 0);
        drop(h);
        // Opened again, it is healthy and says nothing.
        assert_eq!(History::open(&p).unwrap().notice, None);
    }

    fn aside_files(p: &Path) -> Vec<String> {
        let mut out: Vec<String> = std::fs::read_dir(p.parent().unwrap())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.contains(".damaged-"))
            .collect();
        out.sort();
        out
    }

    #[test]
    fn two_set_asides_in_one_second_never_overwrite_each_other() {
        let p = temp("aside-twice");
        std::fs::write(&p, b"first damaged copy").unwrap();
        std::fs::write(format!("{}-wal", p.display()), b"first wal").unwrap();
        set_aside(&p, 1_000, None).unwrap().unwrap();
        std::fs::write(&p, b"second damaged copy").unwrap();
        set_aside(&p, 1_000, None).unwrap().unwrap();
        let aside = aside_files(&p);
        let kept: Vec<Vec<u8>> = aside
            .iter()
            .filter(|n| !n.ends_with("-wal"))
            .map(|n| std::fs::read(p.parent().unwrap().join(n)).unwrap())
            .collect();
        assert!(kept.contains(&b"first damaged copy".to_vec()), "{aside:?}");
        assert!(kept.contains(&b"second damaged copy".to_vec()), "{aside:?}");
        assert_eq!(aside.iter().filter(|n| n.ends_with("-wal")).count(), 1, "{aside:?}");
    }

    #[test]
    fn a_pane_late_to_a_damaged_file_never_moves_the_new_one_aside() {
        let p = temp("aside-late");
        std::fs::write(&p, b"damaged").unwrap();
        // Both panes found this file damaged.
        let failed = identity(&p);
        // The first sets it aside and starts a new file.
        assert!(set_aside(&p, 1_000, failed).unwrap().is_some());
        let h = History::open(&p).unwrap();
        assert_eq!(h.notice, None);
        drop(h);
        let fresh = identity(&p);
        // The second, late, finds a different file there and leaves it.
        assert_eq!(set_aside(&p, 1_000, failed).unwrap(), None);
        assert_eq!(identity(&p), fresh);
        assert_eq!(aside_files(&p).len(), 1);
        assert_eq!(History::open(&p).unwrap().count("sample"), 0);
    }

    #[test]
    fn a_side_file_that_cannot_follow_is_kept_and_recovery_says_it_is_incomplete() {
        // CodeRabbit's case: the main file moved and its WAL could not. The
        // WAL may hold committed history; it was deleted. It is kept now,
        // and recovery reports itself incomplete.
        let p = temp("aside-wal");
        let wal = PathBuf::from(format!("{}-wal", p.display()));
        std::fs::write(&wal, b"frames").unwrap();
        let aside = PathBuf::from(format!("{}.damaged-x", p.display()));
        // The name it would move to is taken, so the move fails.
        std::fs::write(format!("{}-wal", aside.display()), b"other").unwrap();
        let why = move_sides(&p, &aside).unwrap_err();
        assert!(why.contains("-wal"), "{why}");
        assert_eq!(std::fs::read(&wal).unwrap(), b"frames", "the WAL is still there, untouched");
    }

    #[test]
    fn no_new_file_is_started_beside_a_log_left_from_a_set_aside_one() {
        // CodeRabbit's case: a second pane could create and open a new file
        // while the damaged one's WAL was still at the live path, and SQLite
        // would replay it into the new file. With no main file and a WAL
        // still beside it, nothing is created.
        let p = temp("stale-wal");
        let wal = PathBuf::from(format!("{}-wal", p.display()));
        std::fs::write(&wal, b"frames").unwrap();
        match History::open(&p) {
            Err(OpenError::Failed(why)) => assert!(why.contains("-wal"), "{why}"),
            Err(other) => panic!("{other:?}"),
            Ok(_) => panic!("opened a new file beside a stale WAL"),
        }
        assert!(!p.exists(), "no main file was created");
        assert_eq!(std::fs::read(&wal).unwrap(), b"frames");
    }

    #[test]
    fn samples_follow_the_change_and_heartbeat_rules_and_fold_into_cycles() {
        let p = temp("record");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + WEEK;
        // A held reading seen on three passes is one sample.
        for _ in 0..3 {
            h.record(
                &[obs(vec![lane("secondary_window", 10.0, a + H, end, WEEK)])],
                &t,
                a + H,
            )
            .unwrap();
        }
        assert_eq!(h.count("sample"), 1);
        // Changed within five minutes: not written.
        h.record(
            &[obs(vec![lane("secondary_window", 11.0, a + H + 60.0, end, WEEK)])],
            &t,
            a + H + 60.0,
        )
        .unwrap();
        assert_eq!(h.count("sample"), 1);
        // Changed later: written.
        h.record(
            &[obs(vec![lane("secondary_window", 20.0, a + 2.0 * H, end, WEEK)])],
            &t,
            a + 2.0 * H,
        )
        .unwrap();
        // Flat for an hour: the heartbeat.
        h.record(
            &[obs(vec![lane("secondary_window", 20.0, a + 3.0 * H, end, WEEK)])],
            &t,
            a + 3.0 * H,
        )
        .unwrap();
        assert_eq!(h.count("sample"), 3);
        // Projected lanes are never written; nor is one with no fetch time.
        let mut projected = lane("secondary_window", 0.0, a + 5.0 * H, end, WEEK);
        projected.projected = true;
        let undated = lane("primary_window", 0.0, 0.0, end, WEEK);
        h.record(&[obs(vec![projected, undated])], &t, a + 5.0 * H).unwrap();
        assert_eq!(h.count("sample"), 3);
        // Past the end: a rollover.
        h.record(
            &[obs(vec![lane("secondary_window", 1.0, end + H, end + WEEK, WEEK)])],
            &t,
            end + H,
        )
        .unwrap();
        let key = obs(vec![]).account_key();
        let cycles = h.cycles("codex", &key, "secondary_window").unwrap();
        assert_eq!(cycles.len(), 2);
        assert_eq!(cycles[0].end_reason, Some(EndReason::Scheduled));
        assert_eq!(cycles[0].peak_pct, Some(20.0));
        assert_eq!(cycles[1].closed_at, None);
        assert_eq!(h.since("codex", &key), Some(a + H));
    }

    #[test]
    fn an_idle_stretch_written_pass_by_pass_never_erases_the_pools_cycles() {
        let p = temp("idle");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let w = 5.0 * H;
        let key = obs(vec![]).account_key();
        let pass = |h: &mut History, at: f64, pct: f64, reset: f64| {
            h.record(&[obs(vec![lane("primary_window", pct, at, reset, w)])], &t, at)
                .unwrap();
        };
        for k in 0..3 {
            let start = a + k as f64 * w;
            for i in 1..5 {
                pass(&mut h, start + i as f64 * H, 10.0 * i as f64, start + w);
            }
        }
        // Idle past the end: every reading an unstarted window.
        let idle = a + 3.0 * w + 600.0;
        for i in 0..6 {
            let at = idle + i as f64 * 300.0;
            pass(&mut h, at, 0.0, at + w);
        }
        let closed = |h: &History| {
            h.cycles("codex", &key, "primary_window")
                .unwrap()
                .iter()
                .filter(|c| c.closed_at.is_some())
                .count()
        };
        assert_eq!(closed(&h), 3);
        // Used again: the history carries on from there.
        let used = idle + 2.0 * H;
        pass(&mut h, used, 5.0, used + w - 60.0);
        pass(&mut h, used + H, 15.0, used + w - 60.0);
        assert_eq!(closed(&h), 3);
        let cycles = h.cycles("codex", &key, "primary_window").unwrap();
        assert_eq!(cycles.last().unwrap().peak_pct, Some(15.0));
    }

    /// How many samples one `record` of `o` reads.
    fn reads_of(h: &mut History, o: Observation, at: f64) -> usize {
        SAMPLES_READ.with(|n| n.set(0));
        h.record(&[o], &Thresholds::default(), at).unwrap();
        SAMPLES_READ.with(|n| n.get())
    }

    /// A pass of two pools of one account: `a` (five-hour) only when given.
    fn two_pools(at: f64, a: Option<(f64, f64)>, b_pct: f64, b_stale: bool) -> Observation {
        let w = 5.0 * H;
        let mut lanes = Vec::new();
        if let Some((pct, end)) = a {
            lanes.push(lane("primary_window", pct, at, end, w));
        }
        let mut b = lane("secondary_window", b_pct, at, 2_000_000.0 + WEEK, WEEK);
        b.stale = b_stale;
        lanes.push(b);
        obs(lanes)
    }

    #[test]
    fn a_pool_missing_from_live_readings_for_a_window_while_its_siblings_answer_is_gone() {
        // Spec §4: the five-hour pool stops appearing while the weekly pool
        // of the same account goes on answering live, for longer than a
        // whole five-hour window. Its open cycle closes as `pool_gone`.
        let p = temp("pool-gone");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 2_000_000.0;
        let end = a + 4.0 * H;
        h.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        let mut at = a + 0.5 * H;
        while at <= a + 6.0 * H {
            h.record(&[two_pools(at, None, 10.0 + (at - a) / H, false)], &t, at).unwrap();
            at += 0.5 * H;
        }
        let key = obs(vec![]).account_key();
        let c = h.cycles("codex", &key, "primary_window").unwrap();
        assert_eq!(c.len(), 1, "{c:#?}");
        assert_eq!(c[0].end_reason, Some(EndReason::PoolGone));
        assert_eq!(c[0].closed_at, Some(end), "closed no later than its own stated end");
        // A full fold from the samples says the same.
        let (stored, full) = stored_then_full(&mut h, "primary_window");
        assert_eq!(stored, full);
        // Back again: the absence is judged again with the new reading, as
        // every derived row is (§3.5) - the pool was not gone after all, so
        // the cycle ended on schedule - and a new cycle opens.
        let back = a + 7.0 * H;
        h.record(&[two_pools(back, Some((5.0, back + 4.0 * H)), 20.0, false)], &t, back).unwrap();
        let c = h.cycles("codex", &key, "primary_window").unwrap();
        assert_eq!(c[0].end_reason, Some(EndReason::Scheduled));
        assert_eq!(c[0].closed_at, Some(end));
        assert!(c.last().unwrap().closed_at.is_none());
    }

    #[test]
    fn absence_is_measured_from_the_last_live_answer_not_the_last_sample() {
        // CodeRabbit's case: the five-hour pool answers unchanged half an
        // hour after its last stored sample, so that answer is held back,
        // and then it vanishes. Measured from the stored sample the cycle
        // closed after four and a half hours of absence.
        let p = temp("pool-gone-held");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 2_000_000.0;
        let end = a + 9.0 * H;
        h.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        // Unchanged, so not written: the heartbeat is an hour.
        h.record(&[two_pools(a + 0.5 * H, Some((40.0, end)), 10.5, false)], &t, a + 0.5 * H).unwrap();
        let key = obs(vec![]).account_key();
        assert_eq!(h.samples("codex", &key, "primary_window").unwrap().len(), 1);
        let mut at = a + H;
        while at <= a + 7.0 * H {
            h.record(&[two_pools(at, None, 10.0 + (at - a) / H, false)], &t, at).unwrap();
            at += 0.5 * H;
        }
        let c = h.cycles("codex", &key, "primary_window").unwrap();
        assert_eq!(c[0].end_reason, Some(EndReason::PoolGone));
        assert_eq!(c[0].closed_at, Some(a + 0.5 * H + 5.0 * H), "a window after the last live answer");
        let (stored, full) = stored_then_full(&mut h, "primary_window");
        assert_eq!(stored, full);
        // A cached copy of the pool is not an answer, and moves nothing.
        let q = temp("pool-gone-cached");
        let mut g = History::open(&q).unwrap();
        g.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        let mut cached = two_pools(a + 0.5 * H, Some((40.0, end)), 10.5, false);
        cached.lanes[0].stale = true;
        cached.lanes[0].fetched_at = a + 0.5 * H;
        g.record(&[cached], &t, a + 0.5 * H).unwrap();
        let mut at = a + H;
        while at <= a + 7.0 * H {
            g.record(&[two_pools(at, None, 10.0 + (at - a) / H, false)], &t, at).unwrap();
            at += 0.5 * H;
        }
        let c = g.cycles("codex", &key, "primary_window").unwrap();
        assert_eq!(c[0].closed_at, Some(a + 5.0 * H));
    }

    #[test]
    fn absence_while_idle_failing_or_cached_is_never_pool_gone() {
        let t = Thresholds::default();
        let a = 2_000_000.0;
        let end = a + 4.0 * H;
        let key = obs(vec![]).account_key();
        let gone = |h: &History| {
            h.cycles("codex", &key, "primary_window")
                .unwrap()
                .iter()
                .any(|c| c.end_reason == Some(EndReason::PoolGone))
        };
        // Nothing read at all for six hours - asleep, or every fetch failed -
        // then both pools back: the gap is the pane's, not the pool's.
        let mut h = History::open(&temp("gone-idle")).unwrap();
        h.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        h.record(&[two_pools(a + 6.0 * H, None, 12.0, false)], &t, a + 6.0 * H).unwrap();
        assert!(!gone(&h));
        // Only cached sibling readings through the gap: no live evidence.
        let mut h = History::open(&temp("gone-cached")).unwrap();
        h.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        let mut at = a + 0.5 * H;
        while at <= a + 6.0 * H {
            h.record(&[two_pools(at, None, 10.0 + (at - a) / H, true)], &t, at).unwrap();
            at += 0.5 * H;
        }
        assert!(!gone(&h));
        // A limit last read at about zero is idle - Claude leaves an unused
        // limit out - and its absence is no sign it is gone.
        let mut h = History::open(&temp("gone-zero")).unwrap();
        h.record(&[two_pools(a, Some((0.0, end)), 10.0, false)], &t, a).unwrap();
        let mut at = a + 0.5 * H;
        while at <= a + 6.0 * H {
            h.record(&[two_pools(at, None, 10.0 + (at - a) / H, false)], &t, at).unwrap();
            at += 0.5 * H;
        }
        assert!(!gone(&h));
        // Siblings live for less than a window: not yet.
        let mut h = History::open(&temp("gone-short")).unwrap();
        h.record(&[two_pools(a, Some((40.0, end)), 10.0, false)], &t, a).unwrap();
        let mut at = a + 0.5 * H;
        while at <= a + 4.5 * H {
            h.record(&[two_pools(at, None, 10.0 + (at - a) / H, false)], &t, at).unwrap();
            at += 0.5 * H;
        }
        assert!(!gone(&h));
    }

    #[test]
    fn a_pool_whose_only_cycle_is_open_reads_only_its_new_samples_on_a_write() {
        // The known gap: an undated pool that never falls back to zero -
        // Claude's extra usage on a cap nobody reaches - has one open cycle
        // and nothing closed to fold on from, so every write read its whole
        // history. It now folds on from the open cycle itself.
        let p = temp("open-only");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let claude = |used: f64, at: f64| Observation {
            provider: "claude".into(),
            account_id: "acct-open".into(),
            heading: "CLAUDE".into(),
            tab: "claude".into(),
            lanes: vec![Lane {
                label: "extra A$50".into(),
                pct: 100.0 * used / 5000.0,
                pool_key: "extra_usage".into(),
                unit: Unit::Money { currency: "AUD".into(), exponent: 2 },
                role: PoolRole::Overage,
                used: Some(used),
                limit: Some(5000.0),
                fetched_at: at,
                ..Lane::default()
            }],
            ..Observation::default()
        };
        // Twelve hundred readings, each a little more spent; with a gap
        // over two hours now and then, so coverage is not trivially whole.
        let mut at = a;
        for i in 0..1200 {
            h.record(&[claude(1.0 + i as f64 * 0.5, at)], &Thresholds::default(), at).unwrap();
            at += if i % 97 == 0 { 3.0 * H } else { 0.5 * H };
        }
        assert!(h.count("sample") >= 1200);
        let read = reads_of(&mut h, claude(1000.0, at), at);
        assert!(read <= 5, "one write read {read} samples");
        let key = claude(0.0, 0.0).account_key();
        let stored = h.cycles("claude", &key, "extra_usage").unwrap();
        assert_eq!(stored.len(), 1);
        assert!(stored[0].closed_at.is_none());
        let spans_stored = h.count("overage_span");
        let tx = h.conn.transaction().unwrap();
        refold_pool(&tx, 1, None, &Thresholds::default()).unwrap();
        tx.commit().unwrap();
        let full = h.cycles("claude", &key, "extra_usage").unwrap();
        assert_eq!(spans_stored, h.count("overage_span"));
        // Equal to a fold over everything; coverage only to rounding, since
        // the open cycle's watched time is carried as its stored fraction.
        let (mut s, mut f) = (stored[0].clone(), full[0].clone());
        assert!((s.coverage - f.coverage).abs() < 1e-9, "{} vs {}", s.coverage, f.coverage);
        s.coverage = 0.0;
        f.coverage = 0.0;
        assert_eq!(s, f);
        let segments = |h: &History| -> Vec<(i64, f64, Option<f64>)> {
            let mut st = h.conn.prepare("SELECT n, starts_at, peak_pct FROM segment ORDER BY n").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?))).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(segments(&h).len(), 1);
    }

    #[test]
    fn one_write_reads_a_bounded_number_of_samples_however_long_the_history() {
        let p = temp("bounded");
        let mut h = History::open(&p).unwrap();
        let a = 1_000_000.0;
        let w = 5.0 * H;
        let balance = |left: f64, at: f64| Lane {
            label: "credits".into(),
            pool_key: "credits".into(),
            role: PoolRole::Balance,
            unit: Unit::Credits,
            limit: Some(left),
            pct: f64::NAN,
            fetched_at: at,
            ..Lane::default()
        };
        let rolling = |at: f64| Lane {
            label: "rolling".into(),
            pool_key: "rolling".into(),
            pct: 20.0 + (at / 600.0) % 7.0,
            window_secs: Some(w),
            reset: Some(at + w),
            fetched_at: at,
            ..Lane::default()
        };
        let pass = |at: f64| {
            let start = a + ((at - a) / w).floor() * w;
            let pct = 10.0 + (at - start) / w * 80.0;
            obs(vec![
                lane("primary_window", pct, at, start + w, w),
                balance(1000.0 - (at - a) / H, at),
                rolling(at),
            ])
        };
        // Forty 5h windows read every half hour: four hundred samples a pool.
        let mut at = a + 60.0;
        while at < a + 40.0 * w {
            h.record(&[pass(at)], &Thresholds::default(), at).unwrap();
            at += 0.5 * H;
        }
        assert!(h.count("sample") >= 1200, "{}", h.count("sample"));
        let read = reads_of(&mut h, pass(at), at);
        // About two windows of the dated pool (twenty), two of the balance
        // and its linked pool's few, and a couple of the rolling one.
        assert!(read <= 40, "one write read {read} samples");
        // And what those bounded writes folded is what one fold over
        // everything gives.
        let key = obs(vec![]).account_key();
        let spans = |h: &History| -> Vec<(f64, f64, f64, String)> {
            let mut st = h
                .conn
                .prepare("SELECT at_lo, at_hi, amount, exhausted FROM overage_span ORDER BY overage_id, at_hi")
                .unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap()
        };
        let before: Vec<_> = ["primary_window", "rolling"]
            .iter()
            .map(|k| h.cycles("codex", &key, k).unwrap())
            .collect();
        let spans_before = spans(&h);
        assert!(!spans_before.is_empty());
        let tx = h.conn.transaction().unwrap();
        let pools: Vec<i64> = tx
            .prepare("SELECT id FROM pool")
            .unwrap()
            .query_map([], |r| r.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        for pool in pools {
            refold_pool(&tx, pool, None, &Thresholds::default()).unwrap();
        }
        tx.commit().unwrap();
        let after: Vec<_> = ["primary_window", "rolling"]
            .iter()
            .map(|k| h.cycles("codex", &key, k).unwrap())
            .collect();
        assert_eq!(before, after);
        assert_eq!(spans_before, spans(&h));
    }

    #[test]
    fn an_outage_with_no_account_id_is_kept_under_the_last_account_seen() {
        let p = temp("no-id");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let mut o = obs(vec![lane("primary_window", 10.0, a, a + WEEK, WEEK)]);
        o.provider = "copilot".into();
        h.record(&[o.clone()], &t, a).unwrap();
        // The live call fails: no body, so no id, and the pane says why.
        let mut down = o.clone();
        down.account_id.clear();
        down.lanes.clear();
        down.watch = Some(super::super::WatchReading {
            at: a + H,
            state: "network".into(),
            reason: "timed out".into(),
        });
        h.record(&[down.clone()], &t, a + H).unwrap();
        assert_eq!(h.count("account"), 1, "an outage made an account of its own");
        assert_eq!(h.count("watch_state"), 1);
        assert_eq!(h.account_key_of(&down), o.account_key());
        // A provider never seen with an id keeps its profile key.
        let mut never = down.clone();
        never.provider = "notion".into();
        assert_eq!(h.account_key_of(&never), "profile:default");
    }

    #[test]
    fn a_cached_reading_is_written_as_cached() {
        let p = temp("cached");
        let mut h = History::open(&p).unwrap();
        let mut l = lane("primary_window", 10.0, 5000.0, 20000.0, 18000.0);
        l.stale = true;
        h.record(&[obs(vec![l])], &Thresholds::default(), 6000.0).unwrap();
        let s = h
            .samples("codex", &obs(vec![]).account_key(), "primary_window")
            .unwrap();
        assert_eq!(s[0].source, Source::Cached);
    }

    #[test]
    fn an_absent_lane_writes_nothing() {
        let p = temp("absent");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        h.record(
            &[obs(vec![
                lane("primary_window", 10.0, 5000.0, 20000.0, 18000.0),
                lane("secondary_window", 10.0, 5000.0, 600000.0, WEEK),
            ])],
            &t,
            5000.0,
        )
        .unwrap();
        // A pass where the secondary window is missing: nothing for it, and
        // its open cycle stays open (absence is not pool_gone).
        h.record(
            &[obs(vec![lane("primary_window", 30.0, 9000.0, 20000.0, 18000.0)])],
            &t,
            9000.0,
        )
        .unwrap();
        let key = obs(vec![]).account_key();
        assert_eq!(h.samples("codex", &key, "secondary_window").unwrap().len(), 1);
        let c = h.cycles("codex", &key, "secondary_window").unwrap();
        assert_eq!(c.len(), 1);
        assert_eq!(c[0].closed_at, None);
    }

    #[test]
    fn an_out_of_order_sample_refolds_the_pool() {
        let p = temp("order");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + WEEK;
        // Three weeks, read late in each, then a reading from the first
        // week arrives - another pane's older fetch.
        for k in 0..3 {
            let base = a + k as f64 * WEEK;
            h.record(
                &[obs(vec![lane(
                    "secondary_window",
                    30.0,
                    base + WEEK - 2.0 * H,
                    base + WEEK,
                    WEEK,
                )])],
                &t,
                base + WEEK - 2.0 * H,
            )
            .unwrap();
        }
        let key = obs(vec![]).account_key();
        assert_eq!(h.cycles("codex", &key, "secondary_window").unwrap().len(), 3);
        h.record(
            &[obs(vec![lane("secondary_window", 95.0, a + 2.0 * H, end, WEEK)])],
            &t,
            a + 4.0 * WEEK,
        )
        .unwrap();
        let cycles = h.cycles("codex", &key, "secondary_window").unwrap();
        assert_eq!(cycles.len(), 3);
        assert_eq!(cycles[0].peak_pct, Some(95.0));
        assert_eq!(cycles[0].first_obs_at, Some(a + 2.0 * H));
        // The same as folding every sample at once. 95% then 30% in one
        // week is a reset (phase 2), so the fresh fold is handed it too.
        let all = h.samples("codex", &key, "secondary_window").unwrap();
        let found = resets::detect(&resets::DetectInput {
            before: None,
            samples: &all,
            banks: &[],
            plans: &[],
            thresholds: t,
            role: Role::Included,
            effect_override: None,
            rolling_seed: None,
            claimed: &[],
        });
        assert_eq!(found.len(), 1);
        let fresh = fold::fold(&FoldInput {
            role: Role::Included,
            samples: &all,
            plans: &[],
            resets: &found,
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: t,
        });
        let strip = |v: Vec<Cycle>| -> Vec<Cycle> {
            v.into_iter()
                .map(|mut c| {
                    c.segments.clear();
                    c
                })
                .collect()
        };
        assert_eq!(strip(cycles), strip(fresh.cycles));
    }

    #[test]
    fn a_changed_detector_version_refolds_everything_on_open() {
        let p = temp("detector");
        let a = 1_000_000.0;
        {
            let mut h = History::open(&p).unwrap();
            h.record(
                &[obs(vec![lane("secondary_window", 30.0, a + H, a + WEEK, WEEK)])],
                &Thresholds::default(),
                a + H,
            )
            .unwrap();
            // Pretend an older detector folded it, wrongly.
            h.conn
                .execute("UPDATE meta SET v = '0' WHERE k = 'detector_version'", [])
                .unwrap();
            h.conn.execute("UPDATE cycle SET peak_pct = 1.0", []).unwrap();
        }
        let h = History::open(&p).unwrap();
        let c = h
            .cycles("codex", &obs(vec![]).account_key(), "secondary_window")
            .unwrap();
        assert_eq!(c[0].peak_pct, Some(30.0));
    }

    #[test]
    fn a_changed_detector_version_rebuilds_the_bank_events_too() {
        // CodeRabbit's case: the upgrade refolded every pool and left the
        // bank events an older detector classified, which the write path
        // rebuilds only when an inventory changes.
        let p = temp("detector-bank");
        let a = 1_000_000.0;
        {
            let mut h = History::open(&p).unwrap();
            let mut o = obs(vec![lane("secondary_window", 30.0, a + H, a + WEEK, WEEK)]);
            o.bank = Some(BankReading {
                at: a + H,
                left: 1,
                credits: Some(vec![super::super::RawCredit {
                    id: Some("c1".into()),
                    status: "available".into(),
                    expires_at: Some(9e9),
                }]),
            });
            h.record(std::slice::from_ref(&o), &Thresholds::default(), a + H).unwrap();
            assert_eq!(h.count("bank_event"), 0);
            // An older detector's reading of the same inventory.
            h.conn
                .execute(
                    "INSERT INTO bank_event(account_id, at_lo, at_hi, kind, count, expires_at)
                     SELECT account_id, at - 60.0, at, 'redeemed', 1, NULL FROM bank_sample",
                    [],
                )
                .unwrap();
            h.conn
                .execute("UPDATE meta SET v = '0' WHERE k = 'detector_version'", [])
                .unwrap();
        }
        let h = History::open(&p).unwrap();
        assert_eq!(h.count("bank_event"), 0);
    }

    #[test]
    fn a_new_detector_refolds_with_the_configured_thresholds() {
        let p = temp("detector-config");
        let a = 1_000_000.0;
        // A fall to 3% while watching is a reset only with a near-zero of 5.
        let t = Thresholds {
            near_zero_pct: 5.0,
            ..Thresholds::default()
        };
        {
            let mut h = History::open(&p).unwrap();
            for (at, pct) in [(a + H, 50.0), (a + H + 400.0, 3.0)] {
                h.record(&[obs(vec![lane("secondary_window", pct, at, a + WEEK, WEEK)])], &t, at)
                    .unwrap();
            }
            assert_eq!(h.count("reset_event"), 1);
            h.conn
                .execute("UPDATE meta SET v = '0' WHERE k = 'detector_version'", [])
                .unwrap();
        }
        let mut r = super::super::Recorder::with_rollouts(p.clone(), Vec::new());
        r.pass(&[], &t, a + 2.0 * H);
        let h = History::open_with(&p, &t).unwrap();
        assert_eq!(h.count("reset_event"), 1);
        // The local zone is kept beside the times (§3.6).
        assert!(h.meta("tz").unwrap().is_some());
        assert!(h.meta("tz_offset").unwrap().is_some_and(|o| o.starts_with(['+', '-'])));
        assert_eq!(h.meta("detector_version").unwrap().as_deref(), Some(&*fold::DETECTOR_VERSION.to_string()));
    }

    #[test]
    fn bank_plan_and_watch_are_written_only_on_change() {
        let p = temp("change");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let mut o = obs(vec![]);
        let credit = |status: &str| super::super::RawCredit {
            id: Some("c1".into()),
            status: status.into(),
            expires_at: Some(9e9),
        };
        o.bank = Some(BankReading {
            at: 100.0,
            left: 1,
            credits: Some(vec![credit("available")]),
        });
        o.plan = Some(super::super::PlanReading {
            at: 100.0,
            plan: "pro".into(),
        });
        o.watch = Some(WatchReading {
            at: 100.0,
            state: "ok".into(),
            reason: String::new(),
        });
        h.record(std::slice::from_ref(&o), &t, 100.0).unwrap();
        // The same again, later: nothing new.
        o.bank.as_mut().unwrap().at = 200.0;
        o.plan.as_mut().unwrap().at = 200.0;
        o.watch.as_mut().unwrap().at = 200.0;
        h.record(std::slice::from_ref(&o), &t, 200.0).unwrap();
        assert_eq!(
            (h.count("bank_sample"), h.count("plan_sample"), h.count("watch_state")),
            (1, 1, 1)
        );
        // The credit spent: the raw list changed though the server's count
        // could have stayed; written.
        o.bank = Some(BankReading {
            at: 300.0,
            left: 0,
            credits: Some(vec![credit("spent")]),
        });
        o.plan.as_mut().unwrap().plan = "plus".into();
        o.plan.as_mut().unwrap().at = 300.0;
        o.watch = Some(WatchReading {
            at: 300.0,
            state: "auth".into(),
            reason: "token expired".into(),
        });
        h.record(std::slice::from_ref(&o), &t, 300.0).unwrap();
        assert_eq!(
            (h.count("bank_sample"), h.count("plan_sample"), h.count("watch_state")),
            (2, 2, 2)
        );
        let raw: String = h
            .conn
            .query_row("SELECT credits FROM bank_sample ORDER BY at DESC LIMIT 1", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert!(raw.contains("spent"), "{raw}");
    }

    /// Every stored cycle and reset of one pool, then the same after a fold
    /// of the whole pool from its samples.
    fn stored_then_full(h: &mut History, key: &str) -> ((Vec<Cycle>, Vec<String>), (Vec<Cycle>, Vec<String>)) {
        let t = Thresholds::default();
        let pool: i64 = h
            .conn
            .query_row("SELECT id FROM pool WHERE pool_key = ?1", [key], |r| r.get(0))
            .unwrap();
        let read = |h: &History| {
            let cycles: Vec<Cycle> = load_cycles(&h.conn, pool).unwrap().into_iter().map(|(_, c)| c).collect();
            let mut st = h
                .conn
                .prepare("SELECT at_lo, at_hi, kind, signals, cycle_effect FROM reset_event WHERE pool_id = ?1 ORDER BY at_hi")
                .unwrap();
            let resets: Vec<String> = st
                .query_map([pool], |r| {
                    Ok(format!(
                        "{}-{} {} {} {}",
                        r.get::<_, f64>(0)?,
                        r.get::<_, f64>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            (cycles, resets)
        };
        let stored = read(h);
        let tx = h.conn.transaction().unwrap();
        refold_pool(&tx, pool, None, &t).unwrap();
        tx.commit().unwrap();
        (stored, read(h))
    }

    #[test]
    fn a_kept_cycle_closed_by_a_reading_that_turns_out_rolling_is_folded_again() {
        // CodeRabbit's case: one fixed weekly reading before E, then five in
        // use after it, each stating a reset a week after its own fetch, at
        // 10, 70, 130, 190 and 250 minutes. The first closed the fixed cycle
        // at E; only the fifth confirms the run as rolling, and by then the
        // checkpoint kept the close a fresh fold no longer makes.
        let p = temp("kept-rolling");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let e = a + WEEK;
        h.record(&[obs(vec![lane("secondary_window", 40.0, e - 2.0 * H, e, WEEK)])], &t, e - 2.0 * H).unwrap();
        for (k, mins) in [10.0, 70.0, 130.0, 190.0, 250.0].into_iter().enumerate() {
            let at = e + mins * 60.0;
            h.record(&[obs(vec![lane("secondary_window", 41.0 + k as f64, at, at + WEEK, WEEK)])], &t, at).unwrap();
        }
        let (stored, full) = stored_then_full(&mut h, "secondary_window");
        assert_eq!(stored.0, full.0, "{:#?}\n{:#?}", stored.0, full.0);
    }

    #[test]
    fn a_resumed_fold_judges_a_reset_across_a_cached_reading_as_a_full_fold_does() {
        // CodeRabbit's case: an undated pool reads 60% live, then a cached
        // reading, then 0% live with a credit spent between. With
        // kept_boundary configured a full fold adds an allowance; resumed
        // from the cached reading, detection had no live reading to pair
        // the 0% with, and the fold closed the cycle instead.
        let p = temp("resume-cached");
        let mut h = History::open(&p).unwrap();
        let mut t = Thresholds::default();
        t.reset_effect = super::super::resets::ResetEffects::from_config(&serde_json::json!({"codex": "kept_boundary"}));
        let a = 1_000_000.0;
        let undated = |pct: f64, at: f64, stale: bool| Lane {
            label: "extra".into(),
            pct,
            pool_key: "undated".into(),
            fetched_at: at,
            stale,
            ..Lane::default()
        };
        let credit = |status: &str| super::super::RawCredit {
            id: Some("c1".into()),
            status: status.into(),
            expires_at: Some(9e9),
        };
        let mut o = obs(vec![undated(60.0, a, false)]);
        o.bank = Some(BankReading { at: a, left: 1, credits: Some(vec![credit("available")]) });
        h.record(std::slice::from_ref(&o), &t, a).unwrap();
        // The credit is seen spent with the cached reading, so the pass that
        // brings the 0% only appends, and the open cycle is resumed.
        let mut o = obs(vec![undated(61.0, a + 10.0 * 60.0, true)]);
        o.bank = Some(BankReading { at: a + 600.0, left: 0, credits: Some(vec![credit("spent")]) });
        h.record(std::slice::from_ref(&o), &t, a + 600.0).unwrap();
        let o = obs(vec![undated(0.0, a + 1200.0, false)]);
        h.record(std::slice::from_ref(&o), &t, a + 1200.0).unwrap();
        let pool: i64 = h.conn.query_row("SELECT id FROM pool WHERE pool_key = 'undated'", [], |r| r.get(0)).unwrap();
        let stored: Vec<Cycle> = load_cycles(&h.conn, pool).unwrap().into_iter().map(|(_, c)| c).collect();
        let tx = h.conn.transaction().unwrap();
        refold_pool(&tx, pool, None, &t).unwrap();
        tx.commit().unwrap();
        let full: Vec<Cycle> = load_cycles(&h.conn, pool).unwrap().into_iter().map(|(_, c)| c).collect();
        assert_eq!(full.len(), 1, "{full:#?}");
        assert_eq!(full[0].allowances, 2, "the full fold kept the boundary");
        assert_eq!(stored, full);
    }

    #[test]
    fn a_late_fixed_reading_inside_an_established_rolling_stretch_folds_as_a_full_fold() {
        // CodeRabbit's case: four readings established a rolling stretch, so
        // the pool keeps no cycle and folds on from its last reading as a
        // seed. A late fixed-window reading then lands between the first two
        // and breaks the stretch there: a full fold keeps the first reading
        // in a cycle, and the seeded fold left it out.
        let p = temp("rolling-late");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let w = 6.0 * H;
        for i in 0..4 {
            let at = a + i as f64 * H;
            h.record(&[obs(vec![lane("primary_window", 20.0, at, at + w, w)])], &t, at).unwrap();
        }
        assert!(h.cycles("codex", &obs(vec![]).account_key(), "primary_window").unwrap().is_empty());
        // Late: fetched half an hour after the first, stating its end.
        h.record(&[obs(vec![lane("primary_window", 25.0, a + 1800.0, a + w, w)])], &t, a + 5.0 * H)
            .unwrap();
        let (stored, full) = stored_then_full(&mut h, "primary_window");
        assert_eq!(stored, full);
        assert_eq!(full.0.len(), 1);
        assert_eq!(full.0[0].first_obs_at, Some(a));
    }

    #[test]
    fn a_restarted_reset_clamped_to_its_earlier_reading_belongs_to_the_cycle_it_ended() {
        // CodeRabbit's case: the new cycle's start would fall before the
        // earlier reading, so it is clamped to it and the new cycle starts
        // exactly there. The reset and its lower-bound flag still belong to
        // the cycle that observed the earlier reading - the one it ended.
        let p = temp("clamped");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let lo = a + 24.0 * H;
        // An hour apart (offline): 90% to 0%, and the week now starts an
        // hour before the earlier reading.
        h.record(&[obs(vec![lane("secondary_window", 90.0, lo, a + WEEK, WEEK)])], &t, lo).unwrap();
        h.record(&[obs(vec![lane("secondary_window", 0.0, lo + H, lo - H + WEEK, WEEK)])], &t, lo + H)
            .unwrap();
        let rows = load_cycles(&h.conn, 1).unwrap();
        assert_eq!(rows.len(), 2, "{rows:#?}");
        assert_eq!(rows[0].1.end_reason, Some(EndReason::Reset));
        assert_eq!(rows[1].1.starts_at, lo);
        let owner: i64 = h.conn.query_row("SELECT cycle_id FROM reset_event", [], |r| r.get(0)).unwrap();
        assert_eq!(owner, rows[0].0, "filed under the cycle it cut short");
        assert!(rows[0].1.consumed_is_lower_bound, "the cut-short cycle used at least what it showed");
        assert!(!rows[1].1.consumed_is_lower_bound);
    }

    #[test]
    fn a_redemption_kept_with_a_retained_reset_is_never_given_to_a_later_pair() {
        // CodeRabbit's case: one bank bracket spans a reset inside a week and
        // the week's rollover. A full fold gives the spent credit to the
        // reset; a later write that keeps that week must not hand the same
        // credit to the pair across the rollover as well.
        let p = temp("redemption");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let e = 2_000_000.0;
        let credit = |status: &str| super::super::RawCredit {
            id: Some("c1".into()),
            status: status.into(),
            expires_at: Some(9e9),
        };
        let pass = |h: &mut History, at: f64, pct: f64, end: f64, bank: Option<&str>| {
            let mut o = obs(vec![lane("secondary_window", pct, at, end, WEEK)]);
            o.bank = bank.map(|s| BankReading {
                at,
                left: (s == "available") as u64,
                credits: Some(vec![credit(s)]),
            });
            h.record(&[o], &t, at).unwrap();
        };
        pass(&mut h, e - 4.0 * H, 40.0, e, Some("available"));
        pass(&mut h, e - 3.0 * H, 50.0, e, None);
        // Offline, 50 -> 20: a reset, and the credit is its.
        pass(&mut h, e - 2.0 * H, 20.0, e, None);
        pass(&mut h, e - H, 30.0, e, None);
        pass(&mut h, e + H, 5.0, e + WEEK, None);
        pass(&mut h, e + 2.0 * H, 8.0, e + WEEK, Some("spent"));
        pass(&mut h, e + 3.0 * H, 10.0, e + WEEK, None);
        let (stored, full) = stored_then_full(&mut h, "secondary_window");
        assert_eq!(stored.1, full.1);
        let banked = full.1.iter().filter(|r| r.contains("bank_fall")).count();
        assert_eq!(banked, 1, "{:#?}", full.1);
    }

    #[test]
    fn a_watched_flat_stretch_is_not_read_as_offline_when_it_moves() {
        // CodeRabbit's case: polled every 30 s, 80% for half an hour, then
        // 60%. Unchanged readings are not written until the hourly
        // heartbeat, so the stored pair was 1,800 s apart - past the 900 s
        // watch gap - and the 20-point fall passed the offline rule as a
        // reset. The pane saw every reading in between.
        let p = temp("watched-flat");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + 3.0 * 86_400.0;
        let mut at = a;
        while at < a + 1800.0 {
            h.record(&[obs(vec![lane("secondary_window", 80.0, at, end, WEEK)])], &t, at).unwrap();
            at += 30.0;
        }
        h.record(&[obs(vec![lane("secondary_window", 60.0, at, end, WEEK)])], &t, at).unwrap();
        assert_eq!(h.count("reset_event"), 0, "a watched 80 -> 60 is an adjustment, not a reset");
        let s = h.samples("codex", &obs(vec![]).account_key(), "secondary_window").unwrap();
        let ats: Vec<f64> = s.iter().map(|x| x.at - a).collect();
        // The first reading, the last unchanged one the pane saw at least
        // five minutes before the change, and the change: all real readings.
        assert_eq!(ats, vec![0.0, 1500.0, 1800.0]);
        // A pane that was not watching - nothing seen in between - still
        // reads the same pair as offline.
        let q = temp("unwatched-flat");
        let mut g = History::open(&q).unwrap();
        g.record(&[obs(vec![lane("secondary_window", 80.0, a, end, WEEK)])], &t, a).unwrap();
        g.record(&[obs(vec![lane("secondary_window", 60.0, a + 1800.0, end, WEEK)])], &t, a + 1800.0)
            .unwrap();
        assert_eq!(g.count("reset_event"), 1);
    }

    #[test]
    fn a_failed_pass_keeps_the_readings_its_bridge_was_made_from() {
        // CodeRabbit's case: the pass that writes a bridge also forgets the
        // readings held back for it. When that pass fails, the file rolls
        // back but the memory did not, so the retry had nothing to bridge
        // with and the watched fall read as an offline reset.
        let p = temp("seen-rollback");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + 3.0 * 86_400.0;
        let mut at = a;
        while at < a + 1800.0 {
            h.record(&[obs(vec![lane("secondary_window", 80.0, at, end, WEEK)])], &t, at).unwrap();
            at += 30.0;
        }
        // The change itself is refused, after the bridge before it was made.
        h.conn
            .execute_batch(&format!(
                "CREATE TEMP TRIGGER refuse BEFORE INSERT ON sample WHEN NEW.at = {at}
                 BEGIN SELECT RAISE(ABORT, 'refused'); END;"
            ))
            .unwrap();
        let change = || obs(vec![lane("secondary_window", 60.0, a + 1800.0, end, WEEK)]);
        assert!(h.record(&[change()], &t, at).is_err());
        h.conn.execute_batch("DROP TRIGGER refuse;").unwrap();
        h.record(&[change()], &t, at).unwrap();
        assert_eq!(h.count("reset_event"), 0, "the retried pass lost its watch evidence");
        let s = h.samples("codex", &obs(vec![]).account_key(), "secondary_window").unwrap();
        let ats: Vec<f64> = s.iter().map(|x| x.at - a).collect();
        assert_eq!(ats, vec![0.0, 1500.0, 1800.0]);
    }

    #[test]
    fn an_unchanged_bank_read_after_a_reset_is_kept_so_the_reset_reads_issued() {
        // Codex's and CodeRabbit's case: the bank is read before a reset and
        // again, unchanged, after it. Dropping the second reading left the
        // reset `unknown` for as long as the inventory stayed the same.
        let p = temp("bank-confirm");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + 3.0 * 86_400.0;
        let bank = |at: f64| BankReading {
            at,
            left: 1,
            credits: Some(vec![super::super::RawCredit {
                id: Some("c1".into()),
                status: "available".into(),
                expires_at: Some(9e9),
            }]),
        };
        let mut o = obs(vec![lane("secondary_window", 80.0, a, end, WEEK)]);
        o.bank = Some(bank(a));
        h.record(std::slice::from_ref(&o), &t, a).unwrap();
        // Ten minutes on, watching: back to zero, the same end, the same bank.
        let mut o = obs(vec![lane("secondary_window", 0.0, a + 600.0, end, WEEK)]);
        o.bank = Some(bank(a + 600.0));
        h.record(std::slice::from_ref(&o), &t, a + 600.0).unwrap();
        let kinds = || -> Vec<String> {
            let mut st = h.conn.prepare("SELECT kind FROM reset_event ORDER BY at_hi").unwrap();
            st.query_map([], |r| r.get(0)).unwrap().map(Result::unwrap).collect()
        };
        assert_eq!(kinds(), vec!["issued".to_string()]);
        // Two readings either side, no bank event: nothing was spent.
        assert_eq!((h.count("bank_sample"), h.count("bank_event")), (2, 0));
        // The same bank again, with no reset waiting on it: not written.
        let mut o = obs(vec![lane("secondary_window", 0.0, a + 1200.0, end, WEEK)]);
        o.bank = Some(bank(a + 1200.0));
        h.record(std::slice::from_ref(&o), &t, a + 1200.0).unwrap();
        assert_eq!(h.count("bank_sample"), 2);
    }

    #[test]
    fn a_unified_account_is_kept_under_the_vendor_and_under_spacexsi() {
        let p = temp("spacexsi");
        let mut h = History::open(&p).unwrap();
        let mut o = obs(vec![lane("weekly_pool", 40.0, 5000.0, 600000.0, WEEK)]);
        o.provider = "cursor".into();
        o.spacexsi = Some(crate::shared::Spacexsi::Shared);
        o.spacexsi_account = Some(o.account_key());
        h.record(&[o.clone()], &Thresholds::default(), 5000.0).unwrap();
        let key = o.account_key();
        let vendor = h.samples("cursor", &key, "weekly_pool").unwrap();
        let unified = h.samples("spacexsi", &key, "weekly_pool").unwrap();
        assert_eq!(vendor.len(), 1);
        assert_eq!(unified.len(), 1);
        assert_eq!(vendor[0].spacexsi.as_deref(), Some("shared"));
        assert_eq!(h.since("spacexsi", &key), Some(5000.0));
    }

    #[test]
    fn grok_keeps_one_vendor_pool_and_its_unified_copy_is_the_shared_weekly_pool() {
        let p = temp("grok-link");
        let mut h = History::open(&p).unwrap();
        let mut o = obs(vec![lane("credits", 40.0, 5000.0, 600000.0, WEEK)]);
        o.provider = "grok".into();
        h.record(&[o.clone()], &Thresholds::default(), 5000.0).unwrap();
        // Linked to the unified plan: same vendor pool, and SpaceXSI's copy
        // in the pool Cursor's reading of it fills.
        o.lanes = vec![lane("credits", 45.0, 9000.0, 600000.0, WEEK)];
        o.spacexsi = Some(crate::shared::Spacexsi::Shared);
        o.spacexsi_account = Some(o.account_key());
        h.record(&[o.clone()], &Thresholds::default(), 9000.0).unwrap();
        let key = o.account_key();
        assert_eq!(h.samples("grok", &key, "credits").unwrap().len(), 2);
        assert!(h.samples("grok", &key, "weekly_pool").unwrap().is_empty());
        assert_eq!(h.samples("spacexsi", &key, "weekly_pool").unwrap().len(), 1);
    }

    #[test]
    fn a_plan_change_on_the_unified_plan_closes_the_unified_cycle() {
        // Codex's case: the tab draws SpaceXSI's cycles, so a tier change
        // that leaves the window alone and lowers the reading has to close
        // the unified cycle as a plan change, not read there as a reset.
        let p = temp("spacexsi-plan");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let shared = Some(crate::shared::Spacexsi::Shared);
        let plan = |at: f64, tier: &str| Some(super::super::PlanReading { at, plan: tier.into() });
        let mut cursor = obs(vec![lane("weekly_pool", 60.0, 5000.0, 600000.0, WEEK)]);
        cursor.provider = "cursor".into();
        cursor.account_id = "cursor-acct".into();
        cursor.spacexsi = shared;
        cursor.spacexsi_account = Some(cursor.account_key());
        cursor.plan = plan(5000.0, "pro");
        let mut grok = obs(vec![lane("credits", 61.0, 5000.0, 600000.0, WEEK)]);
        grok.provider = "grok".into();
        grok.account_id = "grok-acct".into();
        grok.spacexsi = shared;
        grok.spacexsi_account = cursor.spacexsi_account.clone();
        grok.plan = plan(5000.0, "supergrok");
        h.record(&[cursor.clone(), grok.clone()], &t, 5000.0).unwrap();
        // The same two tiers again: neither vendor's is new, though they
        // differ from each other.
        cursor.lanes = vec![lane("weekly_pool", 62.0, 9000.0, 600000.0, WEEK)];
        cursor.plan = plan(9000.0, "pro");
        grok.lanes = vec![lane("credits", 62.0, 9000.0, 600000.0, WEEK)];
        grok.plan = plan(9000.0, "supergrok");
        h.record(&[cursor.clone(), grok.clone()], &t, 9000.0).unwrap();
        let key = cursor.account_key();
        let unified: i64 = h
            .conn
            .query_row("SELECT id FROM account WHERE provider = 'spacexsi'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(load_plans(&h.conn, unified).unwrap().len(), 1, "two vendors' tiers read as a change");
        // Cursor's tier moves up; the window and reset stay, and the
        // reading falls against the larger allowance.
        cursor.lanes = vec![lane("weekly_pool", 20.0, 20000.0, 600000.0, WEEK)];
        cursor.plan = plan(20000.0, "ultra");
        h.record(&[cursor.clone()], &t, 20000.0).unwrap();
        let cycles = h.cycles("spacexsi", &key, "weekly_pool").unwrap();
        assert!(
            cycles.iter().any(|c| c.end_reason == Some(EndReason::PlanChanged)),
            "the unified cycle did not close on the plan change"
        );
    }

    #[test]
    fn both_vendors_changing_tier_in_one_instant_is_one_plan_change_at_that_instant() {
        // CodeRabbit's case: both tiers change at T. The second row is stored
        // a representable instant after T, so the plan in force at T had
        // only the first change and the next sample closed the cycle again
        // a few ulps later.
        let t: f64 = 20_000.0;
        let next = f64::from_bits(t.to_bits() + 1);
        let row = |at: f64, plan: &str| PlanChange { at, plan: plan.into() };
        let rows = vec![
            row(5000.0, "cursor:pro"),
            row(f64::from_bits(5000f64.to_bits() + 1), "grok:supergrok"),
            row(t, "cursor:ultra"),
            row(next, "grok:heavy"),
            // One vendor alone still changes the plan at its own instant.
            row(30_000.0, "grok:supergrok"),
        ];
        let got = compose_unified_plans(rows);
        let got: Vec<(f64, &str)> = got.iter().map(|p| (p.at, p.plan.as_str())).collect();
        assert_eq!(
            got,
            vec![
                (5000.0, "cursor:pro grok:supergrok"),
                (t, "cursor:ultra grok:heavy"),
                (30_000.0, "cursor:ultra grok:supergrok"),
            ]
        );
    }

    #[test]
    fn a_shared_grok_read_without_cursor_keeps_to_the_plans_one_history() {
        let p = temp("shared-grok");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let shared = Some(crate::shared::Spacexsi::Shared);
        let mut cursor = obs(vec![lane("weekly_pool", 40.0, 5000.0, 600000.0, WEEK)]);
        cursor.provider = "cursor".into();
        cursor.account_id = "cursor-acct".into();
        cursor.spacexsi = shared;
        cursor.spacexsi_account = Some(cursor.account_key());
        let mut grok = obs(vec![lane("credits", 41.0, 5000.0, 600000.0, WEEK)]);
        grok.provider = "grok".into();
        grok.account_id = "grok-acct".into();
        grok.spacexsi = shared;
        grok.spacexsi_account = cursor.spacexsi_account.clone();
        h.record(&[cursor.clone(), grok.clone()], &t, 5000.0).unwrap();
        // Cursor not read this pass: Grok names no plan key of its own.
        grok.lanes = vec![lane("credits", 48.0, 9000.0, 600000.0, WEEK)];
        grok.spacexsi_account = None;
        h.record(&[grok.clone()], &t, 9000.0).unwrap();
        let plan = cursor.account_key();
        let accounts: i64 = h
            .conn
            .query_row("SELECT COUNT(*) FROM account WHERE provider = 'spacexsi'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(accounts, 1, "the unified plan's history forked");
        let pcts: Vec<f64> = h
            .samples("spacexsi", &plan, "weekly_pool")
            .unwrap()
            .iter()
            .filter_map(|s| s.pct)
            .collect();
        assert_eq!(pcts, vec![40.0, 48.0]);
        assert_eq!(h.spacexsi_key_of(&grok), Some(plan));
    }

    #[test]
    fn a_money_pool_keeps_its_unit_and_amounts() {
        let p = temp("money");
        let mut h = History::open(&p).unwrap();
        let mut l = lane("extra_usage", 20.0, 5000.0, 0.0, 0.0);
        l.reset = None;
        l.window_secs = None;
        l.unit = Unit::Money {
            currency: "AUD".into(),
            exponent: 2,
        };
        l.role = PoolRole::Overage;
        l.used = Some(10.0);
        l.limit = Some(50.0);
        h.record(&[obs(vec![l])], &Thresholds::default(), 5000.0).unwrap();
        let (unit, cur, exp, role): (String, String, i64, String) = h
            .conn
            .query_row("SELECT unit, currency, exponent, role FROM pool", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .unwrap();
        assert_eq!(
            (unit.as_str(), cur.as_str(), exp, role.as_str()),
            ("money", "AUD", 2, "overage")
        );
        let c = h.cycles("codex", &obs(vec![]).account_key(), "extra_usage").unwrap();
        assert_eq!(c[0].model, CycleModel::Undated);
        assert_eq!(c[0].used_amount, Some(10.0));
    }

    #[test]
    fn two_panes_writing_one_file_at_once_open_one_cycle_and_lose_nothing() {
        let p = temp("race");
        History::open(&p).unwrap();
        let a = 1_000_000.0;
        let writers: Vec<_> = (0..2)
            .map(|w| {
                let p = p.clone();
                std::thread::spawn(move || {
                    let mut h = History::open(&p).unwrap();
                    let t = Thresholds::default();
                    // Both panes see the same fetches, as they do when one
                    // shares its reading with the other, plus a pool each.
                    for i in 0..40 {
                        let at = a + i as f64 * 400.0;
                        let o = obs(vec![
                            lane("secondary_window", i as f64, at, a + WEEK, WEEK),
                            lane(&format!("own_{w}"), i as f64, at, a + WEEK, WEEK),
                        ]);
                        loop {
                            match h.record(&[o.clone()], &t, at) {
                                Ok(()) => break,
                                Err(why) if busy(&why) => std::thread::yield_now(),
                                Err(why) => panic!("{why}"),
                            }
                        }
                    }
                })
            })
            .collect();
        for w in writers {
            w.join().unwrap();
        }
        let h = History::open(&p).unwrap();
        let key = obs(vec![]).account_key();
        assert_eq!(h.samples("codex", &key, "secondary_window").unwrap().len(), 40);
        assert_eq!(h.samples("codex", &key, "own_0").unwrap().len(), 40);
        assert_eq!(h.samples("codex", &key, "own_1").unwrap().len(), 40);
        let open: i64 = h
            .conn
            .query_row("SELECT COUNT(*) FROM cycle WHERE closed_at IS NULL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(open, 3, "one open cycle per pool");
        assert_eq!(h.cycles("codex", &key, "secondary_window").unwrap().len(), 1);
        assert_eq!(h.count("account"), 1);
    }

    fn replayed(at: f64, pct: f64, reset: f64) -> backfill::Replayed {
        backfill::Replayed {
            pool_key: "secondary_window".into(),
            label: "7d".into(),
            unit: Unit::Pct,
            role: PoolRole::Included,
            sample: Sample {
                at,
                pct: Some(pct),
                used: None,
                limit: None,
                window_secs: Some(WEEK),
                reset_at: Some(reset),
                source: Source::Backfill,
                spacexsi: None,
            },
        }
    }

    #[test]
    fn a_backfill_older_than_the_live_samples_refolds_and_never_writes_twice() {
        let p = temp("backfill");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        // Live readings from weeks three and four only.
        let mut live_at = Vec::new();
        for k in 2..4 {
            let base = a + k as f64 * WEEK;
            let at = base + WEEK - 2.0 * H;
            live_at.push(at);
            h.record(&[obs(vec![lane("secondary_window", 30.0, at, base + WEEK, WEEK)])], &t, at)
                .unwrap();
        }
        let key = obs(vec![]).account_key();
        assert_eq!(h.cycles("codex", &key, "secondary_window").unwrap().len(), 2);
        // Weeks one and two from the rollouts, arriving after them, plus a
        // snapshot at the very instant a live sample was fetched.
        let rows = vec![
            replayed(a + 2.0 * H, 10.0, a + WEEK),
            replayed(a + WEEK - 2.0 * H, 60.0, a + WEEK),
            replayed(a + WEEK + 2.0 * H, 5.0, a + 2.0 * WEEK),
            replayed(a + 2.0 * WEEK - 2.0 * H, 80.0, a + 2.0 * WEEK),
            replayed(live_at[0], 99.0, a + 3.0 * WEEK),
        ];
        assert_eq!(h.replay("codex", &key, "CODEX", &rows, &t, a + 4.0 * WEEK).unwrap(), 4);
        let samples = h.samples("codex", &key, "secondary_window").unwrap();
        assert_eq!(samples.len(), 6);
        // The live sample kept its reading; the snapshot at its key did not
        // replace it.
        let kept = samples.iter().find(|s| s.at == live_at[0]).unwrap();
        assert_eq!((kept.source, kept.pct), (Source::Live, Some(30.0)));
        assert_eq!(samples[0].source, Source::Backfill);
        // Re-folded: four weeks, the same as folding every sample at once.
        let cycles = h.cycles("codex", &key, "secondary_window").unwrap();
        assert_eq!(cycles.len(), 4);
        assert_eq!(cycles[0].peak_pct, Some(60.0));
        assert_eq!(cycles[1].peak_pct, Some(80.0));
        let fresh = fold::fold(&FoldInput {
            role: Role::Included,
            samples: &samples,
            plans: &[],
            resets: &[],
            prev: None,
            next_ordinal: 1,
            rolling_seed: None,
            resume: None,
            gone_at: None,
            thresholds: t,
        });
        let strip = |v: Vec<Cycle>| -> Vec<Cycle> {
            v.into_iter()
                .map(|mut c| {
                    c.segments.clear();
                    c
                })
                .collect()
        };
        assert_eq!(strip(cycles), strip(fresh.cycles));
        assert_eq!(h.since("codex", &key), Some(a + 2.0 * H));
        // Recorded as run, with what the account holds.
        let mk = backfill::meta_key(&key);
        assert_eq!(h.meta(&mk).unwrap(), Some(backfill::done(4)));
        // The same replay again writes nothing and still says four.
        assert_eq!(h.replay("codex", &key, "CODEX", &rows, &t, a + 4.0 * WEEK).unwrap(), 4);
        assert_eq!(h.samples("codex", &key, "secondary_window").unwrap().len(), 6);
        assert_eq!(h.count("account"), 1, "the live account, not a second one");
    }

    #[test]
    fn a_backfill_reset_beside_a_live_reading_is_not_thinned_away() {
        // CodeRabbit's case: rollouts at 1000 (90%), 1400 (0%) and 1600
        // (0%), and a live 90% stored at 1300. The pre-pass dropped 1600 as
        // within five minutes of 1400; the replay then refused 1400 as
        // within five minutes of 1300, and the zero - the reset - was lost.
        let p = temp("backfill-thin");
        let mut h = History::open(&p).unwrap();
        let t = Thresholds::default();
        let a = 1_000_000.0;
        let end = a + 3.0 * 86_400.0;
        h.record(&[obs(vec![lane("secondary_window", 90.0, a + 1300.0, end, WEEK)])], &t, a + 1300.0)
            .unwrap();
        let key = obs(vec![]).account_key();
        let rows = vec![
            replayed(a + 1000.0, 90.0, end),
            replayed(a + 1400.0, 0.0, end),
            replayed(a + 1600.0, 0.0, end),
        ];
        // As collect hands them over: in order, and not thinned.
        h.replay("codex", &key, "CODEX", &rows, &t, a + 2000.0).unwrap();
        let s = h.samples("codex", &key, "secondary_window").unwrap();
        let got: Vec<(f64, Option<f64>)> = s.iter().map(|x| (x.at - a, x.pct)).collect();
        assert_eq!(got, vec![(1000.0, Some(90.0)), (1300.0, Some(90.0)), (1600.0, Some(0.0))]);
        // And into a store with no live readings it writes what the write
        // rule alone keeps.
        let q = temp("backfill-alone");
        let mut g = History::open(&q).unwrap();
        g.replay("codex", &key, "CODEX", &rows, &t, a + 2000.0).unwrap();
        let alone: Vec<f64> = g.samples("codex", &key, "secondary_window").unwrap().iter().map(|x| x.at).collect();
        let model: Vec<f64> = backfill::thin(rows).iter().map(|r| r.sample.at).collect();
        assert_eq!(alone, model);
    }

    #[test]
    fn only_one_pane_claims_the_backfill() {
        let p = temp("claim");
        let mut one = History::open(&p).unwrap();
        let mut two = History::open(&p).unwrap();
        let mk = backfill::meta_key("k");
        let claim = |h: &mut History, now: f64| {
            h.meta_update(&mk, |v| backfill::due(v, now).then(|| backfill::running(now)))
                .unwrap()
        };
        assert!(claim(&mut one, 1000.0));
        assert!(!claim(&mut two, 1001.0));
        // A claim nobody renewed runs out.
        assert!(claim(&mut two, 1000.0 + backfill::LEASE));
        assert_eq!(one.meta(&mk).unwrap(), Some(backfill::running(1000.0 + backfill::LEASE)));
    }

    #[test]
    fn the_recorder_backfills_from_the_rollouts_off_the_pass_and_says_so() {
        let p = temp("runner");
        let roots = p.parent().unwrap().join("sessions");
        std::fs::create_dir_all(&roots).unwrap();
        let o = obs(vec![]);
        let snap = |stamp: &str, pct: f64| {
            format!(
                r#"{{"timestamp":"{stamp}","type":"event_msg","payload":{{"type":"token_count","rate_limits":{{"limit_id":"codex","primary":{{"used_percent":{pct},"window_minutes":10080,"resets_at":1790000000}}}}}}}}"#
            )
        };
        let body = [
            format!(r#"{{"type":"session_meta","payload":{{"creator_account_id":"{}"}}}}"#, o.account_id),
            snap("2026-09-10T08:00:00Z", 5.0),
            "{\"rate_limits\" broken".to_string(),
            snap("2026-09-10T09:00:00Z", 9.0),
        ]
        .join("\n");
        std::fs::write(roots.join("rollout-1.jsonl"), body).unwrap();
        let t = Thresholds::default();
        let mut r = super::super::Recorder::with_rollouts(p.clone(), vec![roots.clone()]);
        let now = opscope_core::now();
        let first = r.pass(std::slice::from_ref(&o), &t, now);
        assert_eq!(first.notes.get("codex").map(String::as_str), Some("backfilling from rollouts"));
        // A second pane on the same file does not read them again.
        let mut other = super::super::Recorder::with_rollouts(p.clone(), vec![roots.clone()]);
        let said = other.pass(std::slice::from_ref(&o), &t, now);
        assert_eq!(said.notes.get("codex").map(String::as_str), Some("backfilling from rollouts"));
        let mut done = None;
        for _ in 0..500 {
            let s = r.pass(std::slice::from_ref(&o), &t, opscope_core::now());
            let note = s.notes.get("codex").cloned().unwrap_or_default();
            if note != "backfilling from rollouts" {
                done = Some(s);
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        let done = done.expect("the backfill landed");
        assert_eq!(
            done.notes.get("codex").map(String::as_str),
            Some("2 backfilled from rollouts · 1 lines skipped")
        );
        let at = crate::iso_epoch("2026-09-10T08:00:00Z").unwrap();
        assert_eq!(done.since.get("codex"), Some(&at));
        // The other pane learns it from the file.
        let later = other.pass(std::slice::from_ref(&o), &t, opscope_core::now());
        assert_eq!(later.notes.get("codex").map(String::as_str), Some("2 backfilled from rollouts"));
        // An account with no id of its own is never backfilled: nothing
        // ties a rollout to it.
        let mut anon = o.clone();
        anon.account_id.clear();
        let mut third = super::super::Recorder::with_rollouts(temp("anon"), vec![roots]);
        assert!(third.pass(&[anon], &t, opscope_core::now()).notes.is_empty());
    }
}
