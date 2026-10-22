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

/// The variable a pasted token is read from when `token_env` is unset;
/// spelled out for the settings check, and held to core's by a test.
const TOKEN_ENV: &str = "GOOGLE_OAUTH_ACCESS_TOKEN";
/// The shortest wait between refreshes. The data moves hourly at best, so
/// anything shorter only spends quota.
const MIN_REFRESH: f64 = 120.0;
/// The search types `t` steps through.
const KINDS: &[&str] = &["web", "image", "video", "news", "discover"];
/// The windows `w` steps between.
const WINDOWS: &[i64] = &[7, 28];

/// Everything read from the widget's own config section.
#[derive(Clone)]
struct Config {
    credentials_file: String,
    token: String,
    token_env: String,
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
    /// A key file others can read, or a pasted token's one-hour life.
    warning: String,
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
    let Some(cred) = google::credential(&cfg.credentials_file, &cfg.token, &cfg.token_env) else {
        return Err(google::missing_credential(SETTINGS.section, &cfg.token_env));
    };
    // Read-only: it cannot submit a sitemap or change a property.
    let token = cred.bearer(&[google::SCOPE_WEBMASTERS_READONLY])?;
    let client = Client { bearer: token.value, email: cred.client_email() };
    Ok((cred, client))
}

/// Which properties to read, and the problem with any that cannot be.
fn resolve_sites(cfg: &Config, client: &Client) -> (Vec<(String, String, Option<String>)>, Option<String>) {
    plan_sites(&cfg.sites, api::sites(client), client.email.as_deref())
}

/// `resolve_sites` apart from the network: the configured names against
/// what `sites.list` answered, as (site, permission, problem) and an error
/// that applies to all of them.
#[allow(clippy::type_complexity)]
fn plan_sites(
    configured: &[String],
    listed: Result<Vec<Site>, Fail>,
    email: Option<&str>,
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
        (true, Err(f)) => (Vec::new(), Some(format!("listing the properties failed: {}", f.said))),
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

/// Inspect whatever of the site's set is due, within today's budget.
fn inspect_due(client: &Client, data: &mut SiteData, caches: &Caches, top: usize) {
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
        match answer {
            Ok((raw, i)) => c.record(&url, tc::now(), raw, Ok(i)),
            Err(f) if f.status == 429 || f.status == 401 || f.status == 403 || f.status == 0 => {
                // Google never reached is a request never counted.
                if f.status == 0 {
                    c.release(&url);
                } else {
                    c.abandon(&url);
                }
                data.inspect_error = Some(format!("inspection stopped for this round: {}", f.said));
                break;
            }
            Err(f) => c.record(&url, tc::now(), String::new(), Err(f.said)),
        }
        if let Err(why) = save_cache(&data.site, c) {
            data.inspect_error = Some(format!("inspections are not being kept: {}", why));
        }
    }
}

/// One refresh of every property.
fn poll_round(cfg: &Config, params: &Params, shared: &Arc<Mutex<Shared>>, caches: &Caches) {
    let (_, client) = match signed_in(cfg) {
        Ok(ok) => ok,
        Err(why) => {
            if let Ok(mut g) = shared.lock() {
                g.err = why;
            }
            return;
        }
    };
    let warning = google::credential(&cfg.credentials_file, &cfg.token, &cfg.token_env)
        .and_then(|c| c.warning())
        .or_else(tc::config_token_warning)
        .unwrap_or_default();
    let (targets, err) = resolve_sites(cfg, &client);
    if let Ok(mut g) = shared.lock() {
        g.err = err.unwrap_or_default();
        g.warning = warning;
        // Keep what is already on screen for sites still wanted; add the
        // rest as waiting so the site count is right from the start.
        let old = std::mem::take(&mut g.sites);
        g.sites = targets
            .iter()
            .map(|(site, permission, problem)| {
                let mut d = old.iter().find(|o| o.site == *site).cloned().unwrap_or_else(|| SiteData::new(site, params));
                d.permission = permission.clone();
                d.problem = problem.clone();
                d
            })
            .collect();
    }
    for (site, permission, problem) in targets {
        if problem.is_some() {
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
            fetch_site(&client, &site, params, google_today(), Utc::now())
        };
        data.permission = permission;
        if data.problem.is_none() && cfg.inspect_top > 0 {
            inspect_due(&client, &mut data, caches, cfg.inspect_top);
        }
        if let Ok(mut g) = shared.lock() {
            if let Some(slot) = g.sites.iter_mut().find(|o| o.site == site) {
                let old = std::mem::replace(slot, SiteData::new(&site, params));
                *slot = data.after(old);
            }
        }
    }
}

/// The key a fetched detail is filed under, so a page asked about for
/// another window is not answered from the wrong one.
fn detail_key(site: &str, item: &Item, params: &Params) -> String {
    format!("{}|{:?}|{}|{}|{}", site, item, params.fresh, params.days, params.kind)
}

/// What `c` copies for an item.
fn copy_text(item: &Item, site: Option<&SiteData>) -> Option<String> {
    let site = site?;
    Some(match item {
        Item::Page(u) | Item::Inspected(u) => u.clone(),
        Item::Query(q) => q.clone(),
        Item::Term(n) => site.terms.value.as_ref()?.get(*n)?.query.clone(),
        Item::Sitemap(n) => site.sitemaps.value.as_ref()?.get(*n)?.path.clone(),
        Item::Site(_) => site.site.clone(),
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
        credentials_file: tc::cfg_str(&cfg, "credentials_file", ""),
        token: tc::cfg_str(&cfg, "token", ""),
        token_env: tc::cfg_str(&cfg, "token_env", TOKEN_ENV),
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
            let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| loop {
                let params = asked.lock().map(|p| p.clone()).unwrap_or_else(|p| p.into_inner().clone());
                let work = activity.start();
                poll_round(&cfg, &params, &shared, &caches);
                drop(work);
                let (lock, cond) = &*wake;
                let Ok(mut woken) = lock.lock() else { return };
                if !*woken {
                    woken = match cond.wait_timeout(woken, Duration::from_secs_f64(cfg.refresh)) {
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
        let mut keys = keyboard.poll();
        if open.is_none() {
            if let Some(hit) = tc::rows_clicked(&mut keys, Some(selected), head, scroll, &placed, None) {
                selected = hit;
                moved = true;
            }
        }
        let snapshot = shared.lock().map(|g| g.sites.clone()).unwrap_or_default();
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
                        if let Some(text) = copy_text(&item, site) {
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
                    "r" | "R" => {
                        if let (Some(site), Ok(mut g)) = (site, details.lock()) {
                            g.remove(&detail_key(&site.site, &item, &params));
                        }
                    }
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
                "m" | "M" => metric = tc::cycle(&METRICS, metric),
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
                "c" | "C" => {
                    if let Some(text) = items.get(selected).and_then(|(item, _)| copy_text(item, site)) {
                        let ok = tc::clipboard(&text);
                        if let Ok(mut n) = note.lock() {
                            *n = (if ok { format!("✓ copied {}", text) } else { "! no clipboard here; select the text instead".into() }, tc::now() + 3.0);
                        }
                    }
                }
                "r" | "R" => poke(&wake),
                _ => {}
            }
        }

        let (w, h) = tc::size();
        let (sites, err, warning) = match shared.lock() {
            Ok(g) => (g.sites.clone(), g.err.clone(), g.warning.clone()),
            Err(_) => (Vec::new(), "the screen lost its link to the poller".to_string(), String::new()),
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
            warning: &warning,
            selected,
            now: tc::now(),
            busy: activity.tick(tick),
        };

        if let Some(item) = open.clone() {
            // Detail: fetch on demand, once per item and parameters.
            let fetched = sites.get(at).and_then(|site| {
                let key = detail_key(&site.site, &item, &params);
                let held = details.lock().ok().and_then(|g| g.get(&key).cloned());
                if held.is_none() {
                    start_detail(&cfg, site, &item, &params, key, &details, &fetching, &activity);
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
            let rest = body.len().saturating_sub(1);
            oscroll = oscroll.min(rest.saturating_sub(room.saturating_sub(1)));
            let mut frame: Vec<String> = body.iter().take(1).cloned().collect();
            frame.extend(body.iter().skip(1 + oscroll).take(room.saturating_sub(1)).cloned());
            while frame.len() < room {
                frame.push(String::new());
            }
            let foot_top = frame.len() + flashed;
            frame.extend(foot);
            tc::draw(&frame, w, h);
            keyboard.footer_at(&packed, foot_top, 1);
            keyboard.wait(Duration::from_millis(200));
            continue;
        }

        let built = list(&screen, &mut starts, &p, w);
        items = built.items;
        if !items.is_empty() && selected >= items.len() {
            selected = items.len() - 1;
        }
        let next_metric = tc::cycle(&METRICS, metric).name();
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
        tc::draw(&frame, w, h);
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
                        let said = match answer {
                            Ok((raw, i)) => {
                                let verdict = i.verdict.clone();
                                c.record(&url, tc::now(), raw, Ok(i));
                                format!("✓ inspected: {}", verdict)
                            }
                            Err(f) if f.status == 0 => {
                                c.release(&url);
                                format!("! {}", f.said)
                            }
                            Err(f) => {
                                c.record(&url, tc::now(), String::new(), Err(f.said.clone()));
                                format!("! {}", f.said)
                            }
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

/// Fetch a detail screen's data once, on a thread.
#[allow(clippy::too_many_arguments)]
fn start_detail(
    cfg: &Config,
    site: &SiteData,
    item: &Item,
    params: &Params,
    key: String,
    details: &Arc<Mutex<HashMap<String, Result<Detail, String>>>>,
    fetching: &Arc<Mutex<HashSet<String>>>,
    activity: &tc::Refresh,
) {
    let job: Option<(String, String)> = match item {
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
    };
    let Some((dim, value)) = job else { return };
    if !fetching.lock().map(|mut g| g.insert(key.clone())).unwrap_or(false) {
        return;
    }
    let window = site.windows.map(|(cur, _)| cur);
    let (cfg, site_name, params, details, fetching, activity) =
        (cfg.clone(), site.site.clone(), params.clone(), Arc::clone(details), Arc::clone(fetching), activity.clone());
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

    #[test]
    fn sign_in_is_worded_by_core_for_this_section() {
        assert_eq!(TOKEN_ENV, google::DEFAULT_TOKEN_ENV);
        let cfg = Config {
            credentials_file: String::new(),
            token: String::new(),
            token_env: "OPSCOPE_TEST_UNSET_TOKEN_VARIABLE".into(),
            sites: Vec::new(),
            refresh: 600.0,
            inspect_top: 25,
        };
        let Err(row) = signed_in(&cfg) else { panic!("no credential must not sign in") };
        assert_eq!(row, google::missing_credential("google_search_console", "OPSCOPE_TEST_UNSET_TOKEN_VARIABLE"));
        assert!(row.starts_with("no credential: set google_search_console.credentials_file "), "{row}");
        assert!(row.ends_with(tc::SET_IN_SETTINGS), "{row}");
    }

    fn site(url: &str, permission: &str) -> Site {
        Site { url: url.into(), permission: permission.into() }
    }

    const EMAIL: &str = "reader@example-project.iam.gserviceaccount.com";

    #[test]
    fn an_empty_list_discovers_every_property() {
        let (plan, err) = plan_sites(&[], Ok(vec![site("https://example.org/", "siteOwner"), site("sc-domain:example.com", "siteRestrictedUser")]), Some(EMAIL));
        assert_eq!(err, None);
        assert_eq!(plan.len(), 2);
        assert_eq!(plan[0].0, "https://example.org/");
    }

    #[test]
    fn an_account_that_sees_nothing_says_whom_to_add() {
        let (plan, err) = plan_sites(&[], Ok(Vec::new()), Some(EMAIL));
        assert!(plan.is_empty());
        assert!(err.unwrap().contains(&format!("add {} as a Restricted user", EMAIL)));
    }

    #[test]
    fn a_configured_site_the_account_cannot_see_is_named_with_its_form() {
        let listed = Ok(vec![site("sc-domain:example.com", "siteRestrictedUser")]);
        let (plan, _) = plan_sites(&["https://example.com".into()], listed, Some(EMAIL));
        let problem = plan[0].2.as_deref().unwrap();
        assert!(problem.contains("is not among the 1 properties"), "{problem}");
        assert!(problem.contains("trailing slash"), "{problem}");
        assert!(problem.contains("the account does see sc-domain:example.com"), "{problem}");
    }

    #[test]
    fn a_refused_token_is_one_error_not_one_per_site() {
        let refused = Err(Fail { status: 401, said: "401 the access token was refused".into() });
        let (plan, err) = plan_sites(&["sc-domain:example.com".into()], refused, None);
        assert!(plan.is_empty());
        assert_eq!(err.as_deref(), Some("401 the access token was refused"));
    }

    #[test]
    fn without_the_list_a_wrong_form_is_still_caught() {
        let failed = Err(Fail { status: 500, said: "HTTP 500".into() });
        let (plan, err) = plan_sites(&["example.com".into(), "sc-domain:example.com".into()], failed, None);
        assert!(err.unwrap().contains("each is asked directly"));
        assert!(plan[0].2.as_deref().unwrap().contains("sc-domain:example.com"));
        assert_eq!(plan[1].2, None);
    }

    #[test]
    fn a_detail_is_filed_under_its_parameters() {
        let p7 = Params { fresh: false, days: 7, kind: "web".into() };
        let p28 = Params { days: 28, ..p7.clone() };
        let item = Item::Page("https://example.com/".into());
        assert_ne!(detail_key("sc-domain:example.com", &item, &p7), detail_key("sc-domain:example.com", &item, &p28));
    }
}
