//! Samples this widget measured itself, and the projection built from them.
//!
//! The file is `usage.json` under the shared state directory. Two panes
//! read, union by minute, and write it back under `statefile::with_lock`.
//! Nothing here opens `/proc`. A series is keyed by mount, type and
//! `f_blocks`, so a resize starts a new one instead of projecting the old
//! size forward.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use opscope_core as tc;

const MINUTE: f64 = 60.0;
const QUARTER: f64 = 15.0 * MINUTE;
const DAY: f64 = 86_400.0;
const RETAIN: f64 = 30.0 * DAY;
const MIN_SPAN: f64 = 15.0 * MINUTE;
const TEN_YEARS: f64 = 10.0 * 365.25 * DAY;

#[derive(Clone, Debug, PartialEq)]
pub struct Series {
    pub since: f64,
    pub samples: Vec<(f64, u64)>,
}

impl Default for Series {
    fn default() -> Self {
        Self {
            since: 0.0,
            samples: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Store {
    pub series: BTreeMap<String, Series>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slope {
    pub per_s: f64,
    pub span_s: f64,
    pub samples: usize,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Eta {
    NotFilling,
    Measuring(f64),
    In(f64),
}

/// Mount, filesystem type and block count. A grown or reformatted
/// filesystem does not continue the old line.
pub fn key(mount: &str, fstype: &str, blocks: u64) -> String {
    format!("{mount}\u{1f}{fstype}\u{1f}{blocks}")
}

fn minute_of(t: f64) -> i64 {
    // Nearest minute, so a sample 30s after another lands in that minute
    // and replaces it. Flooring would split 1000 and 1030 across a boundary
    // they do not sit on.
    (t / MINUTE).round() as i64
}

/// Keep one sample in each minute. A second reading in the same minute
/// replaces the first and is not a new sample. Returns whether a minute
/// was added.
pub fn record(series: &mut Series, t: f64, used: u64) -> bool {
    if !t.is_finite() {
        return false;
    }
    if series.samples.is_empty() {
        series.since = t;
        series.samples.push((t, used));
        return true;
    }
    let minute = minute_of(t);
    let last = series.samples.last().map(|(at, _)| minute_of(*at));
    if last == Some(minute) && t >= series.samples.last().map(|(at, _)| *at).unwrap_or(t) {
        series.samples.pop();
        series.samples.push((t, used));
        return false;
    }
    if last.is_some_and(|at| minute < at) {
        let replaced = series
            .samples
            .iter()
            .any(|(at, _)| minute_of(*at) == minute);
        series.samples.retain(|(at, _)| minute_of(*at) != minute);
        series.samples.push((t, used));
        series.samples.sort_by(|a, b| a.0.total_cmp(&b.0));
        return !replaced;
    }
    series.samples.push((t, used));
    true
}

/// One sample a minute for the last day, one every fifteen minutes before
/// that, and nothing older than thirty days.
pub fn compact(series: &mut Series, now: f64) {
    let oldest = now - RETAIN;
    let fine = now - DAY;
    series
        .samples
        .retain(|(t, _)| *t >= oldest && t.is_finite());
    let mut kept: Vec<(f64, u64)> = Vec::new();
    let mut fine_bucket: Option<i64> = None;
    let mut coarse_bucket: Option<i64> = None;
    for sample in series.samples.drain(..) {
        if sample.0 >= fine {
            let bucket = minute_of(sample.0);
            if fine_bucket == Some(bucket) {
                kept.pop();
            }
            fine_bucket = Some(bucket);
            kept.push(sample);
        } else {
            let bucket = (sample.0 / QUARTER).floor() as i64;
            if coarse_bucket == Some(bucket) {
                kept.pop();
            }
            coarse_bucket = Some(bucket);
            kept.push(sample);
        }
    }
    series.samples = kept;
    if let Some((t, _)) = series.samples.first() {
        if series.since < *t {
            series.since = *t;
        }
    }
}

/// A hole longer than `window_s` means the widget was not watching. Samples
/// before that hole are dropped and `since` moves to the first sample after
/// it. The caller passes a copy when the file should keep the older points
/// for a longer window.
pub fn restart_after_gap(series: &mut Series, window_s: f64) {
    if window_s <= 0.0 || series.samples.len() < 2 {
        return;
    }
    let mut restart: Option<usize> = None;
    for (index, pair) in series.samples.windows(2).enumerate() {
        if pair[1].0 - pair[0].0 > window_s {
            restart = Some(index + 1);
        }
    }
    if let Some(at) = restart {
        series.samples.drain(0..at);
        if let Some((t, _)) = series.samples.first() {
            series.since = *t;
        }
    }
}

/// Least-squares bytes per second over the samples inside the window, after
/// a gap longer than the window has restarted the series. `None` when fewer
/// than two samples remain or they span under fifteen minutes.
pub fn slope_bytes_per_s(series: &Series, window_s: f64, now: f64) -> Option<Slope> {
    let mut series = series.clone();
    restart_after_gap(&mut series, window_s);
    let start = now - window_s;
    let points: Vec<(f64, u64)> = series
        .samples
        .iter()
        .copied()
        .filter(|(t, _)| *t >= start && *t <= now)
        .collect();
    if points.len() < 2 {
        return None;
    }
    let span = points.last()?.0 - points.first()?.0;
    if span < MIN_SPAN {
        return None;
    }
    Some(Slope {
        per_s: least_squares(&points),
        span_s: span,
        samples: points.len(),
    })
}

/// How long the samples inside the window actually cover, after a gap reset.
/// Used to say `measuring 4m` when [`slope_bytes_per_s`] has refused the fit.
pub fn covered(series: &Series, window_s: f64, now: f64) -> (f64, usize) {
    let mut series = series.clone();
    restart_after_gap(&mut series, window_s);
    let start = now - window_s;
    let points: Vec<f64> = series
        .samples
        .iter()
        .map(|(t, _)| *t)
        .filter(|t| *t >= start && *t <= now)
        .collect();
    match (points.first(), points.last()) {
        (Some(first), Some(last)) if points.len() >= 2 => (last - first, points.len()),
        (Some(_), Some(_)) => (0.0, points.len()),
        _ => (0.0, 0),
    }
}

fn least_squares(points: &[(f64, u64)]) -> f64 {
    let n = points.len() as f64;
    let mean_t = points.iter().map(|point| point.0).sum::<f64>() / n;
    let mean_u = points.iter().map(|point| point.1 as f64).sum::<f64>() / n;
    let mut num = 0.0;
    let mut den = 0.0;
    for (t, used) in points {
        let dt = t - mean_t;
        num += dt * (*used as f64 - mean_u);
        den += dt * dt;
    }
    if den == 0.0 {
        0.0
    } else {
        num / den
    }
}

pub fn eta(usable: u64, slope: Option<Slope>) -> Eta {
    let Some(slope) = slope else {
        return Eta::Measuring(0.0);
    };
    if slope.span_s < MIN_SPAN || slope.samples < 2 || slope.per_s <= 0.0 {
        if slope.per_s <= 0.0 && slope.span_s >= MIN_SPAN && slope.samples >= 2 {
            return Eta::NotFilling;
        }
        return Eta::Measuring(slope.span_s);
    }
    let secs = usable as f64 / slope.per_s;
    if !secs.is_finite() || secs > TEN_YEARS {
        return Eta::NotFilling;
    }
    Eta::In(secs)
}

/// `↑` when the one-hour ETA is sooner than the window ETA by at least 25%,
/// `↓` when it is later by at least that much. Exactly 25% counts.
pub fn arrow(eta_window: Eta, eta_hour: Eta) -> Option<char> {
    let (Eta::In(window), Eta::In(hour)) = (eta_window, eta_hour) else {
        return None;
    };
    if window <= 0.0 {
        return None;
    }
    if hour <= window * 0.75 {
        Some('↑')
    } else if hour >= window * 1.25 {
        Some('↓')
    } else {
        None
    }
}

/// Union by minute. The later timestamp in a shared minute wins. `since` stays
/// at the earlier start, so one pane cannot erase the other's history.
pub fn union_series(into: &mut Series, extra: &Series) {
    if extra.samples.is_empty() {
        return;
    }
    if into.samples.is_empty() {
        *into = extra.clone();
        return;
    }
    if extra.since > 0.0 && (into.since == 0.0 || extra.since < into.since) {
        into.since = extra.since;
    }
    for (t, used) in &extra.samples {
        let minute = minute_of(*t);
        if let Some(slot) = into
            .samples
            .iter_mut()
            .find(|sample| minute_of(sample.0) == minute)
        {
            if *t >= slot.0 {
                *slot = (*t, *used);
            }
        } else {
            into.samples.push((*t, *used));
        }
    }
    into.samples.sort_by(|a, b| a.0.total_cmp(&b.0));
}

pub fn union_store(into: &mut Store, extra: &Store) {
    for (key, series) in &extra.series {
        union_series(into.series.entry(key.clone()).or_default(), series);
    }
}

pub fn usage_path() -> PathBuf {
    tc::state_dir().join("disk").join("usage.json")
}

pub struct Loaded {
    pub store: Store,
    pub note: String,
}

pub fn load_usage(path: &Path) -> Loaded {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            return Loaded {
                store: Store::default(),
                note: String::new(),
            };
        }
        Err(err) => {
            quarantine(path);
            return Loaded {
                store: Store::default(),
                note: fresh_note(&err.to_string()),
            };
        }
    };
    match parse_store(&text) {
        Ok(store) => Loaded {
            store,
            note: String::new(),
        },
        Err(reason) => {
            quarantine(path);
            Loaded {
                store: Store::default(),
                note: fresh_note(&reason),
            }
        }
    }
}

fn fresh_note(reason: &str) -> String {
    format!(
        "saved history would not read ({}) · started fresh · the old file is usage.json.bad",
        public_reason(reason)
    )
}

fn public_reason(err: &str) -> String {
    err.rsplit(": ").next().unwrap_or(err).trim().to_string()
}

fn quarantine(path: &Path) {
    let Some(parent) = path.parent() else {
        return;
    };
    let dest = parent.join("usage.json.bad");
    let _ = std::fs::rename(path, dest);
}

/// Read the file, union it with `memory`, compact, and write. The merged
/// store is what both panes now agree on. An error leaves `memory` untouched.
pub fn save_usage(path: &Path, memory: &Store) -> Result<Store, String> {
    match tc::statefile::with_lock(path, || {
        let mut merged = memory.clone();
        if let Ok(text) = std::fs::read_to_string(path) {
            if let Ok(on_disk) = parse_store(&text) {
                union_store(&mut merged, &on_disk);
            } else {
                quarantine(path);
            }
        }
        let now = tc::now();
        for series in merged.series.values_mut() {
            compact(series, now);
        }
        let body = render_store(&merged).map_err(|err| format!("could not save history: {err}"))?;
        tc::statefile::write_atomic(path, &body)
            .map_err(|err| format!("could not save history: {}", public_reason(&err)))?;
        Ok(merged)
    }) {
        Ok(inner) => inner,
        Err(err) => Err(format!("could not save history: {}", public_reason(&err))),
    }
}

fn parse_store(text: &str) -> Result<Store, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|err| err.to_string())?;
    let version = value
        .get("version")
        .and_then(|v| v.as_u64())
        .ok_or_else(|| "missing version".to_string())?;
    if version != 1 {
        return Err(format!("version {version}"));
    }
    let mut store = Store::default();
    let Some(series) = value.get("series").and_then(|v| v.as_object()) else {
        return Err("missing series".into());
    };
    for (key, entry) in series {
        let since = entry
            .get("since")
            .and_then(|v| v.as_f64())
            .filter(|t| t.is_finite())
            .ok_or_else(|| format!("{key}: since"))?;
        let samples = entry
            .get("samples")
            .and_then(|v| v.as_array())
            .ok_or_else(|| format!("{key}: samples"))?;
        let mut parsed = Vec::with_capacity(samples.len());
        for sample in samples {
            let pair = sample
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| format!("{key}: sample"))?;
            let t = pair[0]
                .as_f64()
                .filter(|t| t.is_finite())
                .ok_or_else(|| format!("{key}: time"))?;
            let used = pair[1].as_u64().ok_or_else(|| format!("{key}: used"))?;
            parsed.push((t, used));
        }
        parsed.sort_by(|a, b| a.0.total_cmp(&b.0));
        store.series.insert(
            key.clone(),
            Series {
                since,
                samples: parsed,
            },
        );
    }
    Ok(store)
}

fn render_store(store: &Store) -> Result<String, String> {
    let mut series = serde_json::Map::new();
    for (key, entry) in &store.series {
        let samples = entry
            .samples
            .iter()
            .map(|(t, used)| serde_json::json!([t, used]))
            .collect::<Vec<_>>();
        series.insert(
            key.clone(),
            serde_json::json!({ "since": entry.since, "samples": samples }),
        );
    }
    serde_json::to_string(&serde_json::json!({ "version": 1, "series": series }))
        .map_err(|err| err.to_string())
}

/// Points for one trace over the shown window. A hole wider than the
/// samples' own spacing is `None`, so the line does not invent a slope
/// across a time the widget was not running.
pub fn trace(series: &Series, window_s: f64, now: f64) -> (Vec<Option<f64>>, Vec<f64>) {
    let start = now - window_s;
    let points: Vec<(f64, u64)> = series
        .samples
        .iter()
        .copied()
        .filter(|(t, _)| *t >= start && *t <= now)
        .collect();
    if points.is_empty() || window_s <= 0.0 {
        return (Vec::new(), Vec::new());
    }
    let mut values = Vec::new();
    let mut positions = Vec::new();
    let mut previous: Option<f64> = None;
    for (t, used) in points {
        if let Some(before) = previous {
            let limit = if now - before < DAY {
                2.0 * MINUTE
            } else {
                2.0 * QUARTER
            };
            if t - before > limit {
                let mid = (before + t) / 2.0;
                values.push(None);
                positions.push((mid - start) / window_s);
            }
        }
        values.push(Some(used as f64));
        positions.push((t - start) / window_s);
        previous = Some(t);
    }
    (values, positions)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn linear(start: f64, step: f64, count: usize, per_s: f64, base: u64) -> Series {
        let mut series = Series::default();
        for index in 0..count {
            let t = start + step * index as f64;
            let used = base + (per_s * (t - start)) as u64;
            record(&mut series, t, used);
        }
        series
    }

    #[test]
    fn recording_keeps_one_sample_per_minute() {
        let mut series = Series::default();
        assert!(record(&mut series, 1_000.0, 10));
        assert!(!record(&mut series, 1_030.0, 20));
        assert!(record(&mut series, 1_060.0, 30));
        assert_eq!(series.samples.len(), 2);
        assert_eq!(series.samples[0].1, 20);
        assert_eq!(series.samples[1].1, 30);
    }

    #[test]
    fn compaction_keeps_the_day_and_drops_the_month() {
        let mut series = Series::default();
        let now = 40.0 * DAY;
        for minute in 0..(31 * 24 * 60) {
            let t = now - (31 * 24 * 60 - minute) as f64 * MINUTE;
            record(&mut series, t, minute as u64);
        }
        compact(&mut series, now);
        assert!(series
            .samples
            .iter()
            .all(|(t, _)| now - *t <= RETAIN + MINUTE));
        let recent = series
            .samples
            .iter()
            .filter(|(t, _)| now - *t <= DAY)
            .count();
        let older = series.samples.len() - recent;
        assert!(recent <= 24 * 60 + 1, "{recent}");
        assert!(older <= 29 * 24 * 4 + 4, "{older}");
        assert!(!series.samples.is_empty());
    }

    #[test]
    fn a_linear_fill_projects_the_right_eta() {
        let start = 1_700_000_000.0;
        let series = linear(start, MINUTE, 20, 1_000.0, 0);
        let now = start + 19.0 * MINUTE;
        let slope = slope_bytes_per_s(&series, 7.0 * DAY, now).unwrap();
        assert!((slope.per_s - 1_000.0).abs() < 1.0, "{}", slope.per_s);
        let usable = 1_000 * 3_600;
        match eta(usable, Some(slope)) {
            Eta::In(secs) => assert!((secs - 3_600.0).abs() < 2.0, "{secs}"),
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_gap_longer_than_the_window_resets_since() {
        let mut series = Series::default();
        record(&mut series, 1_000.0, 1);
        record(&mut series, 1_000.0 + 8.0 * DAY, 2);
        let window = 7.0 * DAY;
        restart_after_gap(&mut series, window);
        assert_eq!(series.samples.len(), 1);
        assert_eq!(series.since, 1_000.0 + 8.0 * DAY);
    }

    #[test]
    fn a_short_span_and_a_single_sample_are_not_a_slope() {
        let mut one = Series::default();
        record(&mut one, 1_000.0, 5);
        assert!(slope_bytes_per_s(&one, DAY, 1_000.0).is_none());
        let mut brief = Series::default();
        record(&mut brief, 1_000.0, 5);
        record(&mut brief, 1_000.0 + 10.0 * MINUTE, 50);
        assert!(slope_bytes_per_s(&brief, DAY, 1_000.0 + 10.0 * MINUTE).is_none());
    }

    #[test]
    fn the_arrow_changes_at_exactly_twenty_five_percent() {
        let window = Eta::In(100.0);
        assert_eq!(arrow(window, Eta::In(75.0)), Some('↑'));
        assert_eq!(arrow(window, Eta::In(75.1)), None);
        assert_eq!(arrow(window, Eta::In(125.0)), Some('↓'));
        assert_eq!(arrow(window, Eta::In(124.9)), None);
        assert_eq!(arrow(Eta::NotFilling, Eta::In(10.0)), None);
    }

    #[test]
    fn the_key_changes_when_the_filesystem_grows() {
        assert_ne!(key("/data", "ext4", 100), key("/data", "ext4", 200));
        assert_eq!(key("/data", "ext4", 100), key("/data", "ext4", 100));
    }

    #[test]
    fn two_series_union_by_minute_without_duplicates() {
        let mut left = Series::default();
        record(&mut left, 1_000.0, 1);
        record(&mut left, 1_060.0, 2);
        let mut right = Series::default();
        record(&mut right, 1_030.0, 9);
        record(&mut right, 1_120.0, 4);
        union_series(&mut left, &right);
        assert_eq!(left.samples.len(), 3);
        assert_eq!(left.samples[0].1, 9);
        assert_eq!(left.samples[1].1, 2);
        assert_eq!(left.samples[2].1, 4);
    }

    #[test]
    fn a_file_from_another_pane_is_merged_not_replaced() {
        let dir = std::env::temp_dir().join(format!("opscope-disk-merge-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("usage.json");
        let now = tc::now();
        let mut other = Store::default();
        let mut series = Series::default();
        record(&mut series, now - 120.0, 10);
        other.series.insert("k".into(), series);
        std::fs::write(&path, render_store(&other).unwrap()).unwrap();
        let mut mine = Store::default();
        let mut series = Series::default();
        record(&mut series, now - 60.0, 20);
        mine.series.insert("k".into(), series);
        let merged = save_usage(&path, &mine).unwrap();
        let saved = parse_store(&std::fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(merged.series["k"].samples.len(), 2);
        assert_eq!(saved.series["k"].samples.len(), 2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_projection_past_ten_years_is_not_filling() {
        let slope = Slope {
            per_s: 1.0,
            span_s: MIN_SPAN,
            samples: 2,
        };
        let usable = (TEN_YEARS as u64).saturating_add(10);
        assert_eq!(eta(usable, Some(slope)), Eta::NotFilling);
        assert_eq!(
            eta(
                10,
                Some(Slope {
                    per_s: -5.0,
                    span_s: MIN_SPAN,
                    samples: 2,
                })
            ),
            Eta::NotFilling
        );
    }

    #[test]
    fn a_corrupt_history_file_is_quarantined_and_the_next_write_works() {
        static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _guard = LOCK.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
        let dir = std::env::temp_dir().join(format!("opscope-disk-bad-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        // SAFETY: this test is the only one that reads the state directory,
        // and the mutex above keeps a second call from overlapping it.
        unsafe { std::env::set_var("XDG_STATE_HOME", &dir) };
        let path = usage_path();
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, b"not json").unwrap();
        let loaded = load_usage(&path);
        assert!(loaded.note.starts_with("saved history would not read ("));
        assert!(loaded.note.contains("usage.json.bad"));
        assert!(!path.exists());
        assert!(path.parent().unwrap().join("usage.json.bad").exists());
        let saved = save_usage(&path, &Store::default());
        assert!(saved.is_ok(), "{saved:?}");
        assert!(path.exists());
        unsafe { std::env::remove_var("XDG_STATE_HOME") };
        let _ = std::fs::remove_dir_all(&dir);
    }
}
