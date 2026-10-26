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

//! What the pollers hold, and the pollers.
//!
//! Two threads per property. One reads realtime every 15, 30 or 60
//! seconds (stretched when idle or short of quota) and the realtime
//! breakdowns every minute; the other reads the processed reports every
//! `refresh`, so a slow report never delays a live reading.
//! Each records why it stopped, and every failure lands in a field the
//! screen draws, because a poller that dies quietly leaves a pane that
//! looks exactly like a site nobody is visiting.
#![allow(dead_code)]

use std::collections::HashMap;
use std::io::Write;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use opscope_core as tc;
use opscope_core::google::Credential;

use crate::api::{self, Fail};
use crate::model;
use crate::parse::{self, Quota, Report, Sample};

/// The latest realtime reading.
#[derive(Clone, Debug, Default)]
pub struct Rt {
    pub active: f64,
    pub views: f64,
    pub key: f64,
    /// 30 slots, oldest first; the last is the minute in progress.
    pub minutes: Vec<f64>,
    pub views_minutes: Vec<f64>,
}

/// The realtime breakdowns, read once a minute.
#[derive(Clone, Debug, Default)]
pub struct Breakdown {
    pub pages: Report,
    pub devices: Report,
    pub countries: Report,
    pub events: Report,
}

/// The processed reports: the first batch in the order `processed_body`
/// asks for them, then the second batch from `window_body`.
#[derive(Clone, Debug, Default)]
pub struct Processed {
    pub hourly: Report,
    pub sources: Report,
    pub key_events: Report,
    /// Pages by day, for the charts only - never summed into a window.
    pub pages: Report,
    /// Source/medium over each whole window.
    pub source_movers: Report,
    /// The second batch: pages and channels over each whole window, and
    /// organic landing pages. `None` until it has answered once; when it
    /// fails, the last answer for the same window is kept and `window_at`
    /// says how old it is.
    pub page_totals: Option<Report>,
    pub channels: Option<Report>,
    pub organic: Option<Report>,
    pub window_at: f64,
    /// The window the movers were asked for.
    pub window: usize,
    /// The property's date when the reports were asked for: what their
    /// `today` meant. After midnight it is yesterday until the next
    /// refresh, and the screen says so rather than relabel it.
    pub date: chrono::NaiveDate,
}

/// One property and everything known about it.
#[derive(Clone, Debug, Default)]
pub struct Prop {
    pub id: String,
    pub name: String,
    pub time_zone: String,
    pub currency: String,
    /// Analytics 360, whose documented quota is ten times larger.
    /// `None` until the Admin API has said, or a balance above the
    /// standard limit has shown it can only be 360. A `false` here is a
    /// standard property, not an unread one: the two calculate differently
    /// once a bucket falls under the standard limit.
    pub premium: Option<bool>,
    /// GA's flagged key events, once the Admin API has answered.
    pub key_names: Option<Vec<String>>,
    pub rt: Option<Rt>,
    pub rt_at: f64,
    pub breakdown: Option<Breakdown>,
    pub breakdown_at: f64,
    pub processed: Option<Processed>,
    pub core_at: f64,
    pub quota_rt: Option<Quota>,
    pub quota_core: Option<Quota>,
    pub rt_err: String,
    /// The breakdowns' own failure, cleared only when they next succeed.
    pub bd_err: String,
    pub core_err: String,
    pub admin_err: String,
    /// A 429, kept on screen while the last data stays under it.
    pub rt_banner: String,
    pub core_banner: String,
    /// The realtime interval in force, and why it is not the chosen one.
    pub interval: u64,
    pub interval_why: String,
    pub idle_since: Option<f64>,
    pub samples: Vec<Sample>,
    pub sample_path: String,
    pub sample_err: String,
    /// Why the poller stopped, if it has.
    pub died: String,
}

impl Prop {
    /// The property's timezone: the Admin API's, or the one the processed
    /// reports carry when Admin has not answered. Empty only when neither has.
    pub fn zone_name(&self) -> &str {
        if !self.time_zone.is_empty() {
            return &self.time_zone;
        }
        self.processed.as_ref().map(|p| p.hourly.meta.time_zone.as_str()).filter(|s| !s.is_empty()).unwrap_or("")
    }

    /// Keep a timezone a report carried when Admin has not supplied one.
    /// A later Admin answer replaces it; an empty one does not.
    pub fn note_zone(&mut self, reported: &str) {
        if self.time_zone.is_empty() && !reported.is_empty() {
            self.time_zone = reported.to_string();
        }
    }

    /// A balance above the standard limit is Analytics 360. Remember that
    /// so a later reading under the limit is still measured against the
    /// 360 size. Admin's answer, once it has arrived, stands.
    pub fn learn_tier(&mut self) {
        if self.premium.is_some() {
            return;
        }
        let proved = self.quota_rt.as_ref().is_some_and(Quota::proves_premium) || self.quota_core.as_ref().is_some_and(Quota::proves_premium);
        if proved {
            self.premium = Some(true);
        }
    }
}

/// One on-demand detail: the reports it asked for, or why it has none.
#[derive(Clone, Debug)]
pub struct Detail {
    pub reports: Result<Vec<Report>, String>,
    pub at: f64,
}

/// What the pollers and the screen share.
#[derive(Default)]
pub struct Shared {
    pub props: Vec<Prop>,
    /// Discovery's state: empty once properties are known.
    pub discovery: String,
    pub discovery_err: String,
    /// The realtime interval chosen with `l`.
    pub chosen: u64,
    /// The movers window chosen with `w`.
    pub window: usize,
    /// Bumped by `r`; every poller refetches when it moves.
    pub force: u64,
    pub details: HashMap<String, Detail>,
}

/// What the pollers read from config, once.
#[derive(Clone, Debug)]
pub struct Cfg {
    pub refresh: f64,
    pub baseline_weeks: usize,
    pub key_events: Vec<String>,
}

/// Where a property's samples live: `$XDG_STATE_HOME/opscope/
/// google-analytics/<id>.samples`, the same state root `clocks` and
/// `ports` keep theirs under.
pub fn sample_path(id: &str) -> String {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|b| !b.is_empty())
        .unwrap_or_else(|| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    format!("{}/opscope/google-analytics/{}.samples", base, id)
}

/// Two panes on one property share its sample file, so every change to
/// it - an append, and the read, prune and rewrite - happens under an
/// exclusive lock on a file beside it. Without it a prune would replace
/// the file with this pane's own copy and drop what the other appended.
fn locked<R>(path: &str, f: impl FnOnce() -> std::io::Result<R>) -> std::io::Result<R> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).write(true).open(format!("{}.lock", path))?;
    lock.lock()?;
    let out = f();
    let _ = lock.unlock();
    out
}

/// Read the file, prune it, and write it back if anything went - all
/// under the lock, from what is on disk rather than what this pane holds,
/// so readings another pane appended survive. Returns what is kept.
fn prune_file(path: &str, now: f64, weeks: usize) -> std::io::Result<Vec<Sample>> {
    locked(path, || {
        let text = match std::fs::read_to_string(path) {
            Ok(t) => t,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(e) => return Err(e),
        };
        let mut s = parse::parse_samples(&text);
        let before = s.len();
        model::prune(&mut s, now, weeks);
        if s.len() != before {
            rewrite_samples(path, &s)?;
        }
        Ok(s)
    })
}

fn load_samples(path: &str, weeks: usize) -> (Vec<Sample>, String) {
    match prune_file(path, tc::now(), weeks) {
        Ok(s) => (s, String::new()),
        Err(e) => (Vec::new(), format!("could not read or prune the sample file {}: {}", path, e)),
    }
}

/// Call with the lock held. The temporary name carries the process id so
/// two panes never write the same one.
fn rewrite_samples(path: &str, samples: &[Sample]) -> std::io::Result<()> {
    let tmp = format!("{}.{}.tmp", path, std::process::id());
    let mut text = String::new();
    for s in samples {
        text.push_str(&parse::format_sample(s));
        text.push('\n');
    }
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn append_sample(path: &str, s: &Sample) -> std::io::Result<()> {
    locked(path, || {
        let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
        writeln!(f, "{}", parse::format_sample(s))
    })
}

fn save(shared: &Arc<Mutex<Shared>>, i: usize, path: &str, s: &Sample) {
    if let Err(e) = append_sample(path, s) {
        with(shared, i, |p| p.sample_err = format!("could not write the sample file {}: {}", path, e));
    }
}

fn with<R>(shared: &Arc<Mutex<Shared>>, i: usize, f: impl FnOnce(&mut Prop) -> R) -> Option<R> {
    let mut g = shared.lock().ok()?;
    g.props.get_mut(i).map(f)
}

/// The panic message out of a caught payload.
pub fn panic_reason(payload: &(dyn std::any::Any + Send)) -> String {
    tc::take_panic_reason().unwrap_or_else(|| {
        payload
            .downcast_ref::<String>()
            .cloned()
            .or_else(|| payload.downcast_ref::<&'static str>().map(|m| m.to_string()))
            .unwrap_or_else(|| "non-string panic payload".into())
    })
}

/// Start the pollers for property `i`: one for realtime and the Admin
/// API, one for the processed reports, so a slow batch of reports never
/// holds up a live reading. Each records why it stopped.
pub fn spawn_property(
    shared: Arc<Mutex<Shared>>,
    i: usize,
    cred: Credential,
    grant: String,
    cfg: Cfg,
    activity: tc::Refresh,
) {
    let (s2, c2, g2, cfg2, a2) = (Arc::clone(&shared), cred.clone(), grant.clone(), cfg.clone(), activity.clone());
    std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_live(&shared, i, &cred, &grant, &cfg, &activity)
        }));
        record_death(&shared, i, "realtime", run);
    });
    std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_reports(&s2, i, &c2, &g2, &cfg2, &a2)
        }));
        record_death(&s2, i, "reports", run);
    });
}

fn record_death(shared: &Arc<Mutex<Shared>>, i: usize, which: &str, run: std::thread::Result<()>) {
    let why = match run {
        Ok(()) => "the poller returned".to_string(),
        Err(payload) => panic_reason(payload.as_ref()),
    };
    if let Ok(mut g) = shared.lock() {
        if let Some(p) = g.props.get_mut(i) {
            let line = format!("{} poller stopped: {}", which, why);
            p.died = if p.died.is_empty() { line } else { format!("{} · {}", p.died, line) };
        }
    }
}

/// Realtime totals, the realtime breakdowns and the Admin API.
fn run_live(
    shared: &Arc<Mutex<Shared>>,
    i: usize,
    cred: &Credential,
    grant: &str,
    cfg: &Cfg,
    activity: &tc::Refresh,
) {
    let Some(id) = with(shared, i, |p| p.id.clone()) else { return };
    let path = sample_path(&id);
    let (samples, sample_err) = load_samples(&path, cfg.baseline_weeks);
    with(shared, i, |p| {
        p.samples = samples;
        p.sample_path = path.clone();
        p.sample_err = sample_err;
    });
    let (mut next_admin, mut next_rt, mut next_bd) = (0.0f64, 0.0f64, 0.0f64);
    let mut rt_paused = 0.0f64;
    let mut seen_force = 0u64;
    let mut seen_chosen = match shared.lock() {
        Ok(g) => g.chosen,
        Err(_) => return,
    };
    let mut last_prune = tc::now();
    let configured = !cfg.key_events.is_empty();
    loop {
        let now = tc::now();
        let (chosen, force) = match shared.lock() {
            Ok(g) => (g.chosen, g.force),
            Err(_) => return,
        };
        if force != seen_force {
            seen_force = force;
            (next_rt, next_bd) = (now, now);
            if next_admin > now {
                next_admin = now;
            }
            rt_paused = 0.0;
        }
        // `l` changes `chosen` without touching `force`. Take one realtime
        // reading on the new interval. A 429 pause stays in force, and
        // breakdowns, admin and the processed reports keep their clocks.
        if chosen != seen_chosen {
            seen_chosen = chosen;
            next_rt = pull_realtime(next_rt, rt_paused, now);
        }

        if next_admin <= now {
            let work = activity.start();
            let got = admin(cred, &id, grant);
            with(shared, i, |p| match got {
                Ok((info, names)) => {
                    if !info.name.is_empty() {
                        p.name = info.name;
                    }
                    if !info.time_zone.is_empty() {
                        p.time_zone = info.time_zone;
                    }
                    p.currency = info.currency;
                    p.premium = Some(info.premium);
                    p.key_names = Some(names);
                    p.admin_err.clear();
                    next_admin = f64::INFINITY;
                }
                Err(f) => {
                    p.admin_err = f.text;
                    next_admin = now + 300.0;
                }
            });
            drop(work);
        }

        if next_rt <= now && rt_paused <= now {
            let work = activity.start();
            let got = realtime(cred, &id, grant);
            let at = tc::now();
            let mut sample: Option<Sample> = None;
            with(shared, i, |p| {
                match got {
                    Ok((rt, quota)) => {
                        if rt.active > 0.0 {
                            p.idle_since = None;
                        } else if p.idle_since.is_none() {
                            p.idle_since = Some(at);
                        }
                        let s = Sample { t: at, active: Some(rt.active), views: Some(rt.views), events: None };
                        p.samples.push(s.clone());
                        sample = Some(s);
                        if quota.is_some() {
                            p.quota_rt = quota;
                        }
                        p.rt = Some(rt);
                        p.rt_at = at;
                        p.rt_err.clear();
                        p.rt_banner.clear();
                    }
                    Err(f) if f.quota => {
                        p.rt_banner = f.text;
                        rt_paused = at + 300.0;
                    }
                    Err(f) => p.rt_err = f.text,
                }
                p.learn_tier();
                let idle_for = p.idle_since.map_or(0.0, |s| at - s);
                let left = p.quota_rt.as_ref().and_then(|q| q.tightest(p.premium)).map(|t| t.1);
                let (secs, why) = model::realtime_interval(chosen, idle_for, left);
                p.interval = secs;
                p.interval_why = why;
                next_rt = at + secs as f64;
            });
            if let Some(s) = sample {
                save(shared, i, &path, &s);
            }
            if at - last_prune > 86_400.0 {
                last_prune = at;
                match prune_file(&path, at, cfg.baseline_weeks) {
                    // What is on disk includes any other pane's readings.
                    Ok(kept) => with(shared, i, |p| p.samples = kept),
                    Err(e) => with(shared, i, |p| {
                        model::prune(&mut p.samples, at, cfg.baseline_weeks);
                        p.sample_err = format!("could not prune the sample file {}: {}", path, e);
                    }),
                };
            }
            drop(work);
        }

        if next_bd <= now && rt_paused <= now {
            let work = activity.start();
            let got = breakdown(cred, &id, grant, &cfg.key_events);
            let at = tc::now();
            let mut sample: Option<Sample> = None;
            let interval = with(shared, i, |p| {
                match got {
                    Ok((b, quota)) => {
                        // The event counts are a reading of their own,
                        // stamped now, when they were read.
                        let events = model::covered_events(
                            &b.events,
                            if configured { &cfg.key_events } else { &[] },
                            p.key_names.as_deref(),
                        );
                        let s = Sample { t: at, active: None, views: None, events: Some(events) };
                        p.samples.push(s.clone());
                        sample = Some(s);
                        p.breakdown = Some(b);
                        p.breakdown_at = at;
                        p.bd_err.clear();
                        if quota.is_some() {
                            p.quota_rt = quota;
                        }
                    }
                    Err(f) if f.quota => {
                        p.rt_banner = f.text;
                        rt_paused = at + 300.0;
                    }
                    // Its own field: a totals read that succeeds says
                    // nothing about whether the breakdowns did.
                    Err(f) => p.bd_err = format!("realtime breakdowns: {}", f.text),
                }
                p.learn_tier();
                p.interval
            })
            .unwrap_or(60);
            next_bd = at + (interval.max(60)) as f64;
            if let Some(s) = sample {
                save(shared, i, &path, &s);
            }
            drop(work);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// The next realtime read after `l`. A pause that has not elapsed is left
/// alone; otherwise the read is due now, so the interval just chosen is
/// the one that spaces the reading after it.
fn pull_realtime(next_rt: f64, paused_until: f64, now: f64) -> f64 {
    if paused_until <= now { now } else { next_rt }
}

/// The processed reports, on their own clock.
fn run_reports(
    shared: &Arc<Mutex<Shared>>,
    i: usize,
    cred: &Credential,
    grant: &str,
    cfg: &Cfg,
    activity: &tc::Refresh,
) {
    let Some(id) = with(shared, i, |p| p.id.clone()) else { return };
    let mut next_core = 0.0f64;
    let mut core_paused = 0.0f64;
    let mut seen_force = 0u64;
    let mut seen_window = 0usize;
    loop {
        let now = tc::now();
        let (window, force) = match shared.lock() {
            Ok(g) => (g.window, g.force),
            Err(_) => return,
        };
        if force != seen_force {
            seen_force = force;
            next_core = now;
            core_paused = 0.0;
        }
        if window != seen_window {
            seen_window = window;
            next_core = now;
        }
        if next_core <= now && core_paused <= now {
            let work = activity.start();
            let (tz, previous) = with(shared, i, |p| (p.time_zone.clone(), p.processed.clone())).unwrap_or_default();
            let got = processed(cred, &id, grant, window, &cfg.key_events, &tz, previous.as_ref());
            let at = tc::now();
            with(shared, i, |p| {
                match got {
                    Ok((proc_, quota, second)) => {
                        if quota.is_some() {
                            p.quota_core = quota;
                        }
                        p.note_zone(&proc_.hourly.meta.time_zone);
                        p.processed = Some(proc_);
                        p.core_at = at;
                        p.core_err.clear();
                        p.core_banner.clear();
                        match second {
                            None => {}
                            Some(f) if f.quota => {
                                p.core_banner = f.text;
                                core_paused = at + 300.0;
                            }
                            Some(f) => {
                                p.core_err = format!("page and channel totals, organic landing pages: {}", f.text)
                            }
                        }
                    }
                    Err(f) if f.quota => {
                        p.core_banner = f.text;
                        core_paused = at + 300.0;
                    }
                    Err(f) => p.core_err = f.text,
                }
                p.learn_tier();
                let low = p
                    .quota_core
                    .as_ref()
                    .and_then(|q| q.tightest(p.premium))
                    .is_some_and(|t| t.1 < 0.10);
                next_core = at + cfg.refresh * if low { 2.0 } else { 1.0 };
            });
            drop(work);
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn admin(cred: &Credential, id: &str, grant: &str) -> Result<(parse::PropertyInfo, Vec<String>), Fail> {
    let info = api::property(cred, id, grant)
        .and_then(|t| parse::parse_property(&t).map_err(|e| Fail { text: e, quota: false }))?;
    let mut names = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..20 {
        let text = api::key_event_list(cred, id, page.as_deref(), grant)?;
        let (found, next) = parse::parse_key_event_list(&text).map_err(|e| Fail { text: e, quota: false })?;
        names.extend(found);
        match next {
            Some(n) => page = Some(n),
            None => break,
        }
    }
    Ok((info, names))
}

fn bad(e: String) -> Fail {
    Fail { text: e, quota: false }
}

fn realtime(cred: &Credential, id: &str, grant: &str) -> Result<(Rt, Option<Quota>), Fail> {
    let totals = parse::parse_report(&api::realtime(cred, id, &api::realtime_totals_body(), grant)?).map_err(bad)?;
    let minutes = parse::parse_report(&api::realtime(cred, id, &api::realtime_minutes_body(), grant)?).map_err(bad)?;
    let (active, views, key) = model::realtime_totals(&totals);
    Ok((
        Rt {
            active,
            views,
            key,
            minutes: model::per_minute(&minutes, "activeUsers"),
            views_minutes: model::per_minute(&minutes, "screenPageViews"),
        },
        minutes.quota.or(totals.quota),
    ))
}

fn breakdown(cred: &Credential, id: &str, grant: &str, configured: &[String]) -> Result<(Breakdown, Option<Quota>), Fail> {
    let one = |body: serde_json::Value| -> Result<Report, Fail> {
        parse::parse_report(&api::realtime(cred, id, &body, grant)?).map_err(bad)
    };
    let pages = one(api::realtime_breakdown_body("unifiedScreenName", &["activeUsers"], 10))?;
    let devices = one(api::realtime_breakdown_body("deviceCategory", &["activeUsers"], 10))?;
    let countries = one(api::realtime_breakdown_body("country", &["activeUsers"], 10))?;
    let events = one(api::realtime_events_body(configured))?;
    let quota = events.quota.clone();
    Ok((Breakdown { pages, devices, countries, events }, quota))
}

/// The processed reports: two batches. If the second alone fails, the
/// first still lands, the second's last answer for the same window is
/// kept, and its failure - a 429 included - is returned beside them.
fn processed(
    cred: &Credential,
    id: &str,
    grant: &str,
    window: usize,
    configured: &[String],
    tz_name: &str,
    previous: Option<&Processed>,
) -> Result<(Processed, Option<Quota>, Option<Fail>), Fail> {
    let started = tc::now();
    let mut reports = parse::parse_batch(&api::batch(cred, id, &api::processed_body(window, configured), grant)?)
        .map_err(bad)?;
    if reports.len() < 5 {
        return Err(bad(format!("GA answered {} of the 5 reports asked for", reports.len())));
    }
    let mut quota = reports.iter().rev().find_map(|r| r.quota.clone());
    let source_movers = reports.remove(4);
    let pages = reports.remove(3);
    let key_events = reports.remove(2);
    let sources = reports.remove(1);
    let hourly = reports.remove(0);
    // GA reads `today` in the property's timezone, which every report
    // carries; the Admin API's is the fallback.
    let tz = model::tz_of(if hourly.meta.time_zone.is_empty() { tz_name } else { &hourly.meta.time_zone });
    let (date, _, _) = model::local_now(started, tz);
    let mut out = Processed {
        hourly,
        sources,
        key_events,
        pages,
        source_movers,
        page_totals: None,
        channels: None,
        organic: None,
        window_at: 0.0,
        window,
        date,
    };
    let second = api::batch(cred, id, &api::window_body(window), grant).and_then(|t| {
        let r = parse::parse_batch(&t).map_err(bad)?;
        if r.len() < 3 {
            return Err(bad(format!("GA answered {} of the 3 window reports asked for", r.len())));
        }
        Ok(r)
    });
    let failed = match second {
        Ok(mut r) => {
            if let Some(q) = r.iter().rev().find_map(|x| x.quota.clone()) {
                quota = Some(q);
            }
            out.organic = Some(r.remove(2));
            out.channels = Some(r.remove(1));
            out.page_totals = Some(r.remove(0));
            out.window_at = tc::now();
            None
        }
        Err(f) => {
            if let Some(old) = previous.filter(|o| o.window == window) {
                out.page_totals = old.page_totals.clone();
                out.channels = old.channels.clone();
                out.organic = old.organic.clone();
                out.window_at = old.window_at;
            }
            Some(f)
        }
    };
    Ok((out, quota, failed))
}

/// Every property the account can see, through Admin `accountSummaries`.
pub fn discover(cred: &Credential, grant: &str) -> Result<Vec<parse::PropertySummary>, Fail> {
    let mut out = Vec::new();
    let mut page: Option<String> = None;
    for _ in 0..50 {
        let text = api::account_summaries(cred, page.as_deref(), grant)?;
        let (found, next) = parse::parse_account_summaries(&text).map_err(bad)?;
        out.extend(found);
        match next {
            Some(n) => page = Some(n),
            None => return Ok(out),
        }
    }
    Err(bad(format!("the property list ran past 50 pages; {} read so far - list ids in properties instead", out.len())))
}

/// Fetch a detail in the background into `shared.details[key]`.
pub fn spawn_detail(
    shared: Arc<Mutex<Shared>>,
    key: String,
    cred: Credential,
    grant: String,
    id: String,
    bodies: Vec<serde_json::Value>,
    activity: tc::Refresh,
) {
    std::thread::spawn(move || {
        let work = activity.start();
        // The batches' reports, end to end; any one failing fails the
        // detail, since a detail drawn from part of its reports would
        // read as complete.
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let mut all = Vec::new();
            for body in &bodies {
                let r = api::batch(&cred, &id, body, &grant)
                    .map_err(|f| f.text)
                    .and_then(|t| parse::parse_batch(&t))?;
                all.extend(r);
            }
            Ok(all)
        }));
        let reports = match got {
            Ok(r) => r,
            Err(payload) => Err(format!("the detail request stopped: {}", panic_reason(payload.as_ref()))),
        };
        if let Ok(mut g) = shared.lock() {
            g.details.insert(key, Detail { reports, at: tc::now() });
        }
        drop(work);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parse::Bucket;

    #[test]
    fn a_new_realtime_interval_waits_out_a_quota_pause() {
        assert_eq!(pull_realtime(40.0, 0.0, 10.0), 10.0);
        assert_eq!(pull_realtime(40.0, 10.0, 10.0), 10.0);
        assert_eq!(pull_realtime(40.0, 310.0, 10.0), 40.0);
    }

    #[test]
    fn a_report_zone_fills_an_empty_one_and_does_not_replace_admin() {
        let mut p = Prop::default();
        let mut hourly = Report::default();
        hourly.meta.time_zone = "America/New_York".into();
        p.processed = Some(Processed { hourly, ..Processed::default() });
        assert_eq!(p.zone_name(), "America/New_York");
        p.note_zone("America/New_York");
        p.note_zone("Europe/Berlin");
        assert_eq!(p.time_zone, "America/New_York");
        p.time_zone = "Europe/Berlin".into();
        p.note_zone("America/New_York");
        assert_eq!(p.zone_name(), "Europe/Berlin");
    }

    #[test]
    fn an_inferred_360_tier_sticks_and_admin_is_not_overwritten() {
        let mut p = Prop::default();
        p.quota_rt = Some(Quota { buckets: vec![("tokensPerHour".into(), Bucket { consumed: 1.0, remaining: 30_000.0 })] });
        p.learn_tier();
        assert_eq!(p.premium, None, "under the standard limit the tier is still unknown");
        p.quota_rt.as_mut().unwrap().buckets[0].1.remaining = 50_000.0;
        p.learn_tier();
        assert_eq!(p.premium, Some(true));
        p.quota_rt.as_mut().unwrap().buckets[0].1.remaining = 30_000.0;
        let level = p.quota_rt.as_ref().unwrap().level("tokensPerHour", p.premium).unwrap();
        assert_eq!(level.limit, 400_000.0, "a later smaller balance stays on the 360 size");
        p.premium = Some(false);
        p.quota_rt.as_mut().unwrap().buckets[0].1.remaining = 50_000.0;
        p.learn_tier();
        assert_eq!(p.premium, Some(false), "admin's answer stands once it has arrived");
    }
}
