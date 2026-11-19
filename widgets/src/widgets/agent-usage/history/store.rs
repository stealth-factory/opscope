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

use std::path::{Path, PathBuf};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};

use super::fold::{
    self, Cycle, CycleModel, EndReason, FoldInput, PlanChange, Role, Sample, Source, StartSource, Thresholds,
};
use super::{BankReading, Observation, WatchReading, backfill, overage, resets};
use crate::shared::Lane;

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
    use std::os::unix::fs::OpenOptionsExt;
    let lock_path = PathBuf::from(format!("{}.lock", path.display()));
    let lock = std::fs::OpenOptions::new()
        .write(true)
        .create(true)
        .truncate(false)
        .mode(0o600)
        .open(&lock_path)
        .map_err(|e| format!("cannot lock the damaged file: {e}"))?;
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
    for side in ["-wal", "-shm"] {
        let from = PathBuf::from(format!("{}{side}", path.display()));
        if from.exists() {
            // A side file that cannot follow is removed rather than left to
            // be replayed into the new file; the main file is already kept.
            let to = PathBuf::from(format!("{}{side}", aside.display()));
            if move_new(&from, &to).is_err() {
                let _ = std::fs::remove_file(&from);
            }
        }
    }
    Ok(Some(aside))
}

impl History {
    /// Open the history, making and migrating it as needed. A damaged file
    /// is renamed and a new one started; a newer one is left alone.
    pub fn open(path: &Path) -> Result<History, OpenError> {
        let mut tries = 0;
        loop {
            match History::open_once(path) {
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
                    let mut h = History::open_once(path).map_err(|f| f.error)?;
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

    fn open_once(path: &Path) -> Result<History, Failure> {
        use std::os::unix::fs::OpenOptionsExt;
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| Failure::other(format!("cannot make {}: {e}", dir.display())))?;
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
        History::open_conn(path, existed).map_err(|mut f| {
            f.identity = id;
            f
        })
    }

    fn open_conn(path: &Path, existed: bool) -> Result<History, Failure> {
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
        };
        h.refold_if_detector_changed(&Thresholds::default())
            .map_err(Failure::other)?;
        Ok(h)
    }

    /// Fold every pool again when the file was folded by another detector.
    fn refold_if_detector_changed(&mut self, t: &Thresholds) -> Result<(), String> {
        // Read as text, as `meta_set` writes it; anything unreadable is
        // simply not this detector.
        let stamped = |c: &Connection| -> Result<Option<i64>, String> {
            let v: Option<String> = c
                .query_row(
                    "SELECT CAST(v AS TEXT) FROM meta WHERE k = 'detector_version'",
                    [],
                    |r| r.get(0),
                )
                .optional()
                .map_err(sql)?;
            Ok(v.and_then(|v| v.parse().ok()))
        };
        if stamped(&self.conn)? == Some(fold::DETECTOR_VERSION) {
            return Ok(());
        }
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        // Another pane may have done it while this one waited.
        if stamped(&tx)? != Some(fold::DETECTOR_VERSION) {
            let pools: Vec<i64> = {
                let mut st = tx.prepare("SELECT id FROM pool ORDER BY id").map_err(sql)?;
                let rows = st.query_map([], |r| r.get(0)).map_err(sql)?;
                rows.collect::<Result<_, _>>().map_err(sql)?
            };
            for pool in pools {
                refold_pool(&tx, pool, None, t)?;
            }
            meta_set(&tx, "detector_version", &fold::DETECTOR_VERSION.to_string())?;
            meta_set(&tx, "writer", env!("CARGO_PKG_VERSION"))?;
            meta_set(
                &tx,
                "clocks",
                "sample.at, bank_sample.at, plan_sample.at, watch_state.at: this machine's wall clock at fetch; reset_at: the server's",
            )?;
        }
        tx.commit().map_err(sql)
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
    pub fn record(&mut self, obs: &[Observation], t: &Thresholds, now: f64) -> Result<(), String> {
        let tx = self
            .conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(sql)?;
        let mut refold: Vec<(i64, f64)> = Vec::new();
        let mut account_refold: Vec<(i64, f64)> = Vec::new();
        let mut banks_changed: Vec<i64> = Vec::new();
        for o in obs {
            let key = o.account_key();
            let account = upsert_account(&tx, &o.provider, &key, &o.heading, now)?;
            let sx = o.spacexsi.map(super::spacexsi_name);
            write_lanes(&tx, account, &o.lanes, sx, &mut refold)?;
            // On the unified plan the lanes are kept twice (§3.4): under the
            // vendor, and under SpaceXSI, where its cycles are derived.
            if let Some(sk) = &o.spacexsi_account {
                let unified = upsert_account(&tx, "spacexsi", sk, SPACEXSI_HEADING, now)?;
                write_lanes(&tx, unified, &o.lanes, sx, &mut refold)?;
            }
            if let Some(b) = &o.bank {
                if write_bank(&tx, account, b)? {
                    banks_changed.push(account);
                    account_refold.push((account, b.at));
                }
            }
            if let Some(p) = &o.plan {
                if let Some(at) = write_plan(&tx, account, p)? {
                    account_refold.push((account, at));
                }
            }
            if let Some(w) = &o.watch {
                write_watch(&tx, account, w)?;
            }
            // Phase 4's seams: which included pools an overage pool serves,
            // and per-request charges - under the vendor and, on the unified
            // plan, under SpaceXSI as its lanes are. A link that is new folds
            // the overage pool's spans again from the start.
            let mut owners = vec![account];
            if let Some(sk) = &o.spacexsi_account {
                owners.push(upsert_account(&tx, "spacexsi", sk, SPACEXSI_HEADING, now)?);
            }
            for &owner in &owners {
                overage::write_links_and_charges(&tx, owner, o, &mut refold)?;
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
        // An included pool folded again moves what its overage pools'
        // spans were exhausted against.
        overage::refold_linked(&tx, &mut refold)?;
        // One re-fold per pool, from its earliest point.
        refold.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.total_cmp(&b.1)));
        refold.dedup_by_key(|x| x.0);
        for (pool, from) in refold {
            refold_pool(&tx, pool, Some(from), t)?;
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
             first_seen = MIN(first_seen, excluded.first_seen), last_seen = MAX(last_seen, excluded.last_seen)
         RETURNING id",
        params![
            account,
            lane.pool_key,
            lane.label,
            lane.role.name(),
            lane.unit.name(),
            currency,
            exponent,
            at
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
    rows.collect::<Result<_, _>>().map_err(sql)
}

/// Write one lane per pool, under the five-minute and change-or-heartbeat
/// rules, noting which pools need folding from where.
fn write_lanes(
    tx: &Transaction,
    account: i64,
    lanes: &[Lane],
    spacexsi: Option<&str>,
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
        if write_sample(tx, pool, &sample)? {
            refold.push((pool, sample.at));
        }
    }
    Ok(())
}

/// Insert `s` when [`super::should_write`] says so against the stored
/// samples either side of it. True when it was written.
fn write_sample(tx: &Transaction, pool: i64, s: &Sample) -> Result<bool, String> {
    let before: Option<Sample> = tx
        .query_row(
            &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at <= ?2 ORDER BY at DESC LIMIT 1"),
            params![pool, s.at],
            sample_row,
        )
        .optional()
        .map_err(sql)?;
    let after: Option<Sample> = tx
        .query_row(
            &format!("SELECT {SAMPLE_COLS} FROM sample WHERE pool_id = ?1 AND at > ?2 ORDER BY at LIMIT 1"),
            params![pool, s.at],
            sample_row,
        )
        .optional()
        .map_err(sql)?;
    if !super::should_write(before.as_ref(), after.as_ref(), s) {
        return Ok(false);
    }
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

/// Write a bank reading when it differs from the latest one stored and is
/// newer than it. True when it was written.
fn write_bank(tx: &Transaction, account: i64, b: &BankReading) -> Result<bool, String> {
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
        if b.at <= *at || (*left as u64 == b.left && *was == credits) {
            return Ok(false);
        }
    }
    let n = tx
        .execute(
            "INSERT OR IGNORE INTO bank_sample(account_id, at, left, credits) VALUES (?1, ?2, ?3, ?4)",
            params![account, b.at, b.left as i64, credits],
        )
        .map_err(sql)?;
    Ok(n > 0)
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
    rows.collect::<Result<_, _>>().map_err(sql)
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
                    -- Derived, not a column (§5.7): a reset that began in this
                    -- cycle with no known figure before it.
                    EXISTS (SELECT 1 FROM reset_event e
                             WHERE e.pool_id = cycle.pool_id AND e.at_lo >= cycle.starts_at
                               AND (cycle.closed_at IS NULL OR e.at_lo < cycle.closed_at)
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
/// Kept: the closed, observed cycles whose last observation is before
/// `from`, less the last of them - the cycle before the point is folded
/// again, since a sample just before a boundary can move it - and the
/// hollow cycles between them. Everything after is deleted and rebuilt from
/// the samples after the last kept observation.
fn refold_pool(tx: &Transaction, pool: i64, from: Option<f64>, t: &Thresholds) -> Result<(), String> {
    let (account, role, provider): (i64, String, String) = tx
        .query_row(
            "SELECT p.account_id, p.role, a.provider FROM pool p JOIN account a ON a.id = p.account_id WHERE p.id = ?1",
            [pool],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .map_err(sql)?;
    let role = Role::from_name(&role);
    // The second-latest closed, observed cycle seen before `from`: one
    // query, so a pool with years of five-hour cycles does not load them
    // all on every write.
    let kept: Option<i64> = match from {
        None => None,
        Some(from) => tx
            .query_row(
                "SELECT ordinal FROM cycle
                  WHERE pool_id = ?1 AND closed_at IS NOT NULL AND last_obs_at IS NOT NULL AND last_obs_at < ?2
                  ORDER BY ordinal DESC LIMIT 1 OFFSET 1",
                params![pool, from],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?,
    };
    let prev: Option<Cycle> = match kept {
        Some(ordinal) => load_cycles_from(tx, pool, ordinal)?.into_iter().next().map(|(_, c)| c),
        None => None,
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
    let after = prev.as_ref().and_then(|c| c.last_obs_at);
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
    let before: Option<Sample> = match after {
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
    let plans = load_plans(tx, account)?;
    let banks = load_banks(tx, account)?;
    // Phase 2's seam: the resets in these samples.
    let found = resets::detect(&resets::DetectInput {
        before: before.as_ref(),
        samples: &samples,
        banks: &banks,
        plans: &plans,
        thresholds: *t,
        role,
        effect_override: t.reset_effect.get(&provider).map(resets::EffectOverride::name),
    });
    let out = fold::fold(&FoldInput {
        role,
        samples: &samples,
        plans: &plans,
        resets: &found,
        prev: prev.as_ref(),
        next_ordinal: keep_ordinal as u32 + 1,
        thresholds: *t,
    });
    let mut ids: Vec<(i64, &Cycle)> = Vec::new();
    for c in &out.cycles {
        ids.push((insert_cycle(tx, pool, c)?, c));
    }
    for e in &found {
        // The cycle it happened in: the one its earlier reading belongs to,
        // which a restarted reset closed (§3.6) - the kept one when that
        // reading is the kept cycle's last.
        let cycle = ids
            .iter()
            .rev()
            .find(|(_, c)| c.starts_at <= e.at_lo)
            .map(|(id, _)| *id)
            .or(kept_id);
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
        for inc in linked {
            // From the reading in force at the re-fold point, not after it.
            let from: Option<f64> = match after {
                Some(at) => tx
                    .query_row(
                        "SELECT MAX(at) FROM sample WHERE pool_id = ?1 AND at <= ?2",
                        params![inc, at],
                        |r| r.get(0),
                    )
                    .map_err(sql)?,
                None => None,
            };
            loaded.push((inc, load_samples(tx, inc, from.map(|f| f - 1e-3).or(after))?));
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
        for span in overage::spans(role, &own, &included, t) {
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
        });
        assert_eq!(found.len(), 1);
        let fresh = fold::fold(&FoldInput {
            role: Role::Included,
            samples: &all,
            plans: &[],
            resets: &found,
            prev: None,
            next_ordinal: 1,
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
