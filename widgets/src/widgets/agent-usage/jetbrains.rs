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

//! JetBrains AI Assistant: the account's credit quota, as the IDE last
//! recorded it.
//!
//! There is no CLI and no endpoint to ask. Every JetBrains IDE with AI
//! Assistant keeps `options/AIAssistantQuotaManager2.xml` in its config
//! directory and rewrites it whenever it checks the quota, so that file is
//! the source - the same one CodexBar reads. It is the account's figure,
//! not this machine's spend, and it is only as current as the IDE's last
//! look, which the tab says.

use chrono::TimeZone;
use opscope_core as tc;

use crate::parse::{parse_jetbrains_quota, JetBrainsQuota};
use crate::shared::*;
use crate::*;

const QUOTA_FILE: &str = "options/AIAssistantQuotaManager2.xml";

/// Older than this and the reading is marked as cached. The IDE rewrites
/// the file as it is used, so an hour without a write means no IDE has
/// looked in that time, and credits spent elsewhere would not show.
const FRESH_SECS: f64 = 3600.0;

/// Directory prefix to product name. The directory is the product plus its
/// version, `IntelliJIdea2026.2`, and the version is what follows.
const IDES: &[(&str, &str)] = &[
    ("IntelliJIdea", "IntelliJ IDEA"),
    ("IdeaIC", "IntelliJ IDEA CE"),
    // PyCharm Community keeps its config as `PyCharmCE2024.1`.
    ("PyCharmCE", "PyCharm CE"),
    ("PyCharm", "PyCharm"),
    ("WebStorm", "WebStorm"),
    ("GoLand", "GoLand"),
    ("CLion", "CLion"),
    ("DataGrip", "DataGrip"),
    ("RubyMine", "RubyMine"),
    ("Rider", "Rider"),
    ("PhpStorm", "PhpStorm"),
    ("RustRover", "RustRover"),
    ("AndroidStudio", "Android Studio"),
    ("Fleet", "Fleet"),
    ("Aqua", "Aqua"),
    ("DataSpell", "DataSpell"),
];

/// Where JetBrains IDEs keep their config, checked at run time rather than
/// by build target: only the ones that exist are read, and Android Studio
/// lives under Google rather than JetBrains. On Linux the IDEs follow
/// `XDG_CONFIG_HOME` when it is set, with `~/.config` as its default.
fn roots() -> Vec<String> {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok();
    roots_under(xdg.as_deref())
}

fn roots_under(xdg: Option<&str>) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    // An XDG_CONFIG_HOME that is relative is invalid by the spec and ignored.
    if let Some(x) = xdg.map(|x| x.trim_end_matches('/')).filter(|x| x.starts_with('/')) {
        out.push(format!("{}/JetBrains", x));
        out.push(format!("{}/Google", x));
    }
    for r in [
        "Library/Application Support/JetBrains",
        "Library/Application Support/Google",
        ".config/JetBrains",
        ".local/share/JetBrains",
        ".config/Google",
    ] {
        out.push(under_home(r));
    }
    // An XDG root that is ~/.config would otherwise count each IDE twice.
    let mut seen = std::collections::HashSet::new();
    out.retain(|r| seen.insert(r.clone()));
    out
}

/// Product name and version for a config directory, or None when it is
/// not an IDE this knows. The rest of the name has to be a version,
/// `2026.2` or Android Studio's `Preview2026.2`, so a `RustRoverBackup`
/// beside the real one is never read as an IDE.
fn ide_of(dirname: &str) -> Option<String> {
    let lower = dirname.to_lowercase();
    IDES.iter().find_map(|(prefix, name)| {
        if !lower.starts_with(&prefix.to_lowercase()) {
            return None;
        }
        let version = &dirname[prefix.len()..];
        match version.strip_prefix("Preview") {
            Some(v) => is_version(v).then(|| format!("{} Preview {}", name, v)),
            None => is_version(version).then(|| format!("{} {}", name, version)),
        }
    })
}

/// `2026.2`: a four-digit year, a dot, and a release number.
fn is_version(s: &str) -> bool {
    let digits = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    matches!(s.split_once('.'), Some((year, release)) if year.len() == 4 && digits(year) && digits(release))
}

/// Every quota file on this machine, as (IDE, path, modified), and each
/// config root that is there but could not be listed, with why. Only a root
/// that does not exist is passed over in silence: one that refused to be
/// read could hold the newest quota, and saying nothing would draw the pane
/// as if no IDE had recorded one.
/// A quota file as (IDE, path, modified), and a root as (path, why).
type Found = (String, String, f64);
type Unlisted = (String, String);

fn scan() -> (Vec<Found>, Vec<Unlisted>) {
    let mut out = Vec::new();
    let mut unlisted = Vec::new();
    for root in roots() {
        let entries = match std::fs::read_dir(&root) {
            Ok(entries) => entries,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => {
                unlisted.push((root, e.to_string()));
                continue;
            }
        };
        for entry in entries.flatten() {
            let dirname = entry.file_name().to_string_lossy().to_string();
            let Some(ide) = ide_of(&dirname) else {
                continue;
            };
            let path = format!("{}/{}/{}", root, dirname, QUOTA_FILE);
            let Ok(meta) = std::fs::metadata(&path) else {
                continue;
            };
            let modified = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map_or(0.0, |d| d.as_secs_f64());
            out.push((ide, path, modified));
        }
    }
    (out, unlisted)
}

/// What proves JetBrains AI is here: a quota file, or a config root that
/// would not be listed, so the tab appears and can say why it is empty.
pub fn detection_paths() -> Vec<String> {
    let (files, unlisted) = scan();
    files.into_iter().map(|f| f.1).chain(unlisted.into_iter().map(|u| u.0)).collect()
}

#[derive(Clone, Default)]
pub struct Data {
    quota: Option<JetBrainsQuota>,
    /// Which IDE wrote the file read, e.g. `RustRover 2026.2`.
    ide: String,
    /// When it wrote it, as epoch seconds.
    written: f64,
    /// How many IDEs have a quota file, so a reader with two knows the
    /// newest one is the one shown.
    ides: usize,
    /// Newer files were passed over because they held no readable quota.
    skipped: usize,
    /// Another IDE's file has no readable time, so it cannot be ranked and
    /// the one shown cannot claim to be the newest look at the quota.
    undated: bool,
    /// Config roots that are there but could not be listed, with why.
    unlisted: Vec<(String, String)>,
    why: String,
}

/// The newest readable file wins: every IDE on one account records the same
/// quota, and the one written last has the latest look at it. A newer file
/// that holds nothing readable is passed over rather than hiding an older
/// one that does; when none can be read, the newest one's reason is shown.
pub fn read(_caches: &mut Caches, _cfg: &Config) -> Data {
    let (files, unlisted) = scan();
    with_unlisted(pick(files), unlisted)
}

/// A root that could not be listed may hold a newer file than any read, so
/// the one shown is not called the newest; with nothing read at all, the
/// refusal is the reason given rather than "no IDE recorded a quota".
fn with_unlisted(mut d: Data, unlisted: Vec<(String, String)>) -> Data {
    if d.quota.is_none() && d.why.is_empty() {
        if let Some((root, e)) = unlisted.first() {
            d.why = format!("could not list {}: {}", root, e);
        }
    }
    d.unlisted = unlisted;
    d
}

fn pick(mut files: Vec<(String, String, f64)>) -> Data {
    // A time ahead of this clock cannot be ranked any more than a missing
    // one can, so it is treated as undated before sorting rather than
    // winning the sort and hiding a file whose time is good.
    let at = now();
    for f in &mut files {
        if f.2 > at {
            f.2 = 0.0;
        }
    }
    files.sort_by(|a, b| b.2.total_cmp(&a.2));
    let found = files.len();
    let undated = files.iter().filter(|f| f.2 <= 0.0).count();
    let mut first: Option<Data> = None;
    for (skipped, (ide, path, written)) in files.into_iter().enumerate() {
        let mut d = Data {
            ide,
            written,
            ides: found,
            skipped,
            // Another file, not this one, that cannot be ranked.
            undated: undated > usize::from(written <= 0.0),
            ..Data::default()
        };
        match std::fs::read_to_string(&path) {
            Ok(raw) => match parse_jetbrains_quota(&raw) {
                Some(q) => {
                    d.quota = Some(q);
                    return d;
                }
                None => d.why = format!("no quota in {}'s AIAssistantQuotaManager2.xml", d.ide),
            },
            Err(e) => d.why = format!("could not read {}: {}", path, e),
        }
        first.get_or_insert(Data { skipped: 0, ..d });
    }
    first.unwrap_or(Data {
        ides: found,
        ..Data::default()
    })
}

// Old either way: the IDE has not written for a while, or it wrote before a
// refill or the licence's end that has since come due, so the figure belongs
// to a closed period; or another file could be newer and cannot be ranked.
fn stale(d: &Data) -> bool {
    // A file whose time could not be read cannot vouch for being recent.
    let written_long_ago = !written_known(d) || now() - d.written > FRESH_SECS;
    let refill_passed = d
        .quota
        .as_ref()
        .and_then(|q| q.refill)
        .is_some_and(|at| at <= now());
    // An entitlement that has ended leaves a percentage of nothing current.
    let licence_ended = d
        .quota
        .as_ref()
        .and_then(|q| q.until)
        .is_some_and(|at| at <= now());
    written_long_ago || refill_passed || licence_ended || d.undated || !d.unlisted.is_empty()
}

/// Whether the file's time can be trusted as an age: unread (0) and ahead
/// of this clock, after a clock correction or a copy that kept its
/// timestamp, both vouch for nothing.
fn written_known(d: &Data) -> bool {
    d.written > 0.0 && d.written <= now()
}

/// A refill period as a reader would say it: in the largest unit it is a
/// whole number of, so a twelve-hour period is not "0 days". A period that
/// is none of those, such as `P1DT12H30M` or `PT90S`, is given exactly in
/// parts rather than rounded to a cadence the pace bar is not using.
fn period_label(secs: f64) -> String {
    let plural = |n: i64, unit: &str| format!("{} {}{}", n, unit, if n == 1 { "" } else { "s" });
    // Milliseconds, so the fractional seconds the parser accepts survive.
    let ms = (secs * 1000.0).round().max(1.0) as i64;
    // A smaller unit is named alone only below the next one up, so 36.5
    // hours is not "2190 minutes".
    let whole = [
        (86_400_000, "day", i64::MAX),
        (3_600_000, "hour", i64::MAX),
        (60_000, "minute", 3_600_000),
        (1000, "second", 60_000),
    ];
    for (size, unit, below) in whole {
        if ms % size == 0 && ms < below {
            return plural(ms / size, unit);
        }
    }
    let (d, h, m) = (ms / 86_400_000, ms % 86_400_000 / 3_600_000, ms % 3_600_000 / 60_000);
    let mut parts: Vec<String> = [(d, "d"), (h, "h"), (m, "m")]
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|(n, u)| format!("{n}{u}"))
        .collect();
    let rest = ms % 60_000;
    if rest > 0 {
        parts.push(format!("{}s", rest as f64 / 1000.0));
    }
    parts.join(" ")
}

/// A credit count: JetBrains counts in fractions, so below a thousand the
/// value is shown as it is rather than cut to a whole number that disagrees
/// with the percentage beside it.
fn credits(n: f64) -> String {
    if n.abs() < 1000.0 {
        format!("{}", (n * 100.0).round() / 100.0)
    } else {
        big_num(n)
    }
}

pub fn why_no_lane(d: &Data) -> String {
    if !lanes(d).is_empty() {
        return String::new();
    }
    if !d.why.is_empty() {
        return format!("no quota · {}", d.why);
    }
    if d.quota.is_some() {
        return "no quota · the IDE recorded a quota with no maximum to measure against.".into();
    }
    "no quota · no JetBrains IDE on this machine has recorded an AI Assistant quota.".into()
}

/// The one credit pool, for the summary screen.
pub fn lanes(d: &Data) -> Vec<Lane> {
    let Some(q) = d.quota.as_ref() else {
        return Vec::new();
    };
    let Some(pct) = q.used_pct() else {
        return Vec::new();
    };
    vec![Lane {
        label: "credits".into(),
        pct,
        window_secs: q.refill_secs,
        reset: q.refill,
        stale: stale(d),
        projected: false,
        apart: false,
    }]
}

fn quota_rows(d: &Data, q: &JetBrainsQuota, w: usize, p: &Palette) -> Vec<String> {
    // The IDE is named under SUBSCRIPTION, so the header keeps only what
    // decides how far to trust the figure, and adds the scope when it fits
    // rather than clipping the age off a narrow pane.
    let age = if written_known(d) {
        format!("recorded {} ago", ago(d.written))
    } else {
        "age unknown".to_string()
    };
    let scope = " · account-wide";
    let fits = 13 + age.chars().count() + scope.chars().count() <= w - 1;
    let mut rows = vec![tc::seg(
        &[
            (p.lbl.as_str(), " ── QUOTA ── ".into()),
            (if stale(d) { p.warn.as_str() } else { p.ok.as_str() }, age),
            (p.dim.as_str(), if fits { scope.into() } else { String::new() }),
        ],
        w - 1,
    )];
    let mut when = String::new();
    if let Some(at) = q.refill {
        let left = at - now();
        when = if left > 0.0 {
            format!("refills in {}", left_span(left))
        } else {
            // The IDE has not looked since the refill was due, so the
            // figure below belongs to the period that just closed.
            "refill due · the IDE has not looked since".into()
        };
    }
    let period = q.refill_secs.map(period_label);
    if period.is_some() || !when.is_empty() {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  window ".into()),
                (p.txt.as_str(), period.unwrap_or_else(|| "—".into())),
                (p.dim.as_str(), format!(" · {}", when)),
            ],
            w - 1,
        ));
    }
    let Some(pct) = q.used_pct() else {
        rows.extend(no_local(
            "The IDE recorded a quota with no maximum, so there is nothing to measure it against.",
            "",
            w,
            p,
        ));
        return rows;
    };
    let label = tc::pad("credits", 9);
    let room = ((w as i64) - 38 - 9).max(8) as usize;
    let hue = agent_hue("jetbrains");
    let mut line: Vec<(String, String)> = vec![(p.dim.clone(), format!(" {} ", label))];
    line.extend(paced_bar(
        (pct / 100.0).clamp(0.0, 1.0),
        elapsed_of(q.refill_secs, q.refill),
        room,
        hue,
        p,
    ));
    line.push((pct_colour(pct, hue, p), pct_text(pct)));
    line.push(pace_cell(lead(pct, q.refill_secs, q.refill), p));
    let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
    rows.push(tc::seg(&refs, w - 1));
    rows.push(tc::seg(
        &[(
            p.dim.as_str(),
            format!(
                "  {} of {} used · {} left",
                credits(q.used),
                credits(q.maximum),
                credits(q.available)
            ),
        )],
        w - 1,
    ));
    rows
}

fn plan(d: &Data, q: &JetBrainsQuota, w: usize, p: &Palette) -> Vec<String> {
    let mut pairs: Vec<(String, String)> = vec![("ide".into(), d.ide.clone())];
    // The file's own word for the quota's state, such as `Available`. It is
    // not a plan name, so it is not the headline.
    if !q.kind.is_empty() {
        pairs.push(("quota status".into(), q.kind.clone()));
    }
    if d.ides > 1 || !d.unlisted.is_empty() {
        // Neither another file's missing time nor this one's lets the file
        // shown be ranked, nor does a folder that could not be looked in,
        // so in each case it is only one of them.
        let which = if d.undated || !written_known(d) || !d.unlisted.is_empty() {
            "one"
        } else if d.skipped > 0 {
            "newest readable"
        } else {
            "newest"
        };
        let rest = if d.undated { " · another is undated" } else { "" };
        pairs.push(("read from".into(), format!("{} of {} IDEs{}", which, d.ides, rest)));
    }
    for (root, e) in &d.unlisted {
        pairs.push(("not listed".into(), format!("{} · {}", root, e)));
    }
    if let Some(until) = q.until {
        let day = chrono::Local
            .timestamp_opt(until as i64, 0)
            .single()
            .map(|t| t.format("%-d %b %Y").to_string())
            .unwrap_or_default();
        if !day.is_empty() {
            pairs.push(("licence until".into(), day));
        }
    }
    if let (Some(amount), Some(secs)) = (q.refill_amount, q.refill_secs) {
        pairs.push((
            "refill".into(),
            format!("{} every {}", credits(amount), period_label(secs)),
        ));
    }
    // The quota file names no plan, and a status in the plan's place read as
    // one; the headline says it is not known rather than guessing.
    plan_rows("", &pairs, w, "not in the quota file", None, "", p)
}

pub fn tab(d: &Data, w: usize, _h: usize, _cfg: &Config, p: &Palette) -> Vec<String> {
    let Some(q) = d.quota.as_ref() else {
        let what = if d.why.is_empty() {
            "No JetBrains IDE on this machine has recorded an AI Assistant quota. \
             The IDE writes one once AI Assistant is signed in and has been used."
                .to_string()
        } else {
            d.why.clone()
        };
        return no_local(&what, "", w, p);
    };
    add_section(quota_rows(d, q, w, p), plan(d, q, w, p))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row as a reader sees it, with the colour codes taken out.
    fn strip(s: &str) -> String {
        let mut out = String::new();
        let mut chars = s.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn config_directories_name_the_ide_and_its_version() {
        assert_eq!(ide_of("RustRover2026.2").as_deref(), Some("RustRover 2026.2"));
        assert_eq!(ide_of("IntelliJIdea2026.1").as_deref(), Some("IntelliJ IDEA 2026.1"));
        assert_eq!(ide_of("AndroidStudio2025.1").as_deref(), Some("Android Studio 2025.1"));
        assert_eq!(ide_of("PyCharmCE2024.1").as_deref(), Some("PyCharm CE 2024.1"));
        assert_eq!(
            ide_of("AndroidStudioPreview2025.2").as_deref(),
            Some("Android Studio Preview 2025.2")
        );
        // Directories beside them that are not IDEs, including ones that
        // start with a product name.
        assert_eq!(ide_of("RustRoverBackup"), None);
        assert_eq!(ide_of("RustRover"), None);
        assert_eq!(ide_of("PyCharm2026"), None);
        assert_eq!(ide_of("consentOptions"), None);
        assert_eq!(ide_of("Toolbox"), None);
    }

    #[test]
    fn a_reading_becomes_one_lane_on_the_refill_cycle() {
        let d = Data {
            quota: Some(JetBrainsQuota {
                used: 250.0,
                maximum: 1000.0,
                refill: Some(now() + 86400.0),
                refill_secs: Some(30.0 * 86400.0),
                ..Default::default()
            }),
            written: now(),
            ..Data::default()
        };
        let got = lanes(&d);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].pct, 25.0);
        assert_eq!(got[0].window_secs, Some(30.0 * 86400.0));
        assert!(!got[0].stale);
        assert!(why_no_lane(&d).is_empty());
    }

    #[test]
    fn an_old_file_is_marked_as_cached() {
        // A figure nobody refreshed for a day reads as current otherwise.
        let d = Data {
            quota: Some(JetBrainsQuota { used: 1.0, maximum: 10.0, ..Default::default() }),
            written: now() - 86400.0,
            ..Data::default()
        };
        assert!(lanes(&d)[0].stale);
    }

    #[test]
    fn a_reading_taken_before_a_refill_that_has_passed_is_marked_as_cached() {
        // Written a minute ago, but the refill came due since: the percentage
        // belongs to the period that closed.
        let d = Data {
            quota: Some(JetBrainsQuota {
                used: 9.0,
                maximum: 10.0,
                refill: Some(now() - 30.0),
                ..Default::default()
            }),
            written: now() - 60.0,
            ..Data::default()
        };
        assert!(lanes(&d)[0].stale);
    }

    #[test]
    fn a_newer_file_with_nothing_readable_does_not_hide_an_older_one() {
        let dir = std::env::temp_dir().join(format!("opscope-jb-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let empty = dir.join("new.xml");
        let good = dir.join("old.xml");
        std::fs::write(&empty, "<application/>").unwrap();
        std::fs::write(
            &good,
            "<application><component name=\"AIAssistantQuotaManager2\">\
             <option name=\"quotaInfo\" value=\"{&quot;current&quot;:&quot;5&quot;,\
             &quot;maximum&quot;:&quot;10&quot;}\" /></component></application>",
        )
        .unwrap();
        let path = |p: &std::path::Path| p.to_string_lossy().to_string();
        let d = pick(vec![
            ("RustRover 2026.2".into(), path(&good), 100.0),
            ("PyCharm 2026.2".into(), path(&empty), 200.0),
        ]);
        assert_eq!(d.ide, "RustRover 2026.2");
        assert_eq!(d.quota.as_ref().and_then(|q| q.used_pct()), Some(50.0));
        assert_eq!((d.ides, d.skipped), (2, 1));
        // A readable file dated in the future does not outrank one whose
        // time is good: it cannot be ranked, so the good one is shown and
        // no longer claims to be the newest.
        let d = pick(vec![
            ("RustRover 2026.2".into(), path(&good), now() - 60.0),
            ("PyCharm 2026.2".into(), path(&good), now() + 3600.0),
        ]);
        assert_eq!(d.ide, "RustRover 2026.2");
        assert!(d.undated && stale(&d));
        // With nothing readable anywhere, the newest file's reason is shown.
        let d = pick(vec![("PyCharm 2026.2".into(), path(&empty), 200.0)]);
        std::fs::remove_dir_all(&dir).ok();
        assert!(d.quota.is_none());
        assert!(d.why.contains("PyCharm"), "{}", d.why);
    }

    #[test]
    fn a_file_whose_time_is_unknown_is_cached_and_says_so() {
        // Otherwise it reads as fresh and the header says "recorded never ago".
        let d = Data {
            quota: Some(JetBrainsQuota { used: 1.0, maximum: 10.0, ..Default::default() }),
            written: 0.0,
            ..Data::default()
        };
        assert!(lanes(&d)[0].stale);
        let rows = tab(&d, 60, 20, &Config::default(), &palette()).join("\n");
        assert!(rows.contains("age unknown"), "{rows}");
    }

    #[test]
    fn xdg_config_home_is_searched_first_and_never_twice() {
        let moved = roots_under(Some("/srv/cfg/"));
        assert_eq!(&moved[..2], ["/srv/cfg/JetBrains", "/srv/cfg/Google"]);
        assert!(moved.contains(&under_home(".config/JetBrains")));
        // Pointing it at the default adds nothing to read twice.
        let default = under_home(".config");
        assert_eq!(roots_under(Some(&default)).len(), roots_under(None).len());
        // A relative value is not a valid XDG_CONFIG_HOME.
        assert_eq!(roots_under(Some("cfg")), roots_under(None));
    }

    #[test]
    fn a_period_under_a_day_is_not_zero_days() {
        assert_eq!(period_label(30.0 * 86400.0), "30 days");
        assert_eq!(period_label(86400.0), "1 day");
        assert_eq!(period_label(12.0 * 3600.0), "12 hours");
        assert_eq!(period_label(1800.0), "30 minutes");
        // Not a whole number of any unit: exact, never rounded to "2 days".
        assert_eq!(period_label(131_400.0), "1d 12h 30m");
        assert_eq!(period_label(36.0 * 3600.0), "36 hours");
        assert_eq!(period_label(5400.0), "1h 30m");
        // Seconds, whole and fractional, which the duration parser accepts.
        assert_eq!(period_label(90.0), "1m 30s");
        assert_eq!(period_label(45.0), "45 seconds");
        assert_eq!(period_label(1.5), "1.5s");
    }

    #[test]
    fn fractional_credits_are_not_cut_to_whole_numbers() {
        assert_eq!(credits(0.9), "0.9");
        assert_eq!(credits(99.1), "99.1");
        assert_eq!(credits(100.0), "100");
        assert_eq!(credits(992_521.7), "992.5k");
    }

    #[test]
    fn an_ended_licence_or_an_undated_rival_is_not_fresh() {
        let q = JetBrainsQuota { used: 1.0, maximum: 10.0, ..Default::default() };
        let ended = Data {
            quota: Some(JetBrainsQuota { until: Some(now() - 60.0), ..q.clone() }),
            written: now() - 120.0,
            ..Default::default()
        };
        assert!(stale(&ended));
        let fresh = Data { quota: Some(q), written: now() - 120.0, ..Default::default() };
        assert!(!stale(&fresh));
        // An undated file among several cannot be ranked, so the one shown
        // cannot say it is the newest.
        let rival = Data { undated: true, ides: 2, ..fresh };
        assert!(stale(&rival));
        let all = tab(&rival, 100, 30, &Config::default(), &palette()).join(" ");
        let words = all.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("one of 2 IDEs · another is undated"), "{words}");
        // The file shown can be the undated one, after a dated file above it
        // held nothing readable: it is not "newest readable" either.
        let own = Data { written: 0.0, skipped: 1, undated: false, ..rival };
        let all = tab(&own, 100, 30, &Config::default(), &palette()).join(" ");
        let words = all.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("one of 2 IDEs"), "{words}");
        assert!(!words.contains("newest"), "{words}");
    }

    #[test]
    fn a_config_folder_that_cannot_be_listed_is_said_rather_than_skipped() {
        // Treated as missing, it left the pane saying no IDE had recorded a
        // quota, or calling an older file the newest, when the folder it
        // could not look in might hold the newest one.
        let refused = vec![("/cfg/JetBrains".to_string(), "Permission denied".to_string())];
        let d = with_unlisted(pick(Vec::new()), refused.clone());
        assert!(d.why.contains("could not list /cfg/JetBrains: Permission denied"), "{}", d.why);
        let all = tab(&d, 100, 30, &Config::default(), &palette()).join(" ");
        assert!(!all.contains("No JetBrains IDE"), "{all}");
        let q = JetBrainsQuota { used: 1.0, maximum: 10.0, ..Default::default() };
        let one = Data {
            quota: Some(q),
            ide: "RustRover 2026.2".into(),
            written: now() - 120.0,
            ides: 1,
            ..Default::default()
        };
        assert!(!stale(&one));
        let one = with_unlisted(one, refused);
        assert!(stale(&one));
        let all = strip(&tab(&one, 100, 30, &Config::default(), &palette()).join(" "));
        let words = all.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(words.contains("one of 1 IDEs"), "{words}");
        assert!(words.contains("not listed /cfg/JetBrains · Permission denied"), "{words}");
    }

    #[test]
    fn a_file_dated_in_the_future_is_not_taken_as_fresh() {
        // A clock correction or a copied timestamp would otherwise read as
        // "recorded -300s ago" and be drawn as current.
        let d = Data {
            quota: Some(JetBrainsQuota { used: 1.0, maximum: 10.0, ..Default::default() }),
            written: now() + 300.0,
            ..Default::default()
        };
        assert!(stale(&d));
        let rows = tab(&d, 60, 20, &Config::default(), &palette()).join("\n");
        assert!(rows.contains("age unknown") && !rows.contains("ago"), "{rows}");
    }

    #[test]
    fn no_file_says_so_rather_than_drawing_nothing() {
        let d = Data::default();
        assert!(lanes(&d).is_empty());
        assert!(why_no_lane(&d).contains("no JetBrains IDE"));
        let rows = tab(&d, 60, 20, &Config::default(), &palette());
        assert!(!rows.is_empty());
    }

    #[test]
    fn a_reading_draws_its_bar_the_refill_and_the_ide_within_the_pane() {
        let d = Data {
            quota: Some(JetBrainsQuota {
                kind: "Available".into(),
                used: 7478.3,
                maximum: 1_000_000.0,
                available: 992_521.7,
                refill: Some(now() + 10.0 * 86400.0),
                refill_amount: Some(1_000_000.0),
                refill_secs: Some(30.0 * 86400.0),
                until: Some(now() + 200.0 * 86400.0),
            }),
            ide: "RustRover 2026.2".into(),
            written: now() - 300.0,
            ides: 2,
            skipped: 1,
            undated: false,
            unlisted: Vec::new(),
            why: String::new(),
        };
        for w in [40usize, 60, 100] {
            let rows: Vec<String> = tab(&d, w, 30, &Config::default(), &palette())
                .iter()
                .map(|r| strip(r))
                .collect();
            for r in &rows {
                assert!(tc::display_width(r) <= w - 1, "width {w}: {r:?}");
            }
            // The age decides how far to trust the bar, so it is never the
            // part a narrow pane loses.
            assert!(rows[0].contains("recorded 5m ago"), "{:?}", rows[0]);
            let all = rows.join("\n");
            assert!(all.contains("credits"), "{all}");
            assert!(all.contains("7.5k of 1.0M used"), "{all}");
            // A narrow pane wraps this value across rows, so it is read as words.
            let words = all.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(words.contains("newest readable of 2 IDEs"), "{all}");
            // `Available` is the quota's state, not a plan.
            assert!(!words.contains("SUBSCRIPTION ── Available"), "{all}");
            assert!(words.contains("quota status Available"), "{all}");
        }
    }
}
