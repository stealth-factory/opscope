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

//! Google Search Console: which pages and queries moved, what people search
//! to arrive, and whether the pages that earn the traffic are indexed.
//!
//! Signs in through `opscope_core::google` with the read-only
//! `webmasters.readonly` scope. Every figure is Google's, labelled with how
//! far behind Google keeps it: hourly data is a few hours behind and still
//! being collected, final data two or three days.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use chrono::Utc;
use opscope_core as tc;
use opscope_core::google;

mod api;
mod parse;
mod view;

use api::*;
use parse::*;
use view::*;

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "google-search-console",
    section: "google_search_console",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// The shortest `refresh`. The hourly data moves a few times an hour at
/// best, and Google counts repeated asks for the same data against the
/// site's load quota, so anything shorter only spends it.
const MIN_REFRESH: f64 = 300.0;
/// How often a site not on screen has its daily totals read, for the SITES
/// table. Nothing else of it is read until it is shown.
const OFF_SCREEN_EVERY: f64 = 3600.0;
/// The expensive reports - pages, queries and terms, grouped by page or by
/// query over two windows - are read when their windows move, which is
/// about once a day, and at most this long apart otherwise, so a late
/// correction in Google's numbers still arrives.
const HEAVY_EVERY: f64 = 6.0 * 3600.0;
/// Sitemaps change when a site publishes one, which is rare.
const SITEMAPS_EVERY: f64 = 6.0 * 3600.0;
/// How often the poller looks at what is due. Looking costs nothing; only a
/// part that is due is asked for.
const TICK: f64 = 60.0;
/// The search types `t` steps through.
const KINDS: &[&str] = &["web", "image", "video", "news", "discover"];
/// The windows `w` steps between.
const WINDOWS: &[i64] = &[7, 28];

/// Everything read from the widget's own config section.
#[derive(Clone)]
struct Config {
    credentials: String,
    credentials_file: String,
    sites: Vec<String>,
    refresh: f64,
    inspect_top: usize,
}

/// What the poller hands the screen.
#[derive(Default)]
struct Shared {
    sites: Vec<SiteData>,
    /// What stops every site at once: no credential, a key file that cannot
    /// be read, a refused sign-in, sites.list refused, the poller died.
    err: String,
    /// The site on screen, set by the screen every frame. Only it is read in
    /// full.
    focus: String,
    /// `r` was pressed: read everything of the site on screen now, whatever
    /// is due. Taken by the poller.
    force: bool,
}

/// `r`: read the site on screen in full on the next tick, cadence aside.
fn force_read(shared: &Arc<Mutex<Shared>>) {
    if let Ok(mut g) = shared.lock() {
        g.force = true;
    }
}

/// Where inspections are kept between runs.
fn state_dir() -> String {
    let base = std::env::var("XDG_STATE_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("{}/.local/state", std::env::var("HOME").unwrap_or_default()));
    format!("{}/opscope/google-search-console", base)
}

fn load_cache(site: &str) -> InspectionCache {
    let path = format!("{}/{}", state_dir(), cache_name(site));
    std::fs::read_to_string(path).map(|t| parse_inspection_cache(&t)).unwrap_or_default()
}

/// Written whole to a temporary file and renamed over the old one, so a
/// crash mid-write leaves the previous cache rather than half of one.
fn save_cache(site: &str, cache: &InspectionCache) -> Result<(), String> {
    let dir = state_dir();
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create {}: {}", dir, e))?;
    let path = format!("{}/{}", dir, cache_name(site));
    let tmp = format!("{}.tmp", path);
    std::fs::write(&tmp, cache.to_json()).map_err(|e| format!("cannot write {}: {}", tmp, e))?;
    std::fs::rename(&tmp, &path).map_err(|e| format!("cannot replace {}: {}", path, e))
}

/// Google's date: its days, quotas and date ranges are all Pacific.
fn google_today() -> chrono::NaiveDate {
    Utc::now().with_timezone(&chrono_tz::America::Los_Angeles).date_naive()
}

type Caches = Arc<Mutex<BTreeMap<String, InspectionCache>>>;

/// Sign in, or say why not, in the wording every Google widget shares.
fn signed_in(cfg: &Config) -> Result<(google::Credential, Client), String> {
    let cred = match google::credential(&cfg.credentials, &cfg.credentials_file) {
        Ok(Some(cred)) => cred,
        Ok(None) => return Err(google::missing_credential(SETTINGS.section)),
        Err(why) => return Err(why),
    };
    // Read-only: it cannot submit a sitemap or change a property.
    let token = cred.bearer(&[google::SCOPE_WEBMASTERS_READONLY])?;
    let client = Client { bearer: token.value, email: cred.client_email() };
    Ok((cred, client))
}

/// Which properties to read, and the problem with any that cannot be.
/// `last` is the list the previous round settled on.
#[allow(clippy::type_complexity)]
fn resolve_sites(
    cfg: &Config,
    client: &Client,
    last: &[(String, String, Option<String>)],
) -> (Vec<(String, String, Option<String>)>, Option<String>) {
    plan_sites(&cfg.sites, api::sites(client), client.email.as_deref(), last)
}

/// `resolve_sites` apart from the network: the configured names against
/// what `sites.list` answered, as (site, permission, problem) and an error
/// that applies to all of them.
///
/// With no sites configured, the list is the only way to know what to read.
/// When asking for it fails, the properties `last` found are read on behind
/// the error: an empty answer there would drop every reading on screen and
/// throw away what each would otherwise not need to ask again. A refused
/// token (401) still ends it, as it does for configured sites.
#[allow(clippy::type_complexity)]
fn plan_sites(
    configured: &[String],
    listed: Result<Vec<Site>, Fail>,
    email: Option<&str>,
    last: &[(String, String, Option<String>)],
) -> (Vec<(String, String, Option<String>)>, Option<String>) {
    let configured: Vec<String> = configured.iter().map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect();
    match (configured.is_empty(), listed) {
        (true, Ok(mut list)) => {
            if list.is_empty() {
                return (
                    Vec::new(),
                    Some(format!(
                        "this account can see no Search Console property: {}",
                        grant_for(email, "each property")
                    )),
                );
            }
            list.sort_by(|a, b| a.url.cmp(&b.url));
            (list.into_iter().map(|s| (s.url, s.permission, None)).collect(), None)
        }
        (true, Err(f)) if f.status == 401 || last.is_empty() => (Vec::new(), Some(format!("listing the properties failed: {}", f.said))),
        (true, Err(f)) => (
            last.iter().map(|(site, permission, _)| (site.clone(), permission.clone(), None)).collect(),
            Some(format!("listing the properties failed, so the last list is read on: {}", f.said)),
        ),
        // A refused token is refused everywhere: asking each property as
        // well would only repeat the same sentence once per site.
        (false, Err(f)) if f.status == 401 => (Vec::new(), Some(f.said)),
        (false, listed) => {
            let (list, err) = match listed {
                Ok(list) => (Some(list), None),
                Err(f) => (None, Some(format!("listing the properties failed, so each is asked directly: {}", f.said))),
            };
            let out = configured
                .iter()
                .map(|site| match list.as_ref() {
                    // Without the list, a name in neither form can still be
                    // caught here rather than sent to Google to be refused.
                    None => (site.clone(), String::new(), site_form_problem(site)),
                    Some(list) => match list.iter().find(|s| s.url == *site) {
                        Some(s) => (site.clone(), s.permission.clone(), None),
                        None => {
                            let form = site_form_problem(site).map(|p| format!(" ({})", p)).unwrap_or_default();
                            let near = list
                                .iter()
                                .find(|s| site_host(&s.url) == site_host(site))
                                .map(|s| format!("; the account does see {}", s.url))
                                .unwrap_or_default();
                            (
                                site.clone(),
                                String::new(),
                                Some(format!(
                                    "{} is not among the {} properties this account can see{}{}: {}",
                                    site,
                                    list.len(),
                                    form,
                                    near,
                                    grant_for(email, site)
                                )),
                            )
                        }
                    },
                })
                .collect();
            (out, err)
        }
    }
}

/// A bearer for the next request. Cheap while the token is cached: a round
/// can start with about five minutes left on it, and the inspections after
/// the queries can run longer than that. Re-reading mints a new one only
/// once that window has closed.
fn refresh_bearer(cred: &google::Credential, client: &mut Client) -> Result<(), String> {
    client.bearer = cred.bearer(&[google::SCOPE_WEBMASTERS_READONLY])?.value;
    Ok(())
}

/// Inspect whatever of the site's set is due, within today's budget.
fn inspect_due(cred: &google::Credential, client: &mut Client, data: &mut SiteData, caches: &Caches, top: usize) {
    let Some(movers) = data.pages.value.as_ref() else { return };
    let (set, _, _) = inspection_set(&movers.rows, top);
    let today = google_today().format("%Y-%m-%d").to_string();
    let due = match caches.lock() {
        Ok(mut g) => {
            let c = g.entry(data.site.clone()).or_insert_with(|| load_cache(&data.site));
            c.roll(&today, tc::now());
            c.due(&set, tc::now())
        }
        Err(_) => return,
    };
    for url in due {
        // Before the request, not only at the start of the property: one
        // page can take the whole timeout, and a token that was fresh for
        // the first page need not still be for the last.
        if let Err(why) = refresh_bearer(cred, client) {
            data.inspect_error = Some(format!("inspection stopped for this round: {}", why));
            break;
        }
        // Reserved under the lock before every request, not only checked
        // when the list was made: `i` spends from the same budget while
        // this runs, and a page `i` is already asking about is skipped.
        let reserved = match caches.lock() {
            Ok(mut g) => g.entry(data.site.clone()).or_default().reserve(&url),
            Err(_) => return,
        };
        match reserved {
            Ok(()) => {}
            Err(_) if caches.lock().map(|g| g.get(&data.site).map_or(0, |c| c.left())).unwrap_or(0) > 0 => continue,
            Err(_) => break,
        }
        let answer = api::inspect(client, &data.site, &url);
        let Ok(mut g) = caches.lock() else { return };
        let c = g.entry(data.site.clone()).or_default();
        let stop = settle(c, &url, answer, tc::now());
        // Saved before stopping as well: a refused request still counted
        // against the day, and a restart must not read a lower total.
        if let Err(why) = save_cache(&data.site, c) {
            data.inspect_error = Some(format!("inspections are not being kept: {}", why));
        }
        if let Some(why) = stop {
            data.inspect_error = Some(format!("inspection stopped for this round: {}", why));
            break;
        }
    }
}

/// What one inspection answer did to the cache.
#[derive(Debug, PartialEq)]
enum InspectionOutcome {
    /// Google inspected the page. The verdict is the word to show.
    Inspected(String),
    /// A refusal about this page. Remembered, and a round continues.
    Page(String),
    /// A refusal about the quota or the token, or Google never reached.
    /// Not remembered as this page's result: it was not about the page,
    /// and remembering it would hide the page for a day. A round stops.
    Round(String),
}

/// Fold one inspection answer into the cache. The poller's round and a
/// press of `i` both come through here, so a 429 or an expired token is
/// classified once.
fn keep_inspection(c: &mut InspectionCache, url: &str, answer: Result<(String, Inspection), Fail>, now: f64) -> InspectionOutcome {
    match answer {
        Ok((raw, i)) => {
            let verdict = i.verdict.clone();
            c.record(url, now, raw, Ok(i));
            InspectionOutcome::Inspected(verdict)
        }
        // Google never reached is a request never counted.
        Err(f) if f.status == 0 => {
            c.release(url);
            InspectionOutcome::Round(f.said)
        }
        // The quota, the token or the permission: every page would be
        // refused the same way, so this is not the page's result.
        Err(f) if matches!(f.status, 401 | 403 | 429) => {
            c.abandon(url);
            InspectionOutcome::Round(f.said)
        }
        Err(f) => {
            let said = f.said.clone();
            c.record(url, now, String::new(), Err(f.said));
            InspectionOutcome::Page(said)
        }
    }
}

/// Fold one answer from the poller's inspection round into the cache, and
/// say why the round has to stop, if it does.
fn settle(c: &mut InspectionCache, url: &str, answer: Result<(String, Inspection), Fail>, now: f64) -> Option<String> {
    match keep_inspection(c, url, answer, now) {
        InspectionOutcome::Round(why) => Some(why),
        InspectionOutcome::Inspected(_) | InspectionOutcome::Page(_) => None,
    }
}

/// Put one site's reading on screen, folded over what was there.
fn publish(shared: &Arc<Mutex<Shared>>, params: &Params, data: &SiteData) {
    if let Ok(mut g) = shared.lock() {
        if let Some(slot) = g.sites.iter_mut().find(|o| o.site == data.site) {
            let old = std::mem::replace(slot, SiteData::new(&data.site, params));
            *slot = data.clone().after(old);
        }
    }
}

/// What one site needs asking this time, from the reading it already has.
///
/// The site on screen reads its last 24 hours and daily totals every
/// `refresh`, its sitemaps every six hours, and its pages, queries and terms
/// only when Google finalises another day - the windows they compare move -
/// or six hours have passed. A site off screen reads only its daily totals,
/// hourly, for the SITES table, and its sitemaps. Search Console counts
/// asking for the same data again against the site's load quota, and these
/// reports only change when Google finalises a day.
///
/// New settings, `r`, and a part never read are due at once. A failed part
/// is retried on its own cadence from the last attempt, not every tick.
/// While Google's 429 wait is running nothing is due, `r` included.
fn plan_for(prev: &SiteData, params: &Params, focus: bool, forced: bool, now: f64, refresh: f64) -> Plan {
    let nothing = Plan { hours: false, days: false, heavy: Heavy::No, sitemaps: false };
    if prev.load_wait_until.is_some_and(|until| now < until) {
        return nothing;
    }
    // Google refused the property (401 or 403): every request would be
    // refused the same way until the grant changes, so wait `refresh` from
    // the refusal rather than sending it again every tick. New settings and
    // `r` still ask, in case the grant has just been fixed.
    if prev.problem.is_some() && prev.params == *params && !forced && now - prev.fetched < refresh {
        return nothing;
    }
    let renewed = prev.params != *params || forced;
    // A part never asked for is due now - a site read while off screen has
    // its daily totals and no hours yet. Otherwise its own time when it
    // holds a value, or when the site was last asked anything, which is
    // when it last failed or was noted.
    fn due<T>(part: &Part<T>, fetched: f64, renewed: bool, now: f64, every: f64) -> bool {
        let never = part.value.is_none() && part.error.is_none() && part.note.is_none();
        // Timed from this part's own last attempt. A failed part's kept
        // value has the last success's time, already overdue, which would
        // make it due every tick; and the site's `fetched` moves whenever
        // any other part is read, which would put its retry off for ever.
        let last = if part.tried > 0.0 {
            part.tried
        } else if part.error.is_none() && part.at > 0.0 {
            part.at
        } else {
            fetched
        };
        renewed || never || last <= 0.0 || now - last >= every
    }
    let (f, r) = (prev.fetched, renewed);
    if !focus {
        return Plan {
            hours: false,
            days: due(&prev.days, f, r, now, OFF_SCREEN_EVERY),
            heavy: Heavy::No,
            sitemaps: due(&prev.sitemaps, f, r, now, SITEMAPS_EVERY),
        };
    }
    let pages = &prev.pages;
    // Pages, queries and terms are read in that order and a 429 stops the
    // round, so any of them can be the one that failed or was never asked.
    // Pages alone would leave a failed queries read waiting for the window
    // to move, up to six hours, behind a 15-minute retry time.
    fn unread<T>(part: &Part<T>) -> bool {
        part.value.is_none() && part.error.is_none() && part.note.is_none()
    }
    let never = unread(&prev.pages) || unread(&prev.queries) || unread(&prev.terms);
    let failed = prev.pages.error.is_some() || prev.queries.error.is_some() || prev.terms.error.is_some();
    let days = due(&prev.days, f, r, now, refresh);
    let heavy = if renewed || never {
        Heavy::Yes
    } else if failed {
        // From the heavy reports' own attempt, not `fetched`, which the
        // hourly and daily reads move every `refresh`.
        let tried = prev.pages.tried.max(prev.queries.tried).max(prev.terms.tried);
        let last = if tried > 0.0 { tried } else { prev.fetched };
        if now - last >= refresh { Heavy::Yes } else { Heavy::No }
    } else if pages.at > 0.0 && now - pages.at >= HEAVY_EVERY {
        Heavy::Yes
    } else if days {
        // Only alongside a daily read: that is what can show the windows
        // moved. Without one there is nothing to find out, and a plan that
        // is not empty would sign in for nothing.
        Heavy::IfMoved
    } else {
        Heavy::No
    };
    Plan {
        hours: due(&prev.hours, f, r, now, refresh),
        days,
        heavy,
        sitemaps: due(&prev.sitemaps, f, r, now, SITEMAPS_EVERY),
    }
}

/// The site list and when it was read, kept between ticks: it is asked for
/// every `refresh`, not every minute.
#[derive(Default)]
struct Round {
    targets: Vec<(String, String, Option<String>)>,
    err: Option<String>,
    at: f64,
}

/// One tick: every part of every property that is due, and nothing else.
fn poll_round(cfg: &Config, params: &Params, shared: &Arc<Mutex<Shared>>, caches: &Caches, round: &mut Round, activity: &tc::Refresh) {
    let now = tc::now();
    let (focus, forced) = match shared.lock() {
        Ok(mut g) => (g.focus.clone(), std::mem::take(&mut g.force)),
        Err(_) => return,
    };
    let previous = |site: &str| -> SiteData {
        shared
            .lock()
            .ok()
            .and_then(|g| g.sites.iter().find(|o| o.site == site).cloned())
            .unwrap_or_else(|| SiteData::new(site, params))
    };
    let list_due = forced || round.at <= 0.0 || now - round.at >= cfg.refresh;
    let plans: Vec<Plan> = round
        .targets
        .iter()
        .map(|(site, _, _)| plan_for(&previous(site), params, *site == focus, forced, now, cfg.refresh))
        .collect();
    if !list_due && plans.iter().all(Plan::nothing) {
        return;
    }
    let _work = activity.start();
    let (cred, mut client) = match signed_in(cfg) {
        Ok(ok) => ok,
        Err(why) => {
            if let Ok(mut g) = shared.lock() {
                g.err = why;
            }
            return;
        }
    };
    // A key file others can read explains no number on screen, so it is a
    // notice, not a row. A key pasted into config is covered by core's own
    // config-file notice, which `load_config` raises.
    let warning = cred.warning();
    tc::notices::when(warning.is_some(), || {
        tc::notices::Notice::warn("google-search-console-key-file-readable", warning.clone().unwrap_or_default())
    });
    if list_due {
        let (targets, err) = resolve_sites(cfg, &client, &round.targets);
        *round = Round { targets, err, at: now };
    }
    let targets = round.targets.clone();
    let err = round.err.clone();
    if let Ok(mut g) = shared.lock() {
        g.err = err.unwrap_or_default();
        // Keep what is already on screen for sites still wanted; add the
        // rest as waiting so the site count is right from the start.
        let old = std::mem::take(&mut g.sites);
        g.sites = targets
            .iter()
            .map(|(site, permission, problem)| {
                let mut d = old.iter().find(|o| o.site == *site).cloned().unwrap_or_else(|| SiteData::new(site, params));
                d.permission = permission.clone();
                // The list's own problem when it names one; otherwise keep
                // the refusal the last read met, so it neither flickers off
                // nor is forgotten by plan_for. A read that succeeds clears it.
                d.problem = problem.clone().or(d.problem.take());
                d
            })
            .collect();
    }
    // Every listed property's inspection results, from disk, so SITES can
    // say what they found before the property is shown or inspected again.
    if let Ok(mut c) = caches.lock() {
        for (site, _, _) in &targets {
            c.entry(site.clone()).or_insert_with(|| load_cache(site));
        }
    }
    for (site, permission, problem) in targets {
        if problem.is_some() {
            continue;
        }
        // Again before each property, not only at the start of the round.
        if let Err(why) = refresh_bearer(&cred, &mut client) {
            if let Ok(mut g) = shared.lock() {
                g.err = why;
            }
            return;
        }
        let prev = previous(&site);
        let plan = plan_for(&prev, params, site == focus, forced, now, cfg.refresh);
        if plan.nothing() && permission != "siteUnverifiedUser" {
            continue;
        }
        let mut data = if permission == "siteUnverifiedUser" {
            let mut d = SiteData::new(&site, params);
            d.problem = Some(format!(
                "siteUnverifiedUser: Search Console gives an unverified user no data for {}; verify the property, or have an owner {}",
                site,
                grant_for(client.email.as_deref(), &site)
            ));
            d
        } else {
            fetch_site(&client, &site, params, google_today(), Utc::now(), &prev, plan)
        };
        data.permission = permission;
        // The reading goes on screen first: inspections are one request a
        // page, and a cold cache must not hold the numbers back behind
        // every one of them. Then again with what inspecting had to say.
        publish(shared, params, &data);
        // Inspections for the site on screen: they have a budget of their
        // own, 2,000 a day per site, and a page is asked at most daily.
        if data.problem.is_none() && data.load_wait_until.is_none() && site == focus && cfg.inspect_top > 0 {
            // `fetch_site` carries the last reading forward, its inspection
            // error with it. That error was the last batch's: a refusal or
            // a failed save must not stay on screen once inspecting and
            // keeping work again. Whatever this batch meets, it sets.
            data.inspect_error = None;
            inspect_due(&cred, &mut client, &mut data, caches, cfg.inspect_top);
            publish(shared, params, &data);
        }
    }
}

/// What a detail fetches for an item, as (kind, value): a page or query's
/// 28 days, or a sitemap index's children. `None` for a property or a plain
/// sitemap, whose detail is drawn from what the main round already read.
fn detail_job(site: &SiteData, item: &Item) -> Option<(String, String)> {
    match item {
        Item::Page(u) | Item::Inspected(u) => Some(("page".into(), u.clone())),
        Item::Query(q) => Some(("query".into(), q.clone())),
        Item::Term(n) => site.terms.value.as_ref().and_then(|t| t.get(*n)).map(|t| ("query".to_string(), t.query.clone())),
        Item::Sitemap(n) => site
            .sitemaps
            .value
            .as_ref()
            .and_then(|m| m.get(*n))
            .filter(|m| m.index)
            .map(|m| ("sitemap".to_string(), m.path.clone())),
        Item::Site(_) => None,
    }
}

/// The key a fetched detail is filed under: what it is about by value, not
/// by its row's place - a refresh can put another query at the same row -
/// and the parameters and window it was read for, so a page asked about
/// for another window, or before the day moved on, is not answered from
/// the wrong one.
///
/// The parameters are the reading's own, not the ones just asked for: in
/// the moment after `w`, `d` or `t`, the rows and window on screen are
/// still the old reading's, and a detail mixing them with new parameters
/// would be read for one thing and labelled as another. It follows the
/// new ones when the reading does.
fn detail_key(site: &SiteData, item: &Item) -> Option<String> {
    let (kind, value) = detail_job(site, item)?;
    let p = &site.params;
    Some(format!("{}|{}|{}|{:?}|{}|{}|{}", site.site, kind, value, site.windows, p.fresh, p.days, p.kind))
}

/// What `c` copies for an item.
fn copy_text(item: &Item, sites: &[SiteData], at: usize) -> Option<String> {
    let site = sites.get(at)?;
    Some(match item {
        Item::Page(u) | Item::Inspected(u) => u.clone(),
        Item::Query(q) => q.clone(),
        Item::Term(n) => site.terms.value.as_ref()?.get(*n)?.query.clone(),
        Item::Sitemap(n) => site.sitemaps.value.as_ref()?.get(*n)?.path.clone(),
        // The property the row names, which need not be the one on screen.
        Item::Site(n) => sites.get(*n)?.site.clone(),
    })
}

/// The page an item is about, for `i`.
fn page_of(item: &Item, site: &SiteData) -> Option<String> {
    match item {
        Item::Page(u) | Item::Inspected(u) => Some(u.clone()),
        Item::Term(n) => site.terms.value.as_ref()?.get(*n).map(|t| t.page.clone()),
        _ => None,
    }
}

fn read_config() -> Config {
    let cfg = tc::load_config("google_search_console");
    Config {
        credentials: tc::cfg_str(&cfg, "credentials", ""),
        credentials_file: tc::cfg_str(&cfg, "credentials_file", ""),
        sites: tc::cfg_strings(&cfg, "sites", &[]),
        refresh: tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 600.0), 600.0).max(MIN_REFRESH),
        inspect_top: tc::cfg_usize(&cfg, "inspect_top", 25).min(200),
    }
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available("google-search-console", include_str!("dependencies.json"), Some(SETTINGS)) {
        return;
    }
    let raw = tc::load_config("google_search_console");
    let cfg = read_config();
    let kind = tc::cfg_str(&raw, "search_type", "web");
    let mut params = Params {
        fresh: false,
        days: (tc::cfg_usize(&raw, "window_days", 7).clamp(1, 180)) as i64,
        kind: if KINDS.contains(&kind.as_str()) { kind } else { "web".into() },
    };

    let shared = Arc::new(Mutex::new(Shared::default()));
    let caches: Caches = Arc::new(Mutex::new(BTreeMap::new()));
    let asked = Arc::new(Mutex::new(params.clone()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let activity = tc::Refresh::default();
    let note: Arc<Mutex<(String, f64)>> = Arc::new(Mutex::new((String::new(), 0.0)));

    {
        let (shared, caches, asked, wake, activity, cfg) =
            (Arc::clone(&shared), Arc::clone(&caches), Arc::clone(&asked), Arc::clone(&wake), activity.clone(), cfg.clone());
        std::thread::spawn(move || {
            tc::record_panics();
            let mut round = Round::default();
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                let params = asked.lock().map(|p| p.clone()).unwrap_or_else(|p| p.into_inner().clone());
                poll_round(&cfg, &params, &shared, &caches, &mut round, &activity);
                let (lock, cond) = &*wake;
                let Ok(mut woken) = lock.lock() else { return };
                if !*woken {
                    woken = match cond.wait_timeout(woken, Duration::from_secs_f64(TICK)) {
                        Ok((g, _)) => g,
                        Err(_) => return,
                    };
                }
                *woken = false;
            }));
            let why = match outcome {
                Ok(()) => "the poller stopped: a lock it shares with the screen was poisoned".to_string(),
                Err(_) => format!(
                    "the poller stopped: {}",
                    tc::take_panic_reason().unwrap_or_else(|| "it panicked".into())
                ),
            };
            if let Ok(mut g) = shared.lock() {
                g.err = why;
            }
        });
    }
    let poke = |wake: &Arc<(Mutex<bool>, Condvar)>| {
        let (lock, cond) = &**wake;
        if let Ok(mut woken) = lock.lock() {
            *woken = true;
            cond.notify_all();
        }
    };

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let p = palette();
    let details: Arc<Mutex<HashMap<String, Result<Detail, String>>>> = Arc::new(Mutex::new(HashMap::new()));
    let fetching: Arc<Mutex<HashSet<String>>> = Arc::new(Mutex::new(HashSet::new()));
    let mut metric = Metric::Impressions;
    let mut at = 0usize;
    let (mut selected, mut scroll, mut oscroll) = (0usize, 0usize, 0usize);
    let mut starts = [0usize; 6];
    let (mut needle, mut typing) = (String::new(), false);
    let mut open: Option<Item> = None;
    let mut moved = false;
    let mut items: Vec<(Item, Section)> = Vec::new();
    let (mut placed, mut head): (Vec<(usize, usize)>, usize) = (Vec::new(), 1);

    loop {
        let tick = tc::animation_tick();
        // An open `/` filter takes `!` as a character, not as the notices key.
        if typing {
            keyboard.typing();
        }
        let mut keys = keyboard.poll();
        if open.is_none() {
            if let Some(hit) = tc::rows_clicked(&mut keys, Some(selected), head, scroll, &placed, None) {
                selected = hit;
                moved = true;
            }
        }
        let snapshot = shared.lock().map(|g| g.sites.clone()).unwrap_or_default();
        // Tell the poller which site is on screen. A newly shown site wakes
        // it, so whatever of that site is due is read now, not next tick.
        let on_screen = snapshot.get(at.min(snapshot.len().saturating_sub(1))).map(|s| s.site.clone()).unwrap_or_default();
        let shown_new = shared
            .lock()
            .map(|mut g| {
                let changed = g.focus != on_screen;
                g.focus = on_screen;
                changed
            })
            .unwrap_or(false);
        if shown_new {
            poke(&wake);
        }
        for key in keys {
            if typing {
                match key.as_str() {
                    "esc" => {
                        needle.clear();
                        typing = false;
                    }
                    "enter" => typing = false,
                    "backspace" => {
                        needle.pop();
                    }
                    other if other.chars().count() == 1 => needle.push_str(other),
                    _ => {}
                }
                selected = 0;
                starts = [0; 6];
                moved = true;
                continue;
            }
            let site = snapshot.get(at);
            if let Some(item) = open.clone() {
                match key.as_str() {
                    "," => tc::run_settings(&mut keyboard, SETTINGS),
                    "q" | "Q" => {
                        keyboard.restore();
                        tc::restore_screen();
                        return;
                    }
                    "esc" | "left" => open = None,
                    "up" | "k" | "ctrl-y" | "wheel-up" => oscroll = oscroll.saturating_sub(1),
                    "down" | "j" | "ctrl-e" | "wheel-down" => oscroll = oscroll.saturating_add(1),
                    "pgup" => oscroll = oscroll.saturating_sub(tc::size().1.saturating_sub(3).max(1)),
                    "pgdn" => oscroll = oscroll.saturating_add(tc::size().1.saturating_sub(3).max(1)),
                    "home" => oscroll = 0,
                    "end" => oscroll = usize::MAX / 2,
                    "c" | "C" => {
                        if let Some(text) = copy_text(&item, &snapshot, at) {
                            let ok = tc::clipboard(&text);
                            if let Ok(mut n) = note.lock() {
                                *n = (if ok { "✓ copied".into() } else { "! no clipboard here; select the text instead".into() }, tc::now() + 3.0);
                            }
                        }
                    }
                    "l" | "L" => {
                        let link = site.and_then(|s| page_of(&item, s)).and_then(|u| {
                            let g = caches.lock().ok()?;
                            let e = g.get(&site?.site)?.pages.get(&u)?;
                            e.result.as_ref().ok().map(|i| i.link.clone()).filter(|l| !l.is_empty())
                        });
                        if let Ok(mut n) = note.lock() {
                            *n = match link {
                                Some(l) if tc::clipboard(&l) => ("✓ copied the Search Console link".into(), tc::now() + 3.0),
                                Some(_) => ("! no clipboard here; the link is shown under INSPECTION".into(), tc::now() + 3.0),
                                None => ("! no Search Console link until the page is inspected".into(), tc::now() + 3.0),
                            };
                        }
                    }
                    "i" | "I" => {
                        if let Some(url) = site.and_then(|s| page_of(&item, s)) {
                            inspect_now(&cfg, site.map(|s| s.site.clone()).unwrap_or_default(), url, &caches, &note, &activity);
                        }
                    }
                    "r" | "R" => match site.and_then(|s| detail_key(s, &item)) {
                        Some(key) => {
                            if let Ok(mut g) = details.lock() {
                                g.remove(&key);
                            }
                        }
                        // A property or a plain sitemap is drawn from the
                        // main round, so asking again is a refresh of it.
                        None => {
                            force_read(&shared);
                            poke(&wake);
                        }
                    },
                    _ => {}
                }
                continue;
            }
            match key.as_str() {
                "," => tc::run_settings(&mut keyboard, SETTINGS),
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" | "j" => {
                    selected = (selected + 1).min(items.len().saturating_sub(1));
                    moved = true;
                }
                "home" => {
                    selected = 0;
                    moved = true;
                }
                "end" => {
                    selected = items.len().saturating_sub(1);
                    moved = true;
                }
                "tab" => {
                    // The first row of the next section that has any,
                    // round to the first again from the last.
                    let here = items.get(selected).map(|(_, s)| s.slot()).unwrap_or(0);
                    let next = items
                        .iter()
                        .position(|(_, s)| s.slot() > here)
                        .or_else(|| (!items.is_empty()).then_some(0));
                    if let Some(n) = next {
                        selected = n;
                        moved = true;
                    }
                }
                "left" | "right" if snapshot.len() > 1 => {
                    let n = snapshot.len();
                    at = if key == "right" { (at + 1) % n } else { (at + n - 1) % n };
                    selected = 0;
                    starts = [0; 6];
                    scroll = 0;
                }
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                "pgup" => scroll = scroll.saturating_sub(tc::size().1.saturating_sub(3).max(1)),
                "pgdn" => scroll = scroll.saturating_add(tc::size().1.saturating_sub(3).max(1)),
                "enter" => {
                    if let Some((item, _)) = items.get(selected) {
                        open = Some(item.clone());
                        oscroll = 0;
                    }
                }
                "m" | "M" => metric = metric.next(params.has_position()),
                "w" | "W" => {
                    params.days = tc::cycle(WINDOWS, params.days);
                    if let Ok(mut a) = asked.lock() {
                        *a = params.clone();
                    }
                    poke(&wake);
                }
                "d" | "D" => {
                    params.fresh = !params.fresh;
                    if let Ok(mut a) = asked.lock() {
                        *a = params.clone();
                    }
                    poke(&wake);
                }
                "t" | "T" => {
                    let next = tc::cycle(KINDS, params.kind.as_str());
                    params.kind = next.to_string();
                    if let Ok(mut a) = asked.lock() {
                        *a = params.clone();
                    }
                    poke(&wake);
                }
                "i" | "I" => {
                    if let (Some(site), Some((item, _))) = (site, items.get(selected)) {
                        match page_of(item, site) {
                            Some(url) => inspect_now(&cfg, site.site.clone(), url, &caches, &note, &activity),
                            None => {
                                if let Ok(mut n) = note.lock() {
                                    *n = ("! i inspects a page; this row is not one".into(), tc::now() + 3.0);
                                }
                            }
                        }
                    }
                }
                "/" => typing = true,
                // An accepted filter is cleared the way a typed one is.
                "esc" if !needle.is_empty() => {
                    needle.clear();
                    selected = 0;
                    starts = [0; 6];
                    moved = true;
                }
                "c" | "C" => {
                    if let Some(text) = items.get(selected).and_then(|(item, _)| copy_text(item, &snapshot, at)) {
                        let ok = tc::clipboard(&text);
                        if let Ok(mut n) = note.lock() {
                            *n = (if ok { format!("✓ copied {}", text) } else { "! no clipboard here; select the text instead".into() }, tc::now() + 3.0);
                        }
                    }
                }
                "r" | "R" => {
                    force_read(&shared);
                    poke(&wake);
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (sites, err) = match shared.lock() {
            Ok(g) => (g.sites.clone(), g.err.clone()),
            Err(_) => (Vec::new(), "the screen lost its link to the poller".to_string()),
        };
        if at >= sites.len() {
            at = 0;
        }
        let caches_now = caches.lock().map(|g| g.clone()).unwrap_or_default();
        let flash = note
            .lock()
            .ok()
            .filter(|n| tc::now() < n.1)
            .map(|n| n.0.clone())
            .unwrap_or_default();
        // Discover has no position: one chosen before `t` moved to it gives way.
        if metric == Metric::Position && !params.has_position() {
            metric = Metric::Impressions;
        }
        let screen = Screen {
            sites: &sites,
            at,
            caches: &caches_now,
            metric,
            params: &params,
            inspect_top: cfg.inspect_top,
            needle: &needle,
            typing,
            err: &err,
            selected,
            now: tc::now(),
            busy: activity.tick(tick),
        };

        if let Some(item) = open.clone() {
            // Detail: fetch on demand, once per item and parameters.
            let fetched = sites.get(at).and_then(|site| {
                let key = detail_key(site, &item)?;
                let held = details.lock().ok().and_then(|g| g.get(&key).cloned());
                if held.is_none() {
                    start_detail(&cfg, site, &item, key, &details, &fetching, &activity);
                }
                held
            });
            placed.clear();
            let body = detail(&screen, &item, fetched.as_ref(), &p, w);
            let mut hints: Vec<Vec<(&str, String)>> = vec![
                vec![(p.dim.as_str(), "↑↓ scroll".into())],
                vec![(p.dim.as_str(), "← or esc to close".into())],
                vec![(p.dim.as_str(), "[c]opy".into())],
            ];
            if matches!(item, Item::Page(_) | Item::Inspected(_)) {
                hints.push(vec![(p.dim.as_str(), "[l]ink to Search Console".into())]);
                hints.push(vec![(p.dim.as_str(), "[i]nspect now".into())]);
            }
            hints.push(vec![(p.dim.as_str(), "[r]efetch".into())]);
            hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
            hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
            let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
            let mut foot: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();
            let flash_rows = if flash.is_empty() { Vec::new() } else { flow(" ", &[(p.warn.as_str(), flash.clone())], "", w) };
            let flashed = flash_rows.len();
            foot.splice(0..0, flash_rows);
            let room = h.saturating_sub(foot.len()).max(1);
            let rest = body.rows.len().saturating_sub(1);
            oscroll = oscroll.min(rest.saturating_sub(room.saturating_sub(1)));
            let mut frame: Vec<String> = body.rows.iter().take(1).cloned().collect();
            frame.extend(body.rows.iter().skip(1 + oscroll).take(room.saturating_sub(1)).cloned());
            while frame.len() < room {
                frame.push(String::new());
            }
            let foot_top = frame.len() + flashed;
            frame.extend(foot);
            // Charts scroll with the rows they sit on and are cropped, not
            // rescaled, where the window cuts them.
            let plots: Vec<tc::Plot> = body
                .plots
                .into_iter()
                .filter_map(|plot| plot.in_viewport(oscroll, 1, room.saturating_sub(1)))
                .collect();
            tc::draw_plots(&frame, w, h, &plots);
            keyboard.footer_at(&packed, foot_top, 1);
            keyboard.wait(Duration::from_millis(200));
            continue;
        }

        let built = list(&screen, &mut starts, &p, w);
        items = built.items;
        if !items.is_empty() && selected >= items.len() {
            selected = items.len() - 1;
        }
        let next_metric = metric.next(params.has_position()).name();
        let next_window = tc::cycle(WINDOWS, params.days);
        let next_kind = tc::cycle(KINDS, params.kind.as_str());
        let mut hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.dim.as_str(), "↑↓ select".into())],
            vec![(p.dim.as_str(), "tab section".into())],
        ];
        if sites.len() > 1 {
            hints.push(vec![(p.dim.as_str(), "←→ site".into())]);
        }
        hints.push(vec![(p.dim.as_str(), "↵ open".into())]);
        hints.push(vec![(p.dim.as_str(), format!("[m]etric {}", next_metric))]);
        hints.push(vec![(p.dim.as_str(), format!("[w]indow {}d", next_window))]);
        hints.push(vec![(p.dim.as_str(), format!("[d]ata {}", if params.fresh { "final" } else { "fresh" }))]);
        hints.push(vec![(p.dim.as_str(), format!("[t]ype {}", next_kind))]);
        hints.push(vec![(p.dim.as_str(), "[i]nspect".into())]);
        hints.push(vec![(p.dim.as_str(), "[/]filter".into())]);
        hints.push(vec![(p.dim.as_str(), "[c]opy".into())]);
        hints.push(vec![(p.dim.as_str(), "[r]efresh".into())]);
        hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
        hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
        let packed = tc::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
        let mut foot: Vec<String> = packed.lines.iter().map(|l| format!(" {}", l)).collect();
        let flash_rows = if flash.is_empty() { Vec::new() } else { flow(" ", &[(p.warn.as_str(), flash.clone())], "", w) };
        let flashed = flash_rows.len();
        foot.splice(0..0, flash_rows);
        let room = h.saturating_sub(foot.len()).max(1);
        head = 1.min(built.rows.len());
        let below = room.saturating_sub(head);
        // Only on the frame a key moved the cursor: following every frame
        // drags the view back from wherever the wheel put it.
        if moved && below > 0 {
            if let Some(cursor) = built.cursor {
                scroll = tc::follow(scroll, cursor.saturating_sub(head), below);
            }
            moved = false;
        }
        scroll = scroll.min(built.rows.len().saturating_sub(head).saturating_sub(below));
        placed = built.placed;
        let mut frame: Vec<String> = built.rows.iter().take(head).cloned().collect();
        frame.extend(built.rows.iter().skip(head + scroll).take(below).cloned());
        while frame.len() < room {
            frame.push(String::new());
        }
        let foot_top = frame.len() + flashed;
        frame.extend(foot);
        let plots: Vec<tc::Plot> = built
            .plots
            .into_iter()
            .filter_map(|plot| plot.in_viewport(scroll, head, below))
            .collect();
        tc::draw_plots(&frame, w, h, &plots);
        keyboard.footer_at(&packed, foot_top, 1);
        keyboard.wait(Duration::from_millis(250));
    }
}

/// Inspect one page now, on a thread, spending one of today's budget.
/// Refused outright once the budget is spent.
fn inspect_now(cfg: &Config, site: String, url: String, caches: &Caches, note: &Arc<Mutex<(String, f64)>>, activity: &tc::Refresh) {
    let say = |note: &Arc<Mutex<(String, f64)>>, text: String| {
        if let Ok(mut n) = note.lock() {
            *n = (text, tc::now() + 6.0);
        }
    };
    if cfg.inspect_top == 0 {
        say(note, "! inspection is off while inspect_top is 0".into());
        return;
    }
    // Reserved under the lock, so a second press - or the poller's own
    // round - cannot spend the same last inspection or ask twice at once.
    let reserved = caches
        .lock()
        .map_err(|_| "the inspection cache is unavailable".to_string())
        .and_then(|mut g| {
            let c = g.entry(site.clone()).or_insert_with(|| load_cache(&site));
            c.roll(&google_today().format("%Y-%m-%d").to_string(), tc::now());
            c.reserve(&url)
        });
    if let Err(why) = reserved {
        say(note, format!("! {}", why));
        return;
    }
    say(note, format!("inspecting {}…", url));
    let (cfg, caches, note, activity) = (cfg.clone(), Arc::clone(caches), Arc::clone(note), activity.clone());
    std::thread::spawn(move || {
        let work = activity.start();
        let said = match signed_in(&cfg) {
            Err(why) => {
                if let Ok(mut g) = caches.lock() {
                    g.entry(site.clone()).or_default().release(&url);
                }
                format!("! {}", why)
            }
            Ok((_, client)) => {
                let answer = api::inspect(&client, &site, &url);
                match caches.lock() {
                    Err(_) => "! the inspection cache is unavailable".to_string(),
                    Ok(mut g) => {
                        let c = g.entry(site.clone()).or_default();
                        let said = match keep_inspection(c, &url, answer, tc::now()) {
                            InspectionOutcome::Inspected(verdict) => format!("✓ inspected: {}", verdict),
                            InspectionOutcome::Page(why) | InspectionOutcome::Round(why) => format!("! {}", why),
                        };
                        match save_cache(&site, c) {
                            Ok(()) => said,
                            Err(why) => format!("{} · not kept: {}", said, why),
                        }
                    }
                }
            }
        };
        drop(work);
        if let Ok(mut n) = note.lock() {
            *n = (said, tc::now() + 6.0);
        }
    });
}

/// Fetch a detail screen's data once, on a thread, for the parameters the
/// site's reading was made with - the same ones its key and labels use.
fn start_detail(
    cfg: &Config,
    site: &SiteData,
    item: &Item,
    key: String,
    details: &Arc<Mutex<HashMap<String, Result<Detail, String>>>>,
    fetching: &Arc<Mutex<HashSet<String>>>,
    activity: &tc::Refresh,
) {
    let Some((dim, value)) = detail_job(site, item) else { return };
    if !fetching.lock().map(|mut g| g.insert(key.clone())).unwrap_or(false) {
        return;
    }
    let window = site.windows.map(|(cur, _)| cur);
    let (cfg, site_name, params, details, fetching, activity) =
        (cfg.clone(), site.site.clone(), site.params.clone(), Arc::clone(details), Arc::clone(fetching), activity.clone());
    std::thread::spawn(move || {
        let work = activity.start();
        let got = match signed_in(&cfg) {
            Err(why) => Err(why),
            Ok((_, client)) if dim == "sitemap" => Ok(fetch_children(&client, &site_name, &value)),
            Ok((_, client)) => match window {
                None => Err("no window to read yet: the main screen has not finished its first round".into()),
                Some(cur) => fetch_detail(&client, &site_name, &params, &dim, &value, cur.1, cur),
            },
        };
        drop(work);
        if let Ok(mut g) = details.lock() {
            g.insert(key.clone(), got);
        }
        if let Ok(mut g) = fetching.lock() {
            g.remove(&key);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read_at(t: f64, params: &Params) -> SiteData {
        // A read always leaves a value, a note or an error; a note stands
        // in for the value here.
        let mut d = SiteData::new("https://example.com/", params);
        d.fetched = t;
        d.hours = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.days = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.sitemaps = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.pages = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.queries = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.terms = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d
    }

    fn params() -> Params {
        Params { fresh: false, days: 7, kind: "web".into() }
    }

    #[test]
    fn a_site_never_read_is_read_in_full_when_shown_and_only_its_totals_when_not() {
        let p = params();
        let fresh = SiteData::new("https://example.com/", &p);
        let shown = plan_for(&fresh, &p, true, false, 1_000.0, 900.0);
        assert_eq!(shown, Plan { hours: true, days: true, heavy: Heavy::Yes, sitemaps: true });
        let hidden = plan_for(&fresh, &p, false, false, 1_000.0, 900.0);
        assert_eq!(hidden, Plan { hours: false, days: true, heavy: Heavy::No, sitemaps: true });
    }

    #[test]
    fn the_expensive_reports_wait_for_the_windows_to_move() {
        let p = params();
        let t = 100_000.0;
        let d = read_at(t, &p);
        // A minute later nothing is due.
        assert!(plan_for(&d, &p, true, false, t + 60.0, 900.0).nothing());
        // After `refresh`, the cheap reads are; the expensive ones only if
        // the daily read shows the windows moved.
        let later = plan_for(&d, &p, true, false, t + 901.0, 900.0);
        assert_eq!(later, Plan { hours: true, days: true, heavy: Heavy::IfMoved, sitemaps: false });
        // Six hours on they are read whatever moved, so a correction lands.
        assert_eq!(plan_for(&d, &p, true, false, t + HEAVY_EVERY, 900.0).heavy, Heavy::Yes);
        // Off screen: the daily totals hourly, nothing expensive, ever.
        assert!(plan_for(&d, &p, false, false, t + 900.0, 900.0).nothing());
        let hour = plan_for(&d, &p, false, false, t + OFF_SCREEN_EVERY, 900.0);
        assert_eq!(hour, Plan { hours: false, days: true, heavy: Heavy::No, sitemaps: false });
    }

    #[test]
    fn new_settings_and_r_read_everything_but_a_429_wait_holds_even_against_r() {
        let p = params();
        let t = 100_000.0;
        let d = read_at(t, &p);
        let full = Plan { hours: true, days: true, heavy: Heavy::Yes, sitemaps: true };
        assert_eq!(plan_for(&d, &p, true, true, t + 1.0, 900.0), full);
        let other = Params { days: 28, ..params() };
        assert_eq!(plan_for(&d, &other, true, false, t + 1.0, 900.0), full);
        let mut waiting = d.clone();
        waiting.load_wait_until = Some(t + LOAD_WAIT);
        assert!(plan_for(&waiting, &p, true, true, t + 60.0, 900.0).nothing());
        // Once the wait is over the site is asked again.
        assert!(!plan_for(&waiting, &p, true, false, t + LOAD_WAIT + 1.0, 900.0).nothing());
    }

    #[test]
    fn a_site_read_off_screen_reads_its_hours_the_moment_it_is_shown() {
        // At start the screen has not said which site it shows, so the
        // first round reads every site as off screen: daily totals, no
        // hours. Shown a minute later, its hours must not wait `refresh`.
        let p = params();
        let t = 100_000.0;
        let mut d = SiteData::new("https://example.com/", &p);
        d.fetched = t;
        d.days = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        d.sitemaps = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        let shown = plan_for(&d, &p, true, false, t + 60.0, 900.0);
        assert!(shown.hours, "{shown:?}");
        assert_eq!(shown.heavy, Heavy::Yes, "never read: {shown:?}");
        assert!(!shown.days, "{shown:?}");
    }

    #[test]
    fn a_failed_read_is_retried_on_its_cadence_not_every_minute() {
        let p = params();
        let t = 100_000.0;
        let mut d = read_at(t, &p);
        d.pages = Part::failed("HTTP 500");
        d.hours = Part::failed("HTTP 500");
        // A minute after a failure: neither is asked again yet.
        let soon = plan_for(&d, &p, true, false, t + 60.0, 900.0);
        assert!(!soon.hours && soon.heavy == Heavy::No, "{soon:?}");
        let later = plan_for(&d, &p, true, false, t + 900.0, 900.0);
        assert!(later.hours && later.heavy == Heavy::Yes, "{later:?}");
    }

    #[test]
    fn a_failed_refresh_with_a_kept_value_waits_from_the_failure() {
        // The value from the last success is kept behind the error, with
        // that success's time; the retry is timed from the failure.
        let p = params();
        let t = 100_000.0;
        let mut d = read_at(t, &p);
        // The refresh at t + 900 failed: the reading from t stays on screen
        // behind the error, carrying t as its time.
        d.fetched = t + 900.0;
        d.days = Part::failed("HTTP 500").after(Part::ok(Days::default(), t));
        d.days.tried = t + 900.0;
        assert!(d.days.value.is_some() && d.days.error.is_some() && d.days.at == t);
        // Timed from the kept value, it would be overdue on every tick.
        let soon = plan_for(&d, &p, true, false, t + 960.0, 900.0);
        assert!(!soon.days, "a minute after the failure: {soon:?}");
        let later = plan_for(&d, &p, true, false, t + 1_800.0, 900.0);
        assert!(later.days, "{later:?}");
    }

    #[test]
    fn a_failed_part_is_retried_though_other_parts_keep_succeeding() {
        // Sitemaps failed once. Hours and daily totals then succeed every
        // `refresh` on screen, and daily totals hourly off screen, each
        // moving `fetched`. The sitemaps retry must still come six hours
        // after its own failure, not six hours after the site's last read.
        let p = params();
        let t = 100_000.0;
        for (focus, every) in [(true, 900.0), (false, OFF_SCREEN_EVERY)] {
            let mut d = read_at(t, &p);
            d.sitemaps = Part::failed("HTTP 500").after(Part::ok(Vec::new(), t - 60.0));
            d.sitemaps.tried = t;
            let mut retried = None;
            let mut now = t;
            while now < t + 24.0 * 3600.0 && retried.is_none() {
                now += 60.0;
                let plan = plan_for(&d, &p, focus, false, now, 900.0);
                if plan.sitemaps {
                    retried = Some(now);
                }
                if plan.hours || plan.days {
                    // What fetch_site leaves after a read that worked.
                    d.fetched = now;
                    if plan.hours {
                        d.hours = Part { value: None, note: Some("read".into()), error: None, at: now, tried: now };
                    }
                    if plan.days {
                        d.days = Part { value: None, note: Some("read".into()), error: None, at: now, tried: now };
                    }
                }
            }
            let at = retried.unwrap_or_else(|| panic!("sitemaps never retried (on screen: {focus})"));
            assert!(at - t >= SITEMAPS_EVERY && at - t < SITEMAPS_EVERY + every, "on screen {focus}: retried {}s on", at - t);
        }
    }

    #[test]
    fn sites_shows_totals_for_a_site_whose_movers_were_never_read() {
        // Off screen, only the daily totals are read: no movers, so no
        // `windows`. The SITES row still has the two windows to compare.
        let p = params();
        let mut d = SiteData::new("https://example.com/", &p);
        let end = chrono::NaiveDate::from_ymd_opt(2026, 11, 4).unwrap();
        d.days = Part::ok(Days { final_end: Some(end), fresh_end: Some(end), daily: Vec::new() }, 1.0);
        assert_eq!(d.windows, None);
        assert!(site_totals(&d).is_some());
        // And with no daily reading there is nothing to say.
        assert!(site_totals(&SiteData::new("https://example.com/", &p)).is_none());
    }

    #[test]
    fn a_failed_queries_or_terms_read_is_retried_though_pages_came_back() {
        // Pages came back, then queries hit a 429, which stops the round
        // before terms. Once the wait is over, both are due on the retry
        // cadence - not left until the windows move or six hours pass.
        let p = params();
        let t = 100_000.0;
        let mut d = read_at(t, &p);
        d.queries = Part::failed("429 load quota");
        d.terms = Part::default();
        assert_eq!(plan_for(&d, &p, true, false, t + 60.0, 900.0).heavy, Heavy::Yes, "terms never read");
        d.terms = Part { value: None, note: Some("read".into()), error: None, at: t, tried: t };
        assert_eq!(plan_for(&d, &p, true, false, t + 60.0, 900.0).heavy, Heavy::No, "a minute after the failure");
        assert_eq!(plan_for(&d, &p, true, false, t + 900.0, 900.0).heavy, Heavy::Yes);
        let mut d = read_at(t, &p);
        d.terms = Part::failed("HTTP 500");
        assert_eq!(plan_for(&d, &p, true, false, t + 900.0, 900.0).heavy, Heavy::Yes);
    }

    #[test]
    fn a_refused_property_is_not_asked_again_every_minute() {
        let p = params();
        let t = 100_000.0;
        let mut d = SiteData::new("https://example.com/", &p);
        d.fetched = t;
        d.problem = Some("403 the account is not a user of this property".into());
        assert!(plan_for(&d, &p, true, false, t + 60.0, 900.0).nothing());
        assert!(!plan_for(&d, &p, true, false, t + 900.0, 900.0).nothing());
        // r asks now: the grant may just have been fixed.
        assert!(!plan_for(&d, &p, true, true, t + 60.0, 900.0).nothing());
    }

    #[test]
    fn sign_in_is_worded_by_core_for_this_section() {
        let cfg = Config {
            credentials: String::new(),
            credentials_file: String::new(),
            sites: Vec::new(),
            refresh: 600.0,
            inspect_top: 25,
        };
        let Err(row) = signed_in(&cfg) else { panic!("no credential must not sign in") };
        assert_eq!(row, google::missing_credential("google_search_console"));
        assert!(row.starts_with("no credential: set google_search_console.credentials "), "{row}");
        assert!(row.contains("google_search_console.credentials_file "), "{row}");
        assert!(row.ends_with(tc::SET_IN_SETTINGS), "{row}");
        // A pasted key that is not one says so, rather than falling through
        // to the file or to "no credential".
        let pasted = Config {
            credentials: "{not a key".into(),
            ..cfg
        };
        let Err(row) = signed_in(&pasted) else { panic!("a broken paste must not sign in") };
        assert_ne!(row, google::missing_credential("google_search_console"), "{row}");
    }

    fn site(url: &str, permission: &str) -> Site {
        Site { url: url.into(), permission: permission.into() }
    }

    const EMAIL: &str = "reader@example-project.iam.gserviceaccount.com";

    #[test]
    fn an_empty_list_discovers_every_property() {
        let (plan, err) = plan_sites(&[], Ok(vec![site("https://example.org/", "siteOwner"), site("sc-domain:example.com", "siteRestrictedUser")]), Some(EMAIL), &[]);
        assert_eq!(err, None);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].0, "https://example.org/");
    }

    #[test]
    fn an_account_that_sees_nothing_says_whom_to_add() {
        let (plan, err) = plan_sites(&[], Ok(Vec::new()), Some(EMAIL), &[]);
        assert!(plan.is_empty());
        assert!(err.unwrap().contains(&format!("add {} as a Restricted user", EMAIL)));
    }

    #[test]
    fn a_configured_site_the_account_cannot_see_is_named_with_its_form() {
        let listed = Ok(vec![site("sc-domain:example.com", "siteRestrictedUser")]);
        let (plan, _) = plan_sites(&["https://example.com".into()], listed, Some(EMAIL), &[]);
        let problem = plan[0].2.as_deref().unwrap();
        assert!(problem.contains("is not among the 1 properties"), "{problem}");
        assert!(problem.contains("trailing slash"), "{problem}");
        assert!(problem.contains("the account does see sc-domain:example.com"), "{problem}");
    }

    #[test]
    fn a_failed_discovery_reads_on_the_properties_it_last_found() {
        let last = vec![
            ("https://example.org/".to_string(), "siteOwner".to_string(), None),
            ("sc-domain:example.com".to_string(), "siteRestrictedUser".to_string(), Some("403 old".to_string())),
        ];
        let failed = || Err(Fail { status: 500, said: "HTTP 500".into() });
        let (plan, err) = plan_sites(&[], failed(), Some(EMAIL), &last);
        assert_eq!(plan.iter().map(|t| t.0.as_str()).collect::<Vec<_>>(), ["https://example.org/", "sc-domain:example.com"]);
        assert_eq!(plan[1].1, "siteRestrictedUser");
        // The site's own refusal is kept by the rebuild, not restated here.
        assert_eq!(plan[1].2, None);
        assert!(err.unwrap().contains("the last list is read on: HTTP 500"));
        // Nothing found before: nothing to read on.
        let (plan, err) = plan_sites(&[], failed(), Some(EMAIL), &[]);
        assert!(plan.is_empty() && err.unwrap().contains("listing the properties failed: HTTP 500"));
        // A refused token is refused for every property.
        let refused = Err(Fail { status: 401, said: "401 refused".into() });
        assert!(plan_sites(&[], refused, Some(EMAIL), &last).0.is_empty());
        // A list that came back empty is an answer, not a failure.
        assert!(plan_sites(&[], Ok(Vec::new()), Some(EMAIL), &last).0.is_empty());
    }

    #[test]
    fn a_refused_token_is_one_error_not_one_per_site() {
        let refused = Err(Fail { status: 401, said: "401 the access token was refused".into() });
        let (plan, err) = plan_sites(&["sc-domain:example.com".into()], refused, None, &[]);
        assert!(plan.is_empty());
        assert_eq!(err.as_deref(), Some("401 the access token was refused"));
    }

    #[test]
    fn without_the_list_a_wrong_form_is_still_caught() {
        let failed = Err(Fail { status: 500, said: "HTTP 500".into() });
        let (plan, err) = plan_sites(&["example.com".into(), "sc-domain:example.com".into()], failed, None, &[]);
        assert!(err.unwrap().contains("each is asked directly"));
        assert!(plan[0].2.as_deref().unwrap().contains("sc-domain:example.com"));
        assert_eq!(plan[1].2, None);
    }

    #[test]
    fn a_detail_is_filed_under_its_parameters() {
        let p7 = Params { fresh: false, days: 7, kind: "web".into() };
        let p28 = Params { days: 28, ..p7.clone() };
        let d = SiteData::new("sc-domain:example.com", &p7);
        let item = Item::Page("https://example.com/".into());
        assert_ne!(detail_key(&d, &item), detail_key(&SiteData::new("sc-domain:example.com", &p28), &item));
        // The day moving on moves the window, and with it the key.
        let mut later = d.clone();
        later.windows = Some(windows(parse_date("2026-10-18").unwrap(), 7));
        assert_ne!(detail_key(&d, &item), detail_key(&later, &item));
        // Nothing is fetched for a property, so there is nothing to file.
        assert_eq!(detail_key(&d, &Item::Site(0)), None);
    }

    fn term(query: &str) -> Term {
        Term { query: query.into(), page: "https://example.com/".into(), m: Metrics::default(), prior_clicks: None, others: Vec::new() }
    }

    #[test]
    fn a_detail_is_filed_under_what_its_row_names_not_where_it_sits() {
        let p = Params { fresh: false, days: 7, kind: "web".into() };
        let mut before = SiteData::new("sc-domain:example.com", &p);
        before.terms = Part::ok(vec![term("example widget"), term("example tool")], 1.0);
        let mut after = before.clone();
        after.terms = Part::ok(vec![term("example tool"), term("example widget")], 2.0);
        // Row 0 is another query after the refresh, so its detail is too.
        assert_ne!(detail_key(&before, &Item::Term(0)), detail_key(&after, &Item::Term(0)));
        assert_eq!(detail_key(&before, &Item::Term(0)), detail_key(&after, &Item::Term(1)));
    }

    #[test]
    fn a_refused_inspection_stops_the_round_and_stays_counted() {
        let mut c = InspectionCache::default();
        c.roll("2026-10-21", 10.0);
        c.reserve("https://example.com/a").unwrap();
        let refused = Err(Fail { status: 429, said: "429 quota".into() });
        assert_eq!(settle(&mut c, "https://example.com/a", refused, 11.0).as_deref(), Some("429 quota"));
        // Counted, and so in what the save that follows writes to disk.
        // Not stored as the page's result: the quota refused every page.
        assert_eq!(c.used, 1);
        assert!(c.inflight.is_empty());
        assert!(!c.pages.contains_key("https://example.com/a"));
        assert_eq!(parse_inspection_cache(&c.to_json()).used, 1);
        // Never reached: the slot comes back and the round still stops.
        c.reserve("https://example.com/b").unwrap();
        let unreached = Err(Fail { status: 0, said: "could not reach Google".into() });
        assert!(settle(&mut c, "https://example.com/b", unreached, 12.0).is_some());
        assert_eq!(c.used, 1);
        // A refusal about one page is that page's answer, and the round goes on.
        c.reserve("https://example.com/c").unwrap();
        let one = Err(Fail { status: 400, said: "HTTP 400 · outside the property".into() });
        assert_eq!(settle(&mut c, "https://example.com/c", one, 13.0), None);
        assert_eq!(c.used, 2);
        assert!(c.pages.contains_key("https://example.com/c"));
        // An expired token on `i` is the same classification: counted,
        // because Google received it, and not this page's result for a day.
        c.reserve("https://example.com/d").unwrap();
        let expired = Err(Fail { status: 401, said: "401 the access token was refused".into() });
        assert_eq!(
            keep_inspection(&mut c, "https://example.com/d", expired, 14.0),
            InspectionOutcome::Round("401 the access token was refused".into())
        );
        assert!(!c.pages.contains_key("https://example.com/d"));
        assert_eq!(c.used, 3);
    }

    #[test]
    fn a_reading_is_on_screen_before_its_inspections_and_after_them() {
        let p = Params { fresh: false, days: 7, kind: "web".into() };
        let shared = Arc::new(Mutex::new(Shared { sites: vec![SiteData::new("sc-domain:example.com", &p)], ..Shared::default() }));
        let mut data = SiteData::new("sc-domain:example.com", &p);
        data.sitemaps = Part::ok(Vec::new(), 5.0);
        // Before any inspection: the numbers are already there.
        publish(&shared, &p, &data);
        assert_eq!(shared.lock().unwrap().sites[0].sitemaps.value, Some(Vec::new()));
        // After: the same reading, now with what inspecting said.
        data.inspect_error = Some("inspection stopped for this round: 429 quota".into());
        publish(&shared, &p, &data);
        let on_screen = &shared.lock().unwrap().sites[0];
        assert_eq!(on_screen.sitemaps.value, Some(Vec::new()));
        assert_eq!(on_screen.inspect_error.as_deref(), Some("inspection stopped for this round: 429 quota"));
    }

    #[test]
    fn copying_a_site_row_copies_that_property() {
        let p = Params { fresh: false, days: 7, kind: "web".into() };
        let sites = vec![SiteData::new("https://example.org/", &p), SiteData::new("sc-domain:example.com", &p)];
        assert_eq!(copy_text(&Item::Site(1), &sites, 0).as_deref(), Some("sc-domain:example.com"));
        assert_eq!(copy_text(&Item::Site(0), &sites, 1).as_deref(), Some("https://example.org/"));
    }
}
