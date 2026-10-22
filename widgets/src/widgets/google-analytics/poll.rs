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
//! One thread per property, each on three clocks: realtime every 15, 30
//! or 60 seconds (stretched when idle or short of quota), the realtime
//! breakdowns every minute, and the processed reports every `refresh`.
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

/// The processed reports, in the order `processed_body` asks for them.
#[derive(Clone, Debug, Default)]
pub struct Processed {
    pub hourly: Report,
    pub sources: Report,
    pub key_events: Report,
    pub pages: Report,
    pub channels: Report,
    pub organic: Option<Report>,
    /// The window the movers were asked for.
    pub window: usize,
}

/// One property and everything known about it.
#[derive(Clone, Debug, Default)]
pub struct Prop {
    pub id: String,
    pub name: String,
    pub time_zone: String,
    pub currency: String,
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

fn load_samples(path: &str, weeks: usize) -> (Vec<Sample>, String) {
    match std::fs::read_to_string(path) {
        Ok(text) => {
            let mut s = parse::parse_samples(&text);
            let before = s.len();
            model::prune(&mut s, tc::now(), weeks);
            let mut err = String::new();
            if s.len() != before {
                if let Err(e) = rewrite_samples(path, &s) {
                    err = format!("could not prune the sample file {}: {}", path, e);
                }
            }
            (s, err)
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => (Vec::new(), String::new()),
        Err(e) => (Vec::new(), format!("could not read the sample file {}: {}", path, e)),
    }
}

fn rewrite_samples(path: &str, samples: &[Sample]) -> std::io::Result<()> {
    let tmp = format!("{}.tmp", path);
    let mut text = String::new();
    for s in samples {
        text.push_str(&parse::format_sample(s));
        text.push('\n');
    }
    std::fs::write(&tmp, text)?;
    std::fs::rename(&tmp, path)
}

fn append_sample(path: &str, s: &Sample) -> std::io::Result<()> {
    if let Some(dir) = std::path::Path::new(path).parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut f = std::fs::OpenOptions::new().create(true).append(true).open(path)?;
    writeln!(f, "{}", parse::format_sample(s))
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

/// Start the poller for property `i`, recording why it stopped.
pub fn spawn_property(
    shared: Arc<Mutex<Shared>>,
    i: usize,
    cred: Credential,
    grant: String,
    cfg: Cfg,
    activity: tc::Refresh,
) {
    std::thread::spawn(move || {
        let run = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_property(&shared, i, &cred, &grant, &cfg, &activity)
        }));
        let why = match run {
            Ok(()) => "the poller returned".to_string(),
            Err(payload) => panic_reason(payload.as_ref()),
        };
        if let Ok(mut g) = shared.lock() {
            if let Some(p) = g.props.get_mut(i) {
                p.died = format!("poller stopped: {}", why);
            }
        }
    });
}

fn run_property(
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
    let (mut next_admin, mut next_rt, mut next_bd, mut next_core) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    let (mut rt_paused, mut core_paused) = (0.0f64, 0.0f64);
    let mut seen_force = 0u64;
    let mut seen_window = 0usize;
    let mut last_prune = tc::now();
    loop {
        let now = tc::now();
        let (chosen, window, force) = match shared.lock() {
            Ok(g) => (g.chosen, g.window, g.force),
            Err(_) => return,
        };
        if force != seen_force {
            seen_force = force;
            (next_rt, next_bd, next_core) = (now, now, now);
            if next_admin > now {
                next_admin = now;
            }
            (rt_paused, core_paused) = (0.0, 0.0);
        }
        if window != seen_window {
            seen_window = window;
            next_core = now;
        }
        let configured = !cfg.key_events.is_empty();

        if next_admin <= now {
            let work = activity.start();
            let got = admin(cred, &id, grant);
            with(shared, i, |p| match got {
                Ok((info, names)) => {
                    if !info.name.is_empty() {
                        p.name = info.name;
                    }
                    p.time_zone = info.time_zone;
                    p.currency = info.currency;
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
                        let events = p
                            .breakdown
                            .as_ref()
                            .map(|b| {
                                model::realtime_key_events(&b.events, if configured { &cfg.key_events } else { &[] })
                            })
                            .unwrap_or_default();
                        let s = Sample { t: at, active: rt.active, views: rt.views, events };
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
                let idle_for = p.idle_since.map_or(0.0, |s| at - s);
                let left = p.quota_rt.as_ref().and_then(|q| q.tightest()).map(|t| t.1);
                let (secs, why) = model::realtime_interval(chosen, idle_for, left);
                p.interval = secs;
                p.interval_why = why;
                next_rt = at + secs as f64;
            });
            if let Some(s) = sample {
                if let Err(e) = append_sample(&path, &s) {
                    with(shared, i, |p| p.sample_err = format!("could not write the sample file {}: {}", path, e));
                }
            }
            if at - last_prune > 86_400.0 {
                last_prune = at;
                let kept = with(shared, i, |p| {
                    model::prune(&mut p.samples, at, cfg.baseline_weeks);
                    p.samples.clone()
                });
                if let Some(kept) = kept {
                    if let Err(e) = rewrite_samples(&path, &kept) {
                        with(shared, i, |p| p.sample_err = format!("could not prune the sample file {}: {}", path, e));
                    }
                }
            }
            drop(work);
        }

        if next_bd <= now && rt_paused <= now {
            let work = activity.start();
            let got = breakdown(cred, &id, grant);
            let at = tc::now();
            let interval = with(shared, i, |p| {
                match got {
                    Ok((b, quota)) => {
                        p.breakdown = Some(b);
                        p.breakdown_at = at;
                        if quota.is_some() {
                            p.quota_rt = quota;
                        }
                    }
                    Err(f) if f.quota => {
                        p.rt_banner = f.text;
                        rt_paused = at + 300.0;
                    }
                    Err(f) => p.rt_err = f.text,
                }
                p.interval
            })
            .unwrap_or(60);
            next_bd = at + (interval.max(60)) as f64;
            drop(work);
        }

        if next_core <= now && core_paused <= now {
            let work = activity.start();
            let got = processed(cred, &id, grant, window, &cfg.key_events);
            let at = tc::now();
            with(shared, i, |p| {
                match got {
                    Ok((proc_, quota, organic_err)) => {
                        if quota.is_some() {
                            p.quota_core = quota;
                        }
                        p.processed = Some(proc_);
                        p.core_at = at;
                        p.core_err = organic_err;
                        p.core_banner.clear();
                    }
                    Err(f) if f.quota => {
                        p.core_banner = f.text;
                        core_paused = at + 300.0;
                    }
                    Err(f) => p.core_err = f.text,
                }
                let low = p
                    .quota_core
                    .as_ref()
                    .and_then(|q| q.tightest())
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

fn breakdown(cred: &Credential, id: &str, grant: &str) -> Result<(Breakdown, Option<Quota>), Fail> {
    let one = |dim: &str, metrics: &[&str], limit: u32| -> Result<Report, Fail> {
        parse::parse_report(&api::realtime(cred, id, &api::realtime_breakdown_body(dim, metrics, limit), grant)?)
            .map_err(bad)
    };
    let pages = one("unifiedScreenName", &["activeUsers"], 10)?;
    let devices = one("deviceCategory", &["activeUsers"], 10)?;
    let countries = one("country", &["activeUsers"], 10)?;
    let events = one("eventName", &["eventCount", "keyEvents"], 250)?;
    let quota = events.quota.clone();
    Ok((Breakdown { pages, devices, countries, events }, quota))
}

/// The processed reports. The organic report is a separate request; if it
/// alone fails, the rest still land and its failure is the returned line.
fn processed(
    cred: &Credential,
    id: &str,
    grant: &str,
    window: usize,
    configured: &[String],
) -> Result<(Processed, Option<Quota>, String), Fail> {
    let mut reports = parse::parse_batch(&api::batch(cred, id, &api::processed_body(window, configured), grant)?)
        .map_err(bad)?;
    if reports.len() < 5 {
        return Err(bad(format!("GA answered {} of the 5 reports asked for", reports.len())));
    }
    let quota = reports.iter().rev().find_map(|r| r.quota.clone());
    let (organic, organic_err) = match api::report(cred, id, &api::organic_body(window), grant)
        .and_then(|t| parse::parse_report(&t).map_err(bad))
    {
        Ok(r) => (Some(r), String::new()),
        Err(f) => (None, format!("organic landing pages: {}", f.text)),
    };
    let channels = reports.remove(4);
    let pages = reports.remove(3);
    let key_events = reports.remove(2);
    let sources = reports.remove(1);
    let hourly = reports.remove(0);
    Ok((Processed { hourly, sources, key_events, pages, channels, organic, window }, quota, organic_err))
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
    body: serde_json::Value,
    activity: tc::Refresh,
) {
    std::thread::spawn(move || {
        let work = activity.start();
        let got = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            api::batch(&cred, &id, &body, &grant)
                .map_err(|f| f.text)
                .and_then(|t| parse::parse_batch(&t))
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
