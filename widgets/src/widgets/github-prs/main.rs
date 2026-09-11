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

//! Every open pull request you can see, and what is holding each one up.
//!
//! A port of pr.py. GitHub's search has no OR, so anything that is a union
//! of conditions is several searches merged; each source remembers which
//! PRs it found, so narrowing to one costs no request.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use chrono::{NaiveDateTime, Utc};
use opscope_core as tc;

/// The environment variable a GitHub token is read from when `token_env`
/// says nothing. Named once so the code and the schema cannot drift: the
/// settings screen draws its default from `settings.json`, and a screen
/// showing one string while the code falls back to another is a screen
/// describing a value nobody uses. `a_declared_token_env_matches_the_code`
/// holds the two together.
const TOKEN_ENV: &str = "GITHUB_TOKEN";

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "github-prs",
    section: "github_prs",
    legacy_section: Some("pr"),
    schema: include_str!("settings.json"),
    catalogues: &[],
};

const API: &str = "https://api.github.com/graphql";
const SORTS: &[&str] = &["updated", "created"];
/// Width of the per-day charts, in days.
const OPENED_DAYS: i64 = 30;

/// The rolling window the two large figures count over.
///
/// Rolling, not a calendar day: at nine in the morning "today" is three
/// hours of evidence and reads as a collapse in throughput. Both labels
/// say `last 24h` so nobody reads it as midnight-to-now.
const COUNT_WINDOW_HOURS: i64 = 24;

/// Whether the shipped searches stood in for an empty `sources`. Set once
/// in main, and only so the pane can say it happened.
static SOURCES_REFILLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// The searches to run: the ones config named, or the three shipped here.
///
/// GitHub search has no OR, so anything that is a union of conditions has to
/// be several searches merged - which is why this is a map rather than one
/// query string.
///
/// **An empty map lands on the defaults, exactly as an absent key does.**
/// The two are not distinguished, and that is deliberate: no sources means
/// no request is sent at all - paging is `while !live.is_empty()` over one
/// entry per source - so an empty map buys a board that cannot say anything
/// about anything. There is no use for that state, and it is one keypress
/// away, because `sources` replaces the shipped three wholesale and the
/// settings screen will delete the last entry.
///
/// The cost is that emptying the map is not a way to search nothing. Nothing
/// wanted to.
fn sources_from(cfg: &serde_json::Value) -> Vec<(String, String)> {
    let named: Vec<(String, String)> = cfg
        .get("sources")
        .and_then(|v| v.as_object())
        .map(|map| {
            map.iter()
                .map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string()))
                .collect()
        })
        .unwrap_or_default();
    if !named.is_empty() {
        return named;
    }
    vec![
        ("orgs".into(), "is:open is:pr @mine".into()),
        ("authored".into(), "is:open is:pr author:@me".into()),
        ("assigned".into(), "is:open is:pr assignee:@me".into()),
    ]
}

/// Whether config asked for `sources` and left nothing in it.
///
/// Only worth knowing so the pane can say the defaults are standing in. A
/// config that reads `{}` while the board searches three things is a silent
/// disagreement between the file and the screen, and this repository does
/// not leave those unsaid.
fn sources_were_emptied(cfg: &serde_json::Value) -> bool {
    cfg.get("sources")
        .and_then(|v| v.as_object())
        .map(|map| map.is_empty())
        .unwrap_or(false)
}

/// The GitHub token from this widget's own section, and nowhere else.
///
/// It used to fall back to `github.token` - convenient while the two hold
/// the same string, wrong as soon as they differ, and invisible either way,
/// because the settings screen only ever shows this widget's own value.
fn token(pr_cfg: &serde_json::Value) -> (String, &'static str) {
    let value = tc::cfg_str(pr_cfg, "token", "");
    if !value.is_empty() {
        return (value, "config");
    }
    let name = tc::cfg_str(pr_cfg, "token_env", TOKEN_ENV);
    let name = if name.is_empty() { TOKEN_ENV.into() } else { name };
    match std::env::var(&name) {
        Ok(value) if !value.is_empty() => (value, "env"),
        _ => (String::new(), "missing"),
    }
}

#[derive(Clone, Copy, Default)]
struct Rate {
    /// Remaining and the ceiling it is measured against. A bare remaining
    /// is not a reading: 4737 is reassuring against 5000 and alarming
    /// against 5000000, and the header showed the first number without the
    /// second while `github` and `gha` beside it showed both.
    remaining: Option<i64>,
    limit: Option<i64>,
}

/// What to say when the transport refused, rather than what it said.
///
/// A gateway that gives up answers with an HTML page, and `post_json` hands
/// that back whole: `HTTP 502: <html> <head><title>502 Bad Gateway ...`.
/// Every other error this widget draws is a sentence, and a pane is no
/// place for markup - the banner wrapped the nginx body across four rows
/// and said nothing a reader could act on. The status is the part that
/// carries meaning, so keep it and drop the document.
fn plain_refusal(said: &str) -> String {
    let code = said
        .strip_prefix("HTTP ")
        .and_then(|rest| rest.split(':').next())
        .and_then(|c| c.trim().parse::<u16>().ok());
    match code {
        // GitHub's search backend goes through slow spells and sheds the
        // heaviest queries first; the widget is not broken and neither is
        // the token, so the wording says which end gave up.
        Some(c @ (502 | 503 | 504)) => {
            format!("GitHub returned {} - the search was too slow to serve", c)
        }
        Some(c) => match short_explain(said, c) {
            Some(why) => format!("GitHub returned {}: {}", c, why),
            None => format!("GitHub returned {}", c),
        },
        // Not a status at all: a timeout or a curl failure, already a
        // sentence, and short enough to draw.
        None => said.chars().take(100).collect(),
    }
}

/// A non-gateway body's useful words, when it has any.
///
/// Gateway HTML is dropped above. A 401, 403 or 429 usually carries the
/// reason in JSON (`Bad credentials`, a missing scope, the rate-limit
/// message) and that is the sentence the pane should keep. Markup, or an
/// empty body, leaves only the status.
fn short_explain(said: &str, code: u16) -> Option<String> {
    let rest = said
        .strip_prefix("HTTP ")
        .and_then(|s| s.strip_prefix(&code.to_string()))
        .unwrap_or("")
        .trim_start_matches(':')
        .trim();
    if rest.is_empty() || rest.starts_with('<') {
        return None;
    }
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(rest) {
        if let Some(m) = v.get("message").and_then(|m| m.as_str()) {
            let m = m.trim();
            if !m.is_empty() {
                return Some(m.chars().take(80).collect());
            }
        }
    }
    Some(rest.chars().take(80).collect())
}

fn graphql(
    query: &str,
    tok: &str,
    variables: serde_json::Value,
) -> Result<serde_json::Value, String> {
    let body = serde_json::json!({ "query": query, "variables": variables }).to_string();
    let (out, _headers) = tc::post_json(
        API,
        &[
            ("Authorization", &format!("Bearer {}", tok)),
            ("Content-Type", "application/json"),
            ("User-Agent", "opscope"),
        ],
        &body,
        45,
    )
    .map_err(|said| plain_refusal(&said))?;
    let data: serde_json::Value = serde_json::from_str(&out).map_err(|e| e.to_string())?;
    if let Some(first) = data["errors"].as_array().and_then(|a| a.first()) {
        return Err(first["message"]
            .as_str()
            .unwrap_or("")
            .chars()
            .take(100)
            .collect());
    }
    Ok(data["data"].clone())
}

/// What a search can hand over at any depth.
///
/// Everything here is a plain field on the pull request. Measured against
/// the live API: with these alone a search of 665 pages out in full, in
/// fourteen rounds, and never refuses.
const PR_FIELDS: &str = "
      id number title url isDraft createdAt updatedAt
      additions deletions changedFiles
      author { login }
      repository { nameWithOwner }
      headRefName baseRefName reviewDecision mergeable";

/// The two fields a search cannot page deeply with, fetched by node id.
///
/// `stackEntry` and the check rollup are subqueries per result, and asking
/// for them inside a search is what stops GitHub serving it: measured,
/// pages one to four answer and page five returns 502, whether the search
/// is one query over ten owners or split into one query each. Dropping
/// them removes the ceiling entirely, and asking for them afterwards by
/// node id costs one request per fifty results and answers every time -
/// tested at 25, 50 and 100 ids.
const PR_HEAVY_FIELDS: &str = "
      id
      stackEntry { position stack { number size } }
      commits(last: 1) { nodes { commit { statusCheckRollup { state } } } }";

/// Fill in the fields a search would not serve, for results already held.
///
/// Fifty at a time, matching the page that produced them. A failure here
/// is not fatal and is not reported: the rows are already on screen and
/// correct, they simply show their checks as unknown, which is what an
/// unknown check should look like. Inventing "passing" because a lookup
/// failed is the one outcome that would be worse than a dash.
fn enrich(pool: &mut HashMap<String, serde_json::Value>, by_id: &HashMap<String, String>, tok: &str) {
    let ids: Vec<String> = by_id.keys().cloned().collect();
    for chunk in ids.chunks(50) {
        let query = format!(
            "query($ids: [ID!]!) {{ nodes(ids: $ids) {{ ... on PullRequest {{ {} }} }} }}",
            PR_HEAVY_FIELDS
        );
        let Ok(d) = graphql(&query, tok, serde_json::json!({ "ids": chunk })) else {
            // A failed chunk must not look like "no CI". ready_to_merge
            // treats a missing rollup as a repo with nothing to wait for,
            // and that would count approved PRs as ready while their
            // checks were never read.
            for id in chunk {
                let Some(url) = by_id.get(id) else { continue };
                let Some(entry) = pool.get_mut(url) else { continue };
                entry["checksUnknown"] = serde_json::json!(true);
            }
            continue;
        };
        for node in d["nodes"].as_array().into_iter().flatten() {
            let id = text(node, "id");
            let Some(url) = by_id.get(&id) else { continue };
            let Some(entry) = pool.get_mut(url) else { continue };
            entry["stackEntry"] = node["stackEntry"].clone();
            entry["commits"] = node["commits"].clone();
        }
    }
}

/// One request, one aliased search per source.
///
/// The ceiling is on result nodes rather than field complexity: three
/// searches of 100 return HTTP 502 with or without the check rollup, three
/// of 50 do not. So the page size is per source and deliberately modest.
/// `cursors` carries one `after` per source, so a round asks each source
/// only for the page it has not had yet. A source that is finished is left
/// out of the round entirely rather than re-fetching its last page: the
/// 502 ceiling above is on nodes per request, so dropping finished sources
/// is what keeps a late round cheap.
fn list_query(queries: &[String], limit: usize, cursors: &[Option<String>]) -> String {
    let mut parts = vec!["rateLimit { remaining limit }".to_string()];
    for (i, q) in queries.iter().enumerate() {
        let after = match cursors.get(i).and_then(|c| c.clone()) {
            Some(c) => format!(", after: {}", serde_json::Value::String(c)),
            None => String::new(),
        };
        parts.push(format!(
            "s{}: search(query: {}, type: ISSUE, first: {}{}) {{ issueCount pageInfo {{ hasNextPage endCursor }} nodes {{ ... on PullRequest {{ {} }} }} }}",
            i,
            serde_json::Value::String(q.clone()),
            limit,
            after,
            PR_FIELDS
        ));
    }
    format!("{{ {} }}", parts.join(" "))
}

const DETAIL_QUERY: &str = r#"
query($owner: String!, $name: String!, $number: Int!) {
  repository(owner: $owner, name: $name) {
    pullRequest(number: $number) {
      number title url state isDraft createdAt updatedAt
      additions deletions changedFiles
      author { login } headRefName baseRefName
      mergeable mergeStateStatus reviewDecision
      commitCount: commits { totalCount }
      stack { number size baseRefName
        entries(first: 40) { nodes { position pullRequest {
          number title isDraft reviewDecision mergeable
          additions deletions author { login } headRefName } } } }
      reviewThreads(first: 60) { nodes { isResolved } }
      reviews(last: 20) { nodes { author { login } state submittedAt } }
      reviewRequests(first: 12) { nodes { requestedReviewer {
        ... on User { login } ... on Team { name } } } }
      commits(last: 1) { nodes { commit { statusCheckRollup {
        state contexts(first: 25) { totalCount nodes {
          ... on CheckRun { name conclusion status startedAt completedAt }
          ... on StatusContext { context state } } } } } } }
    }
  }
}"#;

/// Every open PR in one repository, for reconstructing a stack that was not
/// made with `gh stack` - the API's own stack field is authoritative when
/// it is there, and null everywhere else.
///
/// Paged, and it has to be. A stack is inferred by matching each PR's base
/// branch against the head branches of the others, so a PR whose parent did
/// not arrive is read as the root of its own stack. That is not a short
/// answer, it is a wrong one: the drawn tree is missing a level and nothing
/// says so. A hundred was enough until a repo with dependabot on it wasn't,
/// and this repo's own account has several over that.
const REPO_PRS_QUERY: &str = r#"
query($owner: String!, $name: String!, $after: String) {
  repository(owner: $owner, name: $name) {
    pullRequests(states: OPEN, first: 100, after: $after) {
      pageInfo { hasNextPage endCursor }
      nodes { number title isDraft headRefName baseRefName
              additions deletions author { login }
              reviewDecision mergeable }
    }
  }
}"#;

/// Whether this run is reading the pre-rename `pr` section.
///
/// The new name wins wherever it is set, so a config that has been updated
/// is never second-guessed by an old section somebody forgot to delete. A
/// config that has *not* been updated is read anyway rather than falling
/// through to code defaults, which on screen is indistinguishable from a
/// widget nobody configured.
fn on_legacy_section() -> bool {
    !tc::config_has_section("github_prs") && tc::config_has_section("pr")
}

/// And said out loud, because a silent fallback is how a rename becomes
/// permanent: nothing ever tells anyone the old name is still doing the
/// work.
fn legacy_section_note() -> Option<String> {
    on_legacy_section()
        .then(|| "config: reading the old `pr` section — rename it to `github_prs`".into())
}

/// Said when the file and the screen would otherwise disagree in silence.
///
/// `"sources": {}` searches the three shipped here, which is the useful
/// behaviour and not the obvious reading of the file. Somebody who emptied
/// that map on purpose is owed the reason their board is not empty.
fn refilled_sources_note() -> Option<String> {
    SOURCES_REFILLED
        .get()
        .copied()
        .unwrap_or(false)
        .then(|| "config: `sources` is empty — using the three shipped searches".into())
}

fn text(value: &serde_json::Value, key: &str) -> String {
    value[key].as_str().unwrap_or("").to_string()
}

fn number(value: &serde_json::Value, key: &str) -> i64 {
    value[key].as_i64().unwrap_or(0)
}

fn parse(ts: &str) -> Option<NaiveDateTime> {
    if ts.len() < 19 {
        return None;
    }
    NaiveDateTime::parse_from_str(&ts[..19], "%Y-%m-%dT%H:%M:%S").ok()
}

fn hours_since(ts: &str) -> Option<f64> {
    let when = parse(ts)?;
    Some((Utc::now().naive_utc() - when).num_seconds() as f64 / 3600.0)
}

fn ago(ts: &str) -> String {
    let Some(hours) = hours_since(ts) else {
        return "--".into();
    };
    let s = hours * 3600.0;
    if s < 3600.0 {
        format!("{}m", ((s / 60.0) as i64).max(1))
    } else if s < 86400.0 {
        format!("{}h", (s / 3600.0) as i64)
    } else if s < 86400.0 * 365.0 {
        format!("{}d", (s / 86400.0) as i64)
    } else {
        format!("{:.1}y", s / (86400.0 * 365.0))
    }
}

fn span(hours: Option<f64>) -> String {
    let Some(h) = hours else {
        return "--".into();
    };
    if h < 48.0 {
        return format!("{}h", h as i64);
    }
    let days = h / 24.0;
    if days < 365.0 {
        format!("{}d", days as i64)
    } else {
        format!("{:.1}y", days / 365.0)
    }
}

fn rollup(pr: &serde_json::Value) -> String {
    pr["commits"]["nodes"]
        .as_array()
        .and_then(|a| a.first())
        .map(|n| text(&n["commit"]["statusCheckRollup"], "state"))
        .unwrap_or_default()
}

/// Approved, green, no conflict, not a draft - the actionable count.
///
/// Everything else on this board describes work in flight; this is the one
/// number that says something can be done right now.
fn ready_to_merge(pr: &serde_json::Value) -> bool {
    if pr["checksUnknown"].as_bool().unwrap_or(false) {
        return false;
    }
    let checks = rollup(pr);
    text(pr, "reviewDecision") == "APPROVED"
        && (checks == "SUCCESS" || checks.is_empty())
        && text(pr, "mergeable") != "CONFLICTING"
        && !pr["isDraft"].as_bool().unwrap_or(false)
}

/// The chain this PR belongs to, reconstructed from branch names.
///
/// A PR whose base branch is another open PR's head branch is sitting on
/// top of it. That inference produces a tree rather than a line, so each PR
/// keeps its list of children.
type Chain = (Option<i64>, HashMap<i64, i64>, HashMap<i64, Vec<i64>>);

fn stack_of(number: i64, repo_prs: &[serde_json::Value]) -> Chain {
    let mut by_head: HashMap<String, i64> = HashMap::new();
    for other in repo_prs {
        by_head.insert(text(other, "headRefName"), self::number(other, "number"));
    }
    let mut parent: HashMap<i64, i64> = HashMap::new();
    let mut kids: HashMap<i64, Vec<i64>> = HashMap::new();
    for other in repo_prs {
        let mine = self::number(other, "number");
        if let Some(up) = by_head.get(&text(other, "baseRefName")) {
            if *up != mine {
                parent.insert(mine, *up);
                kids.entry(*up).or_default().push(mine);
            }
        }
    }
    if !parent.contains_key(&number) && !kids.contains_key(&number) {
        return (None, HashMap::new(), HashMap::new());
    }
    let mut root = number;
    let mut seen: HashSet<i64> = HashSet::new();
    while let Some(up) = parent.get(&root) {
        if !seen.insert(root) {
            break;
        }
        root = *up;
    }
    (Some(root), parent, kids)
}

/// A row of the stack map: connector, the PR, whether it is the open one,
/// and its position when GitHub gave one.
type StackRow = (String, serde_json::Value, bool, Option<i64>);

/// What the open is actually doing, so the wait can show real work.
#[derive(Clone)]

#[derive(Default)]
struct State {
    viewer: String,
    orgs: Vec<String>,
    prs: Vec<serde_json::Value>,
    total: usize,
    /// Set when a source filled its page, so `total` is a floor and every
    /// count drawn from `prs` describes what was fetched rather than what
    /// is open.
    capped: bool,
    query: String,
    detail: Option<serde_json::Value>,
    stack_rows: Vec<StackRow>,
    want: Option<(String, String, i64)>,
    loading: bool,
    target: String,
    stages: tc::Progress,
    err: String,
    fetched: f64,
    /// Merge activity, which no amount of reading `prs` can produce.
    ///
    /// `None` until the count request has answered once. That is not the
    /// same as every figure being zero, and the pane has to draw the two
    /// differently: a board with nothing merged today is a real reading.
    counts: Option<Counts>,
    /// Why the last count request failed, while the counts above are
    /// whatever the last good one left. Kept apart from `err` so a count
    /// GitHub would not serve cannot make the list look broken.
    counts_err: String,
    /// The page size the last pass was served, when a round had to fall
    /// back from the size it asked for. `None` is the ordinary case.
    served: Option<usize>,
    /// Set when the last pass stopped paging before its sources were
    /// exhausted, so the list is every result GitHub served rather than
    /// every result there is.
    ///
    /// This and `served` describe how the pass went, not that it went
    /// wrong, and they are drawn as a dim note under the header. They are
    /// deliberately not in `err`: the `!` banner is for a pass that
    /// produced nothing and for a refusal that really is one.
    stopped: bool,
}

/// Merge and arrival counts over the span the day charts plot.
///
/// The pool behind this board is `is:open` throughout, so nothing merged
/// is ever in hand and nothing here can be counted from it. GitHub answers
/// it instead, one aliased `issueCount` per day in a single request.
#[derive(Clone, Default, Debug)]
struct Counts {
    /// One entry per calendar day, oldest first, carrying the `%Y-%m-%d`
    /// it describes. The chart looks a day up by that string rather than
    /// by position, so a set of counts fetched before midnight cannot
    /// slide every figure one day along once the axis has moved on.
    days: Vec<(String, i64)>,
    merged_24h: i64,
    opened_24h: i64,
    /// When these landed. Carried here rather than beside them in `State`
    /// because it describes this set of figures and no other: a later pass
    /// that fails leaves both untouched together, where two fields could
    /// end up saying the counts are fresher than they are.
    at: f64,
}

impl State {
    /// Kept as a two-argument call so the twenty-odd sites that use it did
    /// not all have to change; the model underneath is now the shared one,
    /// which `gha` also reports through and which can carry counts when a
    /// stage learns how many things it is working over.
    fn stage(&mut self, label: &str, done: bool) {
        if done {
            self.stages.finish(label);
        } else {
            self.stages.begin(label);
        }
    }
}

/// Everywhere `@mine` reaches, as one run of owner qualifiers.
///
/// Repeated qualifiers of the same kind are OR'd by GitHub, so one search
/// covers every org and your own account at once. The per-day counts ask
/// for the same ground as the list, which is only true while both build
/// the scope here rather than each spelling it out.
fn mine_scope(viewer: &str, orgs: &[String]) -> String {
    let mut mine: Vec<String> = orgs.iter().map(|o| format!("org:{}", o)).collect();
    if !viewer.is_empty() {
        mine.push(format!("user:{}", viewer));
    }
    mine.join(" ")
}

/// Each configured source, with `@mine` expanded and args appended.
///
/// Relationships that reach outside the owner list - authored, assigned -
/// need their own source; `@mine` cannot express them.
fn searches(
    sources: &[(String, String)],
    viewer: &str,
    orgs: &[String],
    extra: &[String],
) -> Vec<(String, String)> {
    let mine = mine_scope(viewer, orgs);
    sources
        .iter()
        .map(|(name, q)| {
            let mut parts = vec![q.replace("@mine", &mine)];
            parts.extend(extra.iter().cloned());
            (name.clone(), parts.join(" "))
        })
        .collect()
}

/// The calendar days both charts plot, oldest first.
///
/// UTC, because GitHub reads a bare `merged:YYYY-MM-DD` in UTC and the
/// arrival chart already bins `createdAt` on a UTC date. An axis built in
/// local time would label bins it does not describe.
fn chart_days() -> Vec<String> {
    let today = Utc::now().date_naive();
    (0..OPENED_DAYS)
        .rev()
        .map(|k| {
            (today - chrono::Duration::days(k))
                .format("%Y-%m-%d")
                .to_string()
        })
        .collect()
}

/// Every figure the two day charts need, in one request.
///
/// Aggregates rather than records: `issueCount` costs one request however
/// many aliases ask for it, where counting merged pull requests by paging
/// them would be a hundred round trips for a number. Measured against the
/// live API at thirty-two aliases over ten owners - HTTP 200, and the
/// whole pass including the account walk cost 15 of 5000 points.
///
/// The two rolling windows are full datetimes rather than dates, which
/// GitHub's search accepts and which is the only way to say *the last 24
/// hours* rather than *today so far*. Verified against the live API: the
/// same query at a one-hour cut returned a smaller count than at 24.
fn count_query(scope: &str, days: &[String], since: &str) -> String {
    let mut parts = vec!["{".to_string()];
    for (n, day) in days.iter().enumerate() {
        parts.push(format!(
            "\n  m{n}: search(query:\"{scope} is:pr is:merged merged:{day}\", type:ISSUE) \
             {{ issueCount }}"
        ));
    }
    parts.push(format!(
        "\n  mw: search(query:\"{scope} is:pr is:merged merged:>={since}\", type:ISSUE) \
         {{ issueCount }}\
         \n  cw: search(query:\"{scope} is:pr created:>={since}\", type:ISSUE) \
         {{ issueCount }}\
         \n  rateLimit {{ remaining limit }}\n}}"
    ));
    parts.join("")
}

/// Read the aliases back, refusing to invent a figure for a missing one.
///
/// Deliberately not `unwrap_or(0)`. An alias GitHub did not answer is an
/// unknown, and a zero drawn in its place is a bar chart asserting a quiet
/// day that may have been the busiest of the month - the founding rule of
/// this repo, in the one place where the pane cannot tell.
fn read_counts(d: &serde_json::Value, days: &[String]) -> Result<Counts, String> {
    let one = |alias: &str| -> Result<i64, String> {
        d[alias]["issueCount"]
            .as_i64()
            .ok_or_else(|| format!("GitHub left {} out of the answer", alias))
    };
    let mut out = Counts::default();
    for (n, day) in days.iter().enumerate() {
        out.days.push((day.clone(), one(&format!("m{}", n))?));
    }
    out.merged_24h = one("mw")?;
    out.opened_24h = one("cw")?;
    Ok(out)
}

/// The count request, on its own so a refusal cannot cost the list.
///
/// Runs after `fetch_list` because the owner list `@mine` expands to is
/// discovered there, and reports through `counts_err` rather than `err`
/// for the reason OPS-84 exists: the board's own resilience to GitHub's
/// slow spells must not be undone by a second request beside it.
fn fetch_counts(
    tok: &str,
    days: &[String],
    state: &Arc<Mutex<State>>,
    rate: &Arc<Mutex<Rate>>,
) -> Result<(), String> {
    let (viewer, orgs) = state
        .lock()
        .map(|g| (g.viewer.clone(), g.orgs.clone()))
        .unwrap_or_default();
    let scope = mine_scope(&viewer, &orgs);
    if scope.is_empty() {
        return Err("no account to count over yet".to_string());
    }
    let since = (Utc::now() - chrono::Duration::hours(COUNT_WINDOW_HOURS))
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string();
    let d = graphql(&count_query(&scope, days, &since), tok, serde_json::json!({}))?;
    if let Some(left) = d["rateLimit"]["remaining"].as_i64() {
        if let Ok(mut g) = rate.lock() {
            g.remaining = Some(left);
            if let Some(limit) = d["rateLimit"]["limit"].as_i64() {
                g.limit = Some(limit);
            }
        }
    }
    let mut counts = read_counts(&d, days)?;
    counts.at = tc::now();
    if let Ok(mut g) = state.lock() {
        g.counts = Some(counts);
        g.counts_err = String::new();
    }
    Ok(())
}

fn fetch_detail(
    tok: &str,
    want: &(String, String, i64),
    state: &Arc<Mutex<State>>,
) -> Result<(), String> {
    let (owner, name, num) = want;
    if let Ok(mut g) = state.lock() {
        g.stage("pull request, checks, reviews", false);
    }
    let d = graphql(
        DETAIL_QUERY,
        tok,
        serde_json::json!({ "owner": owner, "name": name, "number": num }),
    )?;
    if let Ok(mut g) = state.lock() {
        g.stage("pull request, checks, reviews", true);
    }
    let pr = d["repository"]["pullRequest"].clone();
    let mut rows: Vec<StackRow> = Vec::new();
    if !pr.is_null() {
        let native = &pr["stack"];
        if !native.is_null() {
            if let Ok(mut g) = state.lock() {
                g.stage("stack, from GitHub", true);
            }
            // GitHub hands the order over directly, position 1 nearest the
            // base. A native stack is a line, not a tree, so it draws flat -
            // eleven levels of indentation would be unreadable and would
            // imply a branching that is not there.
            let mut entries: Vec<serde_json::Value> = native["entries"]["nodes"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            entries.sort_by_key(|e| number(e, "position"));
            let last = entries.len().saturating_sub(1);
            for (i, e) in entries.iter().enumerate() {
                let child = e["pullRequest"].clone();
                let twig = if i == last { "└─ " } else { "├─ " };
                let is_here = number(&child, "number") == *num;
                let position = e["position"].as_i64();
                rows.push((twig.to_string(), child, is_here, position));
            }
        } else {
            if let Ok(mut g) = state.lock() {
                g.stage("stack, from open branches", false);
            }
            let mut others: Vec<serde_json::Value> = Vec::new();
            let mut after: Option<String> = None;
            loop {
                let repo = graphql(
                    REPO_PRS_QUERY,
                    tok,
                    serde_json::json!({ "owner": owner, "name": name, "after": after }),
                )?;
                let conn = &repo["repository"]["pullRequests"];
                others.extend(
                    conn["nodes"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default(),
                );
                if let Ok(mut g) = state.lock() {
                    g.stages.count("stack, from open branches", others.len(), None);
                }
                let next = conn["pageInfo"]["endCursor"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                // A cursor that stops advancing ends the loop rather than
                // spinning it - the same guard the org walk already carries,
                // and the reason it carries it is that this runs on the
                // poller thread where a spin is invisible.
                if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false)
                    || next.is_empty()
                    || Some(&next) == after.as_ref()
                {
                    break;
                }
                after = Some(next);
            }
            if let Ok(mut g) = state.lock() {
                g.stage("stack, from open branches", true);
            }
            let (root, _parent, kids) = stack_of(*num, &others);
            if let Some(root) = root {
                let by_num: HashMap<i64, &serde_json::Value> =
                    others.iter().map(|o| (number(o, "number"), o)).collect();
                // An inferred stack really is a tree - one PR can have two
                // others branched off it - so the connectors are drawn
                // properly rather than indenting by depth alone.
                let mut stack = vec![(root, String::new(), true)];
                while let Some((num_at, prefix, last)) = stack.pop() {
                    if let Some(node) = by_num.get(&num_at) {
                        rows.push((
                            format!("{}{}", prefix, if last { "└─ " } else { "├─ " }),
                            (*node).clone(),
                            num_at == *num,
                            None,
                        ));
                    }
                    let mut children = kids.get(&num_at).cloned().unwrap_or_default();
                    children.sort_unstable();
                    let below = format!("{}{}", prefix, if last { "   " } else { "│  " });
                    for (i, kid) in children.iter().enumerate().rev() {
                        stack.push((*kid, below.clone(), i + 1 == children.len()));
                    }
                }
            }
        }
    }
    if let Ok(mut g) = state.lock() {
        g.detail = if pr.is_null() { None } else { Some(pr) };
        g.stack_rows = rows;
        g.loading = false;
    }
    Ok(())
}

/// The smallest page worth asking for before calling a round a failure.
///
/// Below this the request count climbs faster than the odds of an answer,
/// and a search that will not serve ten per page is not having a slow
/// minute, it is down.
const PAGE_FLOOR: usize = 10;

/// Whether a failed round is worth asking again, smaller.
///
/// Only a gateway giving up, or curl hitting `--max-time`. A GraphQL
/// error - bad credentials, a query GitHub will not accept - says the
/// same thing however small the page is, and retrying it twice only
/// spends rate limit to reprint the message. A 500 is the same: the
/// search backend is not shedding load, it is broken, and a smaller
/// page will not change its mind.
fn worth_retrying(said: &str) -> bool {
    said.starts_with("GitHub returned 502")
        || said.starts_with("GitHub returned 503")
        || said.starts_with("GitHub returned 504")
        || said.contains("curl: (28)")
        || said.contains("curl exited 28")
}

/// One round of paging, backing off the page size when GitHub refuses.
///
/// Measured against the live API during a slow spell: every size from 25
/// up returned 502 at about 10.7s while 20 and below answered in three,
/// and an hour later 50 answered in five. The page size is not what
/// decides it - search was slow across the board and the larger pages were
/// simply the first over whatever budget it was enforcing. So the answer
/// is not a smaller page everywhere, it is a smaller page for as long as
/// GitHub is refusing the big one - which is why the caller feeds the size
/// that worked back in rather than starting each round at the default.
/// Halving twice from there reaches the floor, so a bad minute costs two
/// extra requests once, not once per round.
fn fetch_round(
    round: &[String],
    cursors: &[Option<String>],
    limit: usize,
    tok: &str,
) -> Result<(serde_json::Value, usize), String> {
    let mut size = limit.max(1);
    loop {
        match graphql(&list_query(round, size, cursors), tok, serde_json::json!({})) {
            Ok(d) => return Ok((d, size)),
            Err(said) => match smaller(size).filter(|_| worth_retrying(&said)) {
                Some(next) => size = next,
                None => return Err(said),
            },
        }
    }
}

/// How long to wait before asking a refused round one more time.
///
/// Long enough that whatever shed the request has finished shedding it -
/// measured, page one at ten per page answered in 1.3s through the same
/// spell that refused the round - and short enough that the pass is still
/// the pass a reader is waiting on rather than a new one.
const RETRY_PAUSE: Duration = Duration::from_secs(3);

/// Whether a round that `fetch_round` gave up on is worth one more try.
///
/// `fetch_round` halves the page size within the round and stops at the
/// floor; this is the round itself asked again, once, after a pause. Once,
/// because the thing it is built for is a single bad request in an
/// otherwise answering minute: measured during a slow spell, ten per page
/// answered in 1.3s three times running and one later round came back 502
/// anyway. A second retry would be a loop, and a loop against a search
/// that is actually down spends the rate limit to reprint the message.
///
/// `tried` is how many retries this round has already had, so the answer
/// can only be true on the first. The reason is read as well, because
/// `worth_retrying` already knows which refusals mean "later" and which
/// mean "no": bad credentials say the same thing three seconds from now.
fn retry_again(tried: usize, said: &str) -> bool {
    tried == 0 && worth_retrying(said)
}

/// The next page size to try, or `None` at the floor.
///
/// Its own function so the walk can be tested rather than reimplemented in
/// the test, which is a test of the arithmetic it copied.
fn smaller(size: usize) -> Option<usize> {
    (size > PAGE_FLOOR).then(|| (size / 2).max(PAGE_FLOOR))
}

/// How many open pull requests the pooled sources really cover, and whether
/// that number is a floor rather than a count.
///
/// A source that filled its page has more behind it, and the sources
/// overlap - `orgs` and `authored` find the same PR all day - so the
/// `issueCount`s cannot be added up. Two things are known exactly: the union
/// holds every distinct PR already in hand, and it holds the whole of any
/// single source, so it is no smaller than the largest `issueCount`. The
/// larger of those is the floor the header reports. When nothing filled its
/// page the pool *is* the union and the number is a plain total.
fn union_total(sources: &[(i64, usize)], pooled: usize) -> (usize, bool) {
    let capped = sources.iter().any(|(counted, got)| *counted > *got as i64);
    if !capped {
        return (pooled, false);
    }
    let floor = sources
        .iter()
        .map(|(counted, _)| (*counted).max(0) as usize)
        .fold(pooled, usize::max);
    (floor, true)
}

/// Who you are and which orgs you are in, which is what `@mine` means.
///
/// Its own step rather than the head of `fetch_list`, because the counts
/// need the same answer and need it first: on a board of several hundred
/// pull requests the list pages for minutes, and a figure that only turns
/// up at the end of that is a figure nobody sees.
fn discover_owners(tok: &str, state: &Arc<Mutex<State>>) -> Result<(), String> {
    let need_viewer = state.lock().map(|g| g.viewer.is_empty()).unwrap_or(true);
    if need_viewer {
        // Every org, not the first page of them. `@mine` is built out of
        // this list as owner qualifiers, so an org missing here is not an
        // undercount - it is a scope the board never searched, and nothing
        // downstream can tell that from an org with no open PRs.
        let mut login = String::new();
        let mut orgs: Vec<String> = Vec::new();
        let mut cursor: Option<String> = None;
        loop {
            let who = graphql(
                "query($after: String) { viewer { login \
                 organizations(first: 100, after: $after) { \
                 pageInfo { hasNextPage endCursor } nodes { login } } } }",
                tok,
                serde_json::json!({ "after": cursor }),
            )?;
            if login.is_empty() {
                login = text(&who["viewer"], "login");
            }
            let conn = &who["viewer"]["organizations"];
            orgs.extend(
                conn["nodes"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .map(|o| text(o, "login"))
                    .filter(|o| !o.is_empty()),
            );
            let next = conn["pageInfo"]["endCursor"]
                .as_str()
                .unwrap_or("")
                .to_string();
            // This runs on the poller thread: a cursor that stops advancing
            // has to end the loop rather than spin it.
            if !conn["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false)
                || next.is_empty()
                || Some(&next) == cursor.as_ref()
            {
                break;
            }
            cursor = Some(next);
        }
        if let Ok(mut g) = state.lock() {
            g.viewer = login;
            g.orgs = orgs;
        }
    }
    Ok(())
}

fn fetch_list(
    tok: &str,
    source: &str,
    sources: &[(String, String)],
    extra: &[String],
    limit: usize,
    state: &Arc<Mutex<State>>,
    rate: &Arc<Mutex<Rate>>,
) -> Result<(), String> {
    discover_owners(tok, state)?;
    let (viewer, orgs) = state
        .lock()
        .map(|g| (g.viewer.clone(), g.orgs.clone()))
        .unwrap_or_default();
    let pairs = searches(sources, &viewer, &orgs, extra);
    let queries: Vec<String> = pairs.iter().map(|(_, q)| q.clone()).collect();

    // Pool the sources, remembering which found each PR. The sources
    // overlap, so the union is deduplicated by url and cannot be added up
    // from the per-source counts.
    let mut pool: HashMap<String, serde_json::Value> = HashMap::new();
    let mut order: Vec<String> = Vec::new();
    // What each search says it matched, beside what it has handed over so
    // far. `capped` falls out of these once every source is exhausted.
    let mut counted: Vec<(i64, usize)> = vec![(0, 0); pairs.len()];
    let mut cursors: Vec<Option<String>> = vec![None; pairs.len()];
    let mut live: Vec<usize> = (0..pairs.len()).collect();
    // Why paging stopped early, when it did. Kept apart from `err` so a
    // partial list is not dressed up as a failed fetch.
    let mut deepened: Option<String> = None;
    // The smallest page any round had to fall back to, when one did, and
    // what every later round in this pass starts from. A shorter page is
    // not a smaller answer - paging carries on either way - but going back
    // to the full size each round would spend a refusal and ten seconds
    // relearning the same thing on every one of fifty-odd rounds. It also
    // reaches the banner: a pane that says nothing about it looks like a
    // pane that simply got slower.
    let mut served: Option<usize> = None;

    while !live.is_empty() {
        let round: Vec<String> = live.iter().map(|i| queries[*i].clone()).collect();
        let round_cursors: Vec<Option<String>> =
            live.iter().map(|i| cursors[*i].clone()).collect();
        // A round that fails ends the paging; it does not lose the pass.
        // GitHub's search stops serving these nodes somewhere past the
        // fourth page - measured: pages 1-4 answer, page 5 returns 502,
        // and it is the per-node subqueries that cost it, not the depth.
        // Everything already pooled is real and stays on screen, and
        // `capped` below already says the total is a lower bound.
        // Asked again once, after a pause, before the pass is cut short.
        // The retry starts at the floor rather than at the size the round
        // started from: the walk inside `fetch_round` has just established
        // that the larger pages are being refused, and re-spending that
        // walk costs two ten-second refusals to relearn it.
        let mut tried = 0usize;
        let outcome = loop {
            let start = served.unwrap_or(limit);
            let start = if tried == 0 { start } else { start.min(PAGE_FLOOR) };
            match fetch_round(&round, &round_cursors, start, tok) {
                Ok(got) => break Ok(got),
                Err(said) if retry_again(tried, &said) => {
                    tried += 1;
                    std::thread::sleep(RETRY_PAUSE);
                }
                Err(said) => break Err(said),
            }
        };
        let d = match outcome {
            Ok((d, size)) => {
                if size < limit {
                    served = served.map_or(Some(size), |had: usize| Some(had.min(size)));
                }
                d
            }
            Err(said) if !order.is_empty() => {
                deepened = Some(said);
                break;
            }
            Err(said) => return Err(said),
        };
        if let Some(left) = d["rateLimit"]["remaining"].as_i64() {
            if let Ok(mut g) = rate.lock() {
                g.remaining = Some(left);
                // Only when GitHub sent one. A ceiling remembered from an
                // earlier pass is still true; a zero invented here is not.
                if let Some(limit) = d["rateLimit"]["limit"].as_i64() {
                    g.limit = Some(limit);
                }
            }
        }
        // Only this round's new results need enriching; the ones already
        // pooled were filled in when their own round landed.
        let mut fresh_ids: HashMap<String, String> = HashMap::new();
        let mut next_live: Vec<usize> = Vec::new();
        for (slot, src) in live.iter().enumerate() {
            let (name, _) = &pairs[*src];
            let block = &d[format!("s{}", slot)];
            let got: Vec<&serde_json::Value> = block["nodes"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|n| !n.is_null())
                .collect();
            counted[*src].0 = block["issueCount"].as_i64().unwrap_or(0);
            counted[*src].1 += got.len();
            for n in got {
                let url = text(n, "url");
                let id = text(n, "id");
                if !id.is_empty() {
                    fresh_ids.insert(id, url.clone());
                }
                let entry = pool.entry(url.clone()).or_insert_with(|| {
                    order.push(url.clone());
                    let mut copy = n.clone();
                    copy["sources"] = serde_json::Value::Array(Vec::new());
                    copy
                });
                if let Some(list) = entry["sources"].as_array_mut() {
                    let already = list.iter().any(|s| s.as_str() == Some(name.as_str()));
                    if !already {
                        list.push(serde_json::Value::String(name.clone()));
                    }
                }
            }
            let next = block["pageInfo"]["endCursor"]
                .as_str()
                .unwrap_or("")
                .to_string();
            // Same guard as the org walk: a cursor that stops advancing
            // ends this source rather than spinning the poller thread.
            if block["pageInfo"]["hasNextPage"].as_bool().unwrap_or(false)
                && !next.is_empty()
                && Some(&next) != cursors[*src].as_ref()
            {
                cursors[*src] = Some(next);
                next_live.push(*src);
            }
        }
        live = next_live;
        enrich(&mut pool, &fresh_ids, tok);

        // Publish what has landed before asking for the next page. The
        // board fills as the pages arrive rather than staying empty until
        // the last source is exhausted, and the count in the header is the
        // count on screen at every moment in between.
        if let Ok(mut g) = state.lock() {
            g.stages
                .count("pull requests", order.len(), None);
            let partial: Vec<serde_json::Value> = order
                .iter()
                .filter_map(|url| pool.get(url).cloned())
                .collect();
            let (total, capped) = union_total(&counted, partial.len());
            g.total = total;
            // Still fetching is still capped, whatever the counts say: the
            // sources that remain live have more behind them.
            g.capped = capped || !live.is_empty();
            g.prs = partial;
        }
    }

    let nodes: Vec<serde_json::Value> = order
        .into_iter()
        .filter_map(|url| pool.remove(&url))
        .collect();
    if let Ok(mut g) = state.lock() {
        g.query = pairs
            .iter()
            .map(|(n, _)| n.clone())
            .collect::<Vec<_>>()
            .join(", ");
        let mut said = if source == "config" {
            tc::config_token_warning().unwrap_or_default()
        } else {
            String::new()
        };
        for note in [legacy_section_note(), refilled_sources_note()]
            .into_iter()
            .flatten()
        {
            said = if said.is_empty() {
                note
            } else {
                format!("{} · {}", said, note)
            };
        }
        settle(&mut g, nodes, &counted, served, deepened.is_some(), said);
    }
    Ok(())
}

/// What a finished pass leaves on the pane.
///
/// Its own function so a pass that fell back or stopped short can be
/// tested without a network: the two facts about *how* the pass went go in
/// their own fields and `err` carries only what `warnings` brought - the
/// config notes, which really are things to fix. A page size GitHub would
/// not serve and paging that stopped at a ceiling are neither. Folded into
/// `err` they drew the `!` banner in the warning colour and read, to
/// anyone looking at the pane, as a widget that had failed: the list was
/// real, the count was honestly a floor, and a minute later it was gone.
fn settle(
    g: &mut State,
    nodes: Vec<serde_json::Value>,
    counted: &[(i64, usize)],
    served: Option<usize>,
    stopped: bool,
    warnings: String,
) {
    let (total, capped) = union_total(counted, nodes.len());
    g.total = total;
    // Paging that stopped short is capped whatever the arithmetic says.
    g.capped = capped || stopped;
    g.prs = nodes;
    g.fetched = tc::now();
    g.served = served;
    g.stopped = stopped;
    g.err = warnings;
}

/// The dim line under the header when a pass fell back or stopped short.
///
/// One line, or nothing at all. The sentence this replaced was two
/// sentences long - `GitHub refused 25 per page; served 10 · GitHub
/// stopped paging this search; showing every result it served` - and in a
/// wall pane it wrapped three rows in the `!` slot, which is most of what
/// made a slow minute look like a broken widget.
///
/// Built to a width rather than to a fixed wording, because the three
/// things worth saying do not fit fifty-eight columns together: the page
/// size served and how far the pass got come first, and *when the next
/// pass is* is dropped when the pane cannot hold it. Dropped, not cut -
/// half a hint about a countdown is worse than no countdown.
///
/// `next_in` is seconds until the next pass and is expected to be
/// recomputed every frame; a countdown frozen into a string at the end of
/// the pass would be wrong before anyone read it.
fn partial_note(
    served: Option<usize>,
    got: usize,
    total: usize,
    capped: bool,
    stopped: bool,
    next_in: Option<u64>,
    w: usize,
) -> String {
    if served.is_none() && !stopped {
        return String::new();
    }
    let mut parts: Vec<String> = vec!["GitHub is slow".to_string()];
    if let Some(size) = served {
        parts.push(format!("served {}/page", size));
    }
    if stopped {
        // "of at least", for the reason the header says it: the sources
        // overlap and one that filled its page has more behind it, so the
        // total is a floor and this is a count against a floor.
        parts.push(format!(
            "{} of {}{}",
            got,
            if capped { "at least " } else { "" },
            total
        ));
    }
    if let Some(secs) = next_in {
        parts.push(format!("next pass in {}s", secs));
    }
    // Everything after the first piece is dropped from the tail until the
    // line fits. The first piece is short enough to fit any pane a widget
    // is drawn in at all, so this always terminates with something.
    while parts.len() > 1 {
        let line = format!(" {}", parts.join(" · "));
        if line.chars().count() <= w.saturating_sub(1) {
            return line;
        }
        parts.pop();
    }
    format!(" {}", parts[0])
}

struct Palette {
    ok: String,
    warn: String,
    bad: String,
    bad_lit: String,
    dim: String,
    /// A colour to draw over the selected-row tint.
    ///
    /// `dim` is 3.81 against `bg(38, 56, 76)`, under the 4.5 CLAUDE.md asks for
    /// against the tint as well as the background. This is the same grey lifted
    /// until it clears - 4.94 - and it is used *only* where a tint is on, so an
    /// untinted row is exactly the colour it always was. Not quite the same as
    /// "unselected": herdr-panes tints a blocked or done row whether or not it
    /// is selected, and those get the lighter colours too.
    ///
    /// The substitution happens inside the closure that composes the tint, not
    /// at each call site. Seventeen sites were counted when this was found and
    /// there were twenty-three by the time it was fixed; more than half of them
    /// reach `dim` through a condition that has nothing to do with selection -
    /// `if count > 0 { loud } else { dim }` - and a zero count is the normal
    /// state, so those are the common case rather than the rare one. Anyone
    /// fixing this a call site at a time would fix the obvious half.
    dim_lit: String,
    grid: String,
    txt: String,
    lbl: String,
    accent: String,
    pr: String,
}

fn palette() -> Palette {
    Palette {
        ok: tc::rgb(90, 240, 160),
        warn: tc::rgb(255, 200, 90),
        bad: tc::rgb(255, 100, 110),
        bad_lit: tc::rgb(255, 128, 136),
        dim: tc::rgb(127, 147, 172),
        dim_lit: tc::rgb(140, 170, 195),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
        pr: tc::rgb(180, 160, 255),
    }
}

fn review_label<'a>(decision: &str, p: &'a Palette) -> (&'static str, &'a str) {
    match decision {
        "APPROVED" => ("approved", &p.ok),
        "CHANGES_REQUESTED" => ("changes", &p.bad),
        "REVIEW_REQUIRED" => ("needs review", &p.warn),
        _ => ("—", &p.dim),
    }
}

fn check_label<'a>(state: &str, p: &'a Palette) -> (&'static str, &'a str) {
    match state {
        "SUCCESS" => ("pass", &p.ok),
        "FAILURE" | "ERROR" => ("FAIL", &p.bad),
        "PENDING" => ("running", &p.warn),
        "EXPECTED" => ("waiting", &p.dim),
        _ => ("—", &p.dim),
    }
}

fn merge_label<'a>(state: &str, p: &'a Palette) -> (&'static str, &'a str) {
    match state {
        "CLEAN" => ("ready", &p.ok),
        "DIRTY" => ("CONFLICT", &p.bad),
        "BLOCKED" => ("blocked", &p.warn),
        "BEHIND" => ("behind", &p.warn),
        "UNSTABLE" => ("checks failing", &p.warn),
        // HAS_HOOKS means a merge queue or a required hook stands between
        // this and the button - the PR itself is mergeable. pr.py has always
        // called it ready; the port called it "checking", which reads as
        // "not finished yet" and is the opposite of what it means.
        "HAS_HOOKS" => ("ready", &p.ok),
        // Drafts had no case and fell to the em-dash, so a draft was
        // indistinguishable from a PR whose state GitHub had not sent.
        "DRAFT" => ("draft", &p.dim),
        "UNKNOWN" => ("checking", &p.dim),
        _ => ("—", &p.dim),
    }
}

fn matches(pr: &serde_json::Value, needle: &str) -> bool {
    if needle.is_empty() {
        return true;
    }
    let hay = [
        number(pr, "number").to_string(),
        text(pr, "title"),
        text(&pr["author"], "login"),
        text(&pr["repository"], "nameWithOwner"),
        text(pr, "headRefName"),
        text(pr, "baseRefName"),
    ]
    .join(" ")
    .to_lowercase();
    hay.contains(&needle.to_lowercase())
}

fn sort_prs(prs: &[serde_json::Value], field: &str, newest_first: bool) -> Vec<serde_json::Value> {
    let key = if field == "updated" { "updatedAt" } else { "createdAt" };
    let mut out = prs.to_vec();
    out.sort_by(|a, b| {
        let (x, y) = (text(a, key), text(b, key));
        if newest_first { y.cmp(&x) } else { x.cmp(&y) }
    });
    out
}

fn main() {
    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    if !tc::dependencies_available(
        "github-prs",
        include_str!("dependencies.json"),
        Some(SETTINGS),
    ) {
        return;
    }
    let cfg = if on_legacy_section() {
        tc::load_config("pr")
    } else {
        tc::load_config("github_prs")
    };
    let mut refresh = tc::poll_secs(tc::cfg_f64(&cfg, "refresh", 60.0), 60.0);
    let limit = tc::cfg_usize(&cfg, "limit", 25);
    let sources = sources_from(&cfg);
    let _ = SOURCES_REFILLED.set(sources_were_emptied(&cfg));

    let args: Vec<String> = std::env::args().skip(1).collect();
    let mut extra: Vec<String> = Vec::new();
    let mut i = 0;
    while i < args.len() {
        match args[i].as_str() {
            "-n" | "--refresh" if i + 1 < args.len() => {
                refresh = tc::poll_secs(args[i + 1].parse().unwrap_or(60.0), 60.0);
                i += 2;
            }
            other if !other.starts_with('-') => {
                extra.push(other.to_string());
                i += 1;
            }
            _ => i += 1,
        }
    }

    let p = palette();
    let state = Arc::new(Mutex::new(State::default()));
    let rate = Arc::new(Mutex::new(Rate::default()));
    let wake = Arc::new((Mutex::new(false), Condvar::new()));
    let (tok, source) = token(&cfg);
    let env_name = {
        let name = tc::cfg_str(&cfg, "token_env", TOKEN_ENV);
        if name.is_empty() {
            TOKEN_ENV.to_string()
        } else {
            name
        }
    };

    let poller = Arc::clone(&state);
    let poller_wake = Arc::clone(&wake);
    let poller_rate = Arc::clone(&rate);
    let poll_tok = tok.clone();
    let poll_env = env_name.clone();
    let poll_sources = sources.clone();
    let poll_extra = extra.clone();
    std::thread::spawn(move || loop {
        if poll_tok.is_empty() {
            if let Ok(mut g) = poller.lock() {
                g.err = tc::missing_config(&format!(
                    "no token: set github_prs.token or ${}",
                    poll_env
                ));
            }
        } else {
            let want = poller.lock().ok().and_then(|g| {
                let have = g.detail.as_ref().map(|d| number(d, "number"));
                match &g.want {
                    Some(w) if Some(w.2) != have => Some(w.clone()),
                    _ => None,
                }
            });
            let mut failed = None;
            if let Some(want) = want {
                if let Err(said) = fetch_detail(&poll_tok, &want, &poller) {
                    failed = Some(said);
                }
            }
            // Before the list, and its own request either way. Before,
            // because the list pages for minutes on a large board and two
            // figures that arrive only at the end of that are two figures
            // nobody looks at. Its own request, because OPS-84 spent a day
            // making the list survive GitHub's slow spells and a count
            // refused beside it must not undo that: the reason lands in
            // `counts_err` rather than `err`, the last good counts stay on
            // screen, and the MERGED caption says they are the older ones.
            //
            // Skipped when the owner walk itself failed, because then the
            // scope is not known and `fetch_list` is about to report the
            // same failure where a reader will look for it.
            if failed.is_none() && discover_owners(&poll_tok, &poller).is_ok() {
                let count_days = chart_days();
                if let Err(said) = fetch_counts(&poll_tok, &count_days, &poller, &poller_rate) {
                    if let Ok(mut g) = poller.lock() {
                        g.counts_err = said;
                    }
                }
            }
            if failed.is_none() {
                if let Err(said) = fetch_list(
                    &poll_tok,
                    source,
                    &poll_sources,
                    &poll_extra,
                    limit,
                    &poller,
                    &poller_rate,
                ) {
                    failed = Some(said);
                }
            }
            if let Some(said) = failed {
                if let Ok(mut g) = poller.lock() {
                    g.err = said;
                    g.loading = false;
                }
            }
        }
        let (lock, cond) = &*poller_wake;
        let mut asked = match lock.lock() {
            Ok(g) => g,
            Err(_) => return,
        };
        if !*asked {
            asked = match cond.wait_timeout(asked, Duration::from_secs_f64(refresh)) {
                Ok((g, _)) => g,
                Err(_) => return,
            };
        }
        *asked = false;
    });

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut selected, mut tick, mut stack_sel) = (0usize, 0usize, 0usize);
    // Where the list and the detail have been scrolled to, and whether a key
    // has just moved the selection. The wheel writes a scroll and never the
    // flag, so neither screen chases a cursor the moment it is turned.
    let (mut board, mut dscroll, mut moved) = (0usize, 0usize, false);
    // The stack cursor is a second selection, on the detail page. The
    // wheel writes `dscroll` and never this, so walking a stack with
    // the arrows is what brings that row back into view.
    let mut stack_moved = false;
    let mut sort_at = 0usize;
    let mut newest_first = true;
    let (mut needle, mut typing) = (String::new(), false);
    let mut show_stats = true;
    let mut copied: (String, f64) = (String::new(), 0.0);
    let mut source_filter = "all".to_string();
    let filter_names: Vec<String> = std::iter::once("all".to_string())
        .chain(sources.iter().map(|(n, _)| n.clone()))
        .collect();

    let nudge = |wake: &Arc<(Mutex<bool>, Condvar)>| {
        let (lock, cond) = &**wake;
        if let Ok(mut asked) = lock.lock() {
            *asked = true;
            cond.notify_all();
        }
    };

    loop {
        tick += 1;
        #[allow(clippy::type_complexity)]
        let (
            prs,
            total,
            capped,
            counts,
            counts_err,
            detail,
            stack_rows,
            loading,
            err,
            served,
            stopped,
            fetched,
            stages,
            target,
            owners,
        ) = match state.lock() {
                Ok(g) => (
                    g.prs.clone(),
                    g.total,
                    g.capped,
                    g.counts.clone(),
                    g.counts_err.clone(),
                    g.detail.clone(),
                    g.stack_rows.clone(),
                    g.loading,
                    g.err.clone(),
                    g.served,
                    g.stopped,
                    g.fetched,
                    g.stages.clone(),
                    g.target.clone(),
                    // The same reckoning `github` and `gha` count: every org
                    // the viewer belongs to, plus the viewer's own account.
                    // `@mine` expands to exactly this list, so the number on
                    // screen is the number searched rather than a config
                    // entry that may name accounts no search reached.
                    g.orgs.len() + usize::from(!g.viewer.is_empty()),
                ),
                Err(_) => return,
            };
        let shown: Vec<serde_json::Value> = sort_prs(&prs, SORTS[sort_at], newest_first)
            .into_iter()
            .filter(|pr| matches(pr, &needle))
            .filter(|pr| {
                source_filter == "all"
                    || pr["sources"]
                        .as_array()
                        .into_iter()
                        .flatten()
                        .any(|s| s.as_str() == Some(source_filter.as_str()))
            })
            .collect();

        for key in keyboard.poll() {
            if typing {
                // While filtering, keys are text - only escape and enter are
                // navigation, or the filter could never contain "q".
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
                moved = true;
                continue;
            }
            if key == "," {
                tc::run_settings(&mut keyboard, SETTINGS);
                continue;
            }
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "/" => typing = true,
                // Left comes out of the detail the way it does everywhere
                // else. esc keeps its second job of clearing the search when
                // there is no detail open; left has no business doing that,
                // so it only acts when there is something to come out of.
                "left" if detail.is_some() || loading => {
                    if let Ok(mut g) = state.lock() {
                        g.want = None;
                        g.detail = None;
                        g.stack_rows.clear();
                        g.loading = false;
                        g.stages.clear();
                    }
                    stack_sel = 0;
                    dscroll = 0;
                }
                "esc" => {
                    if detail.is_some() || loading {
                        if let Ok(mut g) = state.lock() {
                            g.want = None;
                            g.detail = None;
                            g.stack_rows.clear();
                            g.loading = false;
                            g.stages.clear();
                        }
                        stack_sel = 0;
                        dscroll = 0;
                    } else {
                        needle.clear();
                    }
                }
                // Right and enter both go in - and from inside the stack,
                // in again, onto the PR under the cursor.
                "right" | "enter" => {
                    if let Some(open) = &detail {
                        if !stack_rows.is_empty() {
                            // Walk the stack from inside it: the row under
                            // the cursor becomes the PR on screen.
                            let node = &stack_rows[stack_sel.min(stack_rows.len() - 1)].1;
                            let url = text(open, "url");
                            let parts: Vec<&str> = url.split('/').collect();
                            if parts.len() >= 5 && number(node, "number") != number(open, "number")
                            {
                                if let Ok(mut g) = state.lock() {
                                    g.want = Some((
                                        parts[3].to_string(),
                                        parts[4].to_string(),
                                        number(node, "number"),
                                    ));
                                    g.detail = None;
                                    g.stack_rows.clear();
                                    g.loading = true;
                                    g.target =
                                        format!("{}/{} #{}", parts[3], parts[4], number(node, "number"));
                                    g.stages.clear();
                                }
                                stack_sel = 0;
                                dscroll = 0;
                                nudge(&wake);
                            }
                        }
                    } else if !shown.is_empty() && !loading {
                        let pick = &shown[selected.min(shown.len() - 1)];
                        let full = text(&pick["repository"], "nameWithOwner");
                        if let Some((owner, name)) = full.split_once('/') {
                            if let Ok(mut g) = state.lock() {
                                g.want =
                                    Some((owner.into(), name.into(), number(pick, "number")));
                                g.detail = None;
                                g.stack_rows.clear();
                                g.loading = true;
                                g.target = format!("{} #{}", full, number(pick, "number"));
                                g.stages.clear();
                            }
                            nudge(&wake);
                        }
                    }
                }
                "c" | "C" => {
                    // The URL of whatever is on screen: the open PR in the
                    // dashboard, the highlighted row in the list.
                    let url = match &detail {
                        Some(d) => text(d, "url"),
                        None => shown
                            .get(selected.min(shown.len().saturating_sub(1)))
                            .map(|pr| text(pr, "url"))
                            .unwrap_or_default(),
                    };
                    if !url.is_empty() {
                        copied = (
                            if tc::clipboard(&url) {
                                url
                            } else {
                                format!("no clipboard: {}", url)
                            },
                            tc::now(),
                        );
                    }
                }
                "r" | "R" => nudge(&wake),
                "f" | "F" => {
                    // Every PR remembers which sources found it, so
                    // narrowing to one is instant and costs no request.
                    let at = filter_names
                        .iter()
                        .position(|n| *n == source_filter)
                        .unwrap_or(0);
                    source_filter = filter_names[(at + 1) % filter_names.len()].clone();
                    moved = true;
                }
                "s" | "S" => sort_at = (sort_at + 1) % SORTS.len(),
                "o" | "O" => newest_first = !newest_first,
                "t" | "T" => show_stats = !show_stats,
                "up" => {
                    if detail.is_some() {
                        stack_sel = stack_sel.saturating_sub(1);
                        stack_moved = true;
                    } else {
                        selected = selected.saturating_sub(1);
                        moved = true;
                    }
                }
                "down" => {
                    if detail.is_some() {
                        stack_sel += 1;
                        stack_moved = true;
                    } else {
                        selected += 1;
                        moved = true;
                    }
                }
                // The wheel moves whichever screen is in front of you and
                // never a selection - selection is the arrows' job, here as
                // everywhere in the collection.
                "ctrl-y" | "wheel-up" => {
                    let at = if detail.is_some() { &mut dscroll } else { &mut board };
                    *at = at.saturating_sub(1);
                }
                "ctrl-e" | "wheel-down" => {
                    let at = if detail.is_some() { &mut dscroll } else { &mut board };
                    *at = at.saturating_add(1);
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        // "github" in the name, like the two beside it on the wall. Three
        // panes headed GITHUB OPS, GITHUB PRS and GITHUB ACTIONS say what
        // they have in common; PR WATCH read as a fourth unrelated thing.
        let mut rows = vec![tc::title("github prs", w, &p.accent)];
        // Two rows, in the order the other GitHub widgets use them. The
        // first is the chrome every polling widget draws and is meant to
        // read identically across panes: who is being watched, when it last
        // answered, what is left of the budget. The second is this widget's
        // own count, which does not fit that shape and should not be bent
        // into it - `gha` puts its runs/repos line in the same place.
        let mut head = vec![(
            p.dim.as_str(),
            format!(" {} account{}", owners, if owners == 1 { "" } else { "s" }),
        )];
        let budget = rate
            .lock()
            .map(|g| match (g.remaining, g.limit) {
                (Some(left), Some(limit)) => Some((left, limit)),
                _ => None,
            })
            .unwrap_or(None);
        let tail = tc::polled(fetched, budget, &p.dim, &p.ok, &p.warn);
        for (colour, txt) in &tail {
            head.push((colour.as_str(), txt.clone()));
        }
        rows.push(tc::seg(&head, w - 1));

        // "at least", because a source that filled its page has more behind
        // it and the sources overlap, so the union cannot be added up - only
        // bounded from below.
        let mut count = vec![
            (
                p.dim.as_str(),
                format!(
                    " {} of {}{}",
                    shown.len(),
                    if capped { "at least " } else { "" },
                    total
                ),
            ),
            (
                p.dim.as_str(),
                if !needle.is_empty() || source_filter != "all" {
                    " shown".to_string()
                } else {
                    " open".to_string()
                },
            ),
        ];
        if !copied.0.is_empty() && tc::now() - copied.1 < 4.0 {
            count.push((p.ok.as_str(), "   copied ".into()));
            count.push((
                p.dim.as_str(),
                copied.0.chars().take(w.saturating_sub(46).max(10)).collect(),
            ));
        }
        rows.push(tc::seg(&count, w - 1));
        // How the last pass went, when it went unusually: dim, one line,
        // under the count it qualifies. Recomputed here rather than stored
        // because the countdown moves - and because the width it has to fit
        // is only known here.
        let next_in = (fetched > 0.0 && !loading)
            .then(|| fetched + refresh - tc::now())
            .filter(|left| *left > 0.0)
            .map(|left| left.round() as u64);
        let note = partial_note(served, prs.len(), total, capped, stopped, next_in, w);
        if !note.is_empty() {
            rows.push(tc::seg(&[(p.dim.as_str(), note)], w - 1));
        }
        if !err.is_empty() {
            rows.extend(tc::error_rows(p.bad.as_str(), &err, w));
        }

        let mut stack_cursor: Option<usize> = None;
        let hints: Vec<Vec<(&str, String)>> = if detail.is_some() || loading {
            let mut stack_sel_clamped = stack_sel;
            if !stack_rows.is_empty() {
                stack_sel_clamped = stack_sel.min(stack_rows.len() - 1);
                stack_sel = stack_sel_clamped;
            }
            let top = rows.len();
            let (detail_rows, stack_at) = detail_view(
                detail.as_ref(),
                &stack_rows,
                stack_sel_clamped,
                loading,
                w,
                h,
                tick,
                stages.steps(),
                &target,
                top,
                &p,
            );
            rows.extend(detail_rows);
            stack_cursor = stack_at.map(|at| top + at);
            let mut hints: Vec<Vec<(&str, String)>> = Vec::new();
            if !stack_rows.is_empty() {
                hints.push(vec![
                    (p.accent.as_str(), "↑↓".into()),
                    (p.dim.as_str(), " stack".into()),
                ]);
                hints.push(vec![(p.dim.as_str(), "[↵] open it".into())]);
            }
            hints.push(vec![(p.dim.as_str(), "[c]opy url".into())]);
            hints.push(vec![
                (p.accent.as_str(), "←".into()),
                (p.dim.as_str(), "/[esc] back".into()),
            ]);
            hints.push(vec![(p.dim.as_str(), "[r]efresh".into())]);
            hints.push(vec![(p.dim.as_str(), "[,] settings".into())]);
            hints.push(vec![(p.dim.as_str(), "[q]uit".into())]);
            hints
        } else {
            if !shown.is_empty() && selected >= shown.len() {
                selected = shown.len() - 1;
            }
            // Two day charts plus state and age, so the block is a board of
            // its own. Raising this gate to match that height would hide the
            // new charts on a pane that can scroll, which is the reading the
            // scroll rule forbids. The body is a window onto whatever they
            // need, and `t` hides them when the list should have the first
            // screen.
            if show_stats && h >= 30 {
                // Every open PR, not `shown`: the filter is a search of the
                // board, not a redefinition of it.
                rows.extend(stats_view(
                    &sort_prs(&prs, SORTS[sort_at], newest_first),
                    total,
                    capped,
                    counts.as_ref(),
                    &counts_err,
                    w,
                    tick,
                    &p,
                ));
            }
            let top = rows.len();
            let (list, first) = list_view(
                &shown,
                selected,
                SORTS[sort_at],
                newest_first,
                &needle,
                w,
                h,
                fetched == 0.0,
                &source_filter,
                top,
                board,
                moved,
                &p,
            );
            board = first;
            moved = false;
            rows.extend(list);
            vec![
                vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
                vec![(p.dim.as_str(), "[↵] open".into())],
                vec![(p.dim.as_str(), "[/]filter".into())],
                vec![(p.dim.as_str(), format!("[s]ort {}", SORTS[sort_at]))],
                vec![(
                    p.dim.as_str(),
                    format!("[o]rder {}", if newest_first { "newest" } else { "oldest" }),
                )],
                vec![(p.dim.as_str(), format!("[f]rom {}", source_filter))],
                vec![(
                    p.dim.as_str(),
                    format!("[t]stats {}", if show_stats { "on" } else { "off" }),
                )],
                vec![(p.dim.as_str(), "[c]opy url".into())],
                vec![(p.dim.as_str(), "[r]efresh".into())],
                vec![(p.dim.as_str(), "[,] settings".into())],
                vec![(p.dim.as_str(), "[q]uit".into())],
            ]
        };
        let hints = if typing {
            vec![
                vec![(p.accent.as_str(), format!("/{}▌", needle))],
                vec![(p.dim.as_str(), "[↵] keep".into())],
                vec![(p.dim.as_str(), "[esc] clear".into())],
            ]
        } else {
            hints
        };
        let footer: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        // A window onto the body rather than a cut of it, with the title
        // pinned above: scrolled away, the detail screen stops saying which
        // pull request it is describing. The list windows itself, so only
        // the detail has anywhere to scroll to.
        let room = h.saturating_sub(footer.len());
        let (head, rest) = rows.split_at(1.min(rows.len()));
        let room_below = room.saturating_sub(head.len()).max(1);
        let off = if detail.is_some() || loading {
            // Only on the frame a key walked the stack. The STACK section
            // windows itself around `stack_sel`, but that does not move
            // the section inside the outer page - without this chase a
            // walked row can sit below the fold with no way back but
            // the wheel.
            if stack_moved {
                if let Some(at) = stack_cursor {
                    dscroll = tc::follow(dscroll, at.saturating_sub(head.len()), room_below);
                }
                stack_moved = false;
            }
            dscroll = dscroll.min(rest.len().saturating_sub(room_below));
            dscroll
        } else {
            stack_moved = false;
            0
        };
        let mut frame: Vec<String> = head.to_vec();
        frame.extend(rest.iter().skip(off).take(room_below).cloned());
        while frame.len() < room {
            frame.push(String::new());
        }
        frame.extend(footer);
        tc::draw(&frame, w, h);
        std::thread::sleep(Duration::from_millis(300));
    }
}

/// Shape and age of every open PR, whatever the list is filtered to.
///
/// `tc::seg` for parts whose colours are owned rather than borrowed.
///
/// The two day charts compose their rows out of pieces built in three
/// different places, and threading `&str` lifetimes through that costs
/// more than one clone per segment on a redraw.
fn seg_owned(parts: &[(String, String)], width: usize) -> String {
    let borrowed: Vec<(&str, String)> = parts
        .iter()
        .map(|(colour, text)| (colour.as_str(), text.clone()))
        .collect();
    tc::seg(&borrowed, width)
}

/// A 3x5 pixel digit, `#` on and space off.
///
/// Core has no large-digit font, and the figure beside each chart has to
/// read as the headline of its row rather than as one more label. The two
/// halves of a cell are used as two pixel rows instead: three rows of text
/// carry six, which is exactly one of these glyphs and a blank row under
/// it, so the digits stand three rows tall against the three rows of bars
/// without needing a row of their own.
const DIGITS: [[&str; 5]; 10] = [
    ["###", "# #", "# #", "# #", "###"],
    ["  #", "  #", "  #", "  #", "  #"],
    ["###", "  #", "###", "#  ", "###"],
    ["###", "  #", "###", "  #", "###"],
    ["# #", "# #", "###", "  #", "  #"],
    ["###", "#  ", "###", "  #", "###"],
    ["###", "#  ", "###", "# #", "###"],
    ["###", "  #", "  #", "  #", "  #"],
    ["###", "# #", "###", "# #", "###"],
    ["###", "# #", "###", "  #", "###"],
];

/// One number, three text rows tall. Every row is the same width.
fn big_digits(value: i64) -> Vec<String> {
    let shown = value.to_string();
    let mut rows = vec![String::new(); 3];
    for (i, ch) in shown.chars().enumerate() {
        let glyph = ch
            .to_digit(10)
            .map(|d| DIGITS[d as usize])
            .unwrap_or(["   ", "   ", "   ", "   ", "   "]);
        for (r, row) in rows.iter_mut().enumerate() {
            if i > 0 {
                row.push(' ');
            }
            for c in 0..3 {
                let lit = |pixels: Option<&&str>| {
                    pixels
                        .and_then(|line| line.as_bytes().get(c).copied())
                        .unwrap_or(b' ')
                        == b'#'
                };
                // The sixth pixel row does not exist, which is the gap that
                // keeps two stacked figures from touching.
                row.push(match (lit(glyph.get(r * 2)), lit(glyph.get(r * 2 + 1))) {
                    (true, true) => '█',
                    (true, false) => '▀',
                    (false, true) => '▄',
                    (false, false) => ' ',
                });
            }
        }
    }
    rows
}

/// The two labels the figures carry, and the only two places the wording
/// is written down. They are the widest thing in that half of the pane, so
/// they are what its width is measured from.
const FIGURE_LABELS: [&str; 2] = ["merged · last 24h", "opened · last 24h"];

/// Room for three digits drawn large, which is `4n - 1` cells.
///
/// Fixed at three rather than fitted to the figure so the column does not
/// jump sideways the day a count crosses a hundred. A fourth digit drops
/// that figure to plain text rather than being cut.
const FIGURE_DIGITS: usize = 11;

/// How a chart row's width divides between the bars and the figure.
///
/// Three widths, because the figure is worth more than the label beside
/// it and the label is worth more than nothing:
///
/// - room for both, and the label sits on one line beside the axis;
/// - room for the digits only, and the label wraps onto two lines under
///   them - `merged` over `last 24h`, which still says what it is;
/// - room for neither, and the number sits on its own row under the
///   caption. A narrow pane loses the size of the number, never the number.
fn split_halves(w: usize) -> (usize, usize) {
    let row = w.saturating_sub(1);
    let wide = FIGURE_LABELS
        .iter()
        .map(|l| tc::display_width(l))
        .max()
        .unwrap_or(0)
        + 2;
    let narrow = FIGURE_DIGITS + 2;
    let need = OPENED_DAYS as usize + 2;
    if row >= need + wide {
        (row - wide, wide)
    } else if row >= need + narrow {
        (row - narrow, narrow)
    } else {
        (row, 0)
    }
}

/// The right half of a chart: one figure, or why there is not one yet.
///
/// Five rows, to match the chart beside it - three of bars, the axis rule,
/// the axis labels - so the digits sit against the bars and the label sits
/// on the axis line where the chart's own `30d ago … today` is.
fn figure_block(
    figure: Option<i64>,
    stalled: bool,
    label: &str,
    width: usize,
    tick: usize,
    p: &Palette,
) -> Vec<Vec<(String, String)>> {
    let gutter = || (tc::RST.to_string(), "  ".to_string());
    let inner = width.saturating_sub(2);
    // One line beside the axis when it fits, or wrapped onto two under the
    // digits when it does not. Never cut: half a label is a figure whose
    // window nobody can read.
    let label_rows: Vec<String> = if tc::display_width(label) <= inner {
        vec![label.to_string()]
    } else {
        label.split(" · ").map(|part| part.to_string()).collect()
    };
    let mut rows: Vec<Vec<(String, String)>> = Vec::new();
    match figure {
        Some(v) => {
            let digits = big_digits(v);
            if digits.first().map(|r| r.chars().count()).unwrap_or(0) <= inner {
                for line in digits {
                    rows.push(vec![gutter(), (p.txt.clone(), line)]);
                }
                rows.push(Vec::new());
            } else {
                // More digits than the half is wide. Six figures merged in
                // a day has not happened here, but a truncated number is a
                // wrong number, so it drops to plain text rather than being
                // cut.
                rows.push(Vec::new());
                rows.push(vec![gutter(), (p.txt.clone(), v.to_string())]);
                rows.push(Vec::new());
                rows.push(Vec::new());
            }
        }
        // A figure that has not arrived is not a figure of zero, and this is
        // the one place the difference is easy to lose: nothing merged in a
        // day is a real and unremarkable reading, so the pane waiting has to
        // look like the pane waiting.
        None if stalled => {
            rows.push(Vec::new());
            rows.push(vec![gutter(), (p.warn.clone(), "not counted".to_string())]);
            rows.push(Vec::new());
            rows.push(Vec::new());
        }
        None => {
            rows.push(Vec::new());
            let mut line: Vec<(String, String)> = vec![gutter()];
            for (colour, txt) in &tc::skeleton(inner.min(11).max(4), tick * 2, 5) {
                line.push((colour.clone(), txt.clone()));
            }
            rows.push(line);
            rows.push(vec![gutter(), (p.dim.clone(), "loading".to_string())]);
            rows.push(Vec::new());
        }
    }
    // Always five rows, whatever is in them, because that is the height of
    // the chart beside it and the two have to end level.
    rows.truncate(5 - label_rows.len());
    while rows.len() + label_rows.len() < 5 {
        rows.push(Vec::new());
    }
    for line in label_rows {
        rows.push(vec![gutter(), (p.dim.clone(), line)]);
    }
    rows
}

/// One thirty-day chart with a single large figure beside it.
///
/// Both rows on this board are drawn by this, which is the only way the
/// bars, the axis and the figure land in the same columns on each and the
/// two can be read against one another. `series` is `None` when the counts
/// behind it have not arrived: the bar area shimmers rather than drawing
/// thirty zeros, because thirty zeros is a claim.
#[allow(clippy::too_many_arguments)]
fn day_chart(
    heading: &str,
    caption: Vec<(String, String)>,
    series: Option<&[(String, i64)]>,
    bar_colour: &str,
    figure: Option<i64>,
    stalled: bool,
    label: &str,
    w: usize,
    tick: usize,
    p: &Palette,
) -> Vec<String> {
    let (left, right) = split_halves(w);
    let mut rows = vec![String::new()];
    let mut cap: Vec<(String, String)> =
        vec![(p.lbl.clone(), format!(" ── {} ── ", heading))];
    cap.extend(caption);
    rows.push(seg_owned(&cap, w - 1));
    if right == 0 {
        // The figure cannot sit after the caption: that line is already
        // the heading plus peak and totals, and `seg` clips from the
        // right, so the number would be the first thing to go. A pane too
        // narrow for the right half keeps it on its own row, where it
        // cannot lose the fight for width. The number first, so even a
        // twenty-column pane still has the figure.
        let mut fig: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".to_string())];
        match figure {
            Some(v) => {
                fig.push((p.txt.clone(), v.to_string()));
                fig.push((p.dim.clone(), format!(" {}", label)));
            }
            None if stalled => {
                fig.push((p.warn.clone(), "not counted".to_string()));
                fig.push((p.dim.clone(), format!(" {}", label)));
            }
            None => {
                fig.push((p.dim.clone(), "loading".to_string()));
                fig.push((p.dim.clone(), format!(" {}", label)));
            }
        }
        rows.push(seg_owned(&fig, w - 1));
    }

    let days = OPENED_DAYS as usize;
    let avail = left.saturating_sub(2).max(10);
    let slot = (avail / days).max(1);
    let gap = if slot >= 3 { 1 } else { 0 };
    let barw = slot - gap;
    let span = barw * days + gap * (days - 1);
    let mut left_rows: Vec<Vec<(String, String)>> = Vec::new();
    match series {
        Some(series) => {
            let mut cols: Vec<(f64, String)> = Vec::new();
            for (i, (_, value)) in series.iter().enumerate() {
                cols.extend(std::iter::repeat_n(
                    (*value as f64, bar_colour.to_string()),
                    barw,
                ));
                if gap > 0 && i + 1 < series.len() {
                    cols.extend(std::iter::repeat_n((0.0, bar_colour.to_string()), gap));
                }
            }
            for line in tc::vbars(&cols, 3, 0.0) {
                let mut parts: Vec<(String, String)> =
                    vec![(tc::RST.to_string(), " ".to_string())];
                for (colour, ch) in &line {
                    parts.push((colour.clone(), ch.clone()));
                }
                left_rows.push(parts);
            }
        }
        None => {
            left_rows.push(Vec::new());
            let mut line: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".to_string())];
            for (colour, txt) in &tc::skeleton(span, tick * 2, 7) {
                line.push((colour.clone(), txt.clone()));
            }
            left_rows.push(line);
            left_rows.push(Vec::new());
        }
    }
    left_rows.push(vec![
        (tc::RST.to_string(), " ".to_string()),
        (p.grid.clone(), "─".repeat(span)),
    ]);
    let ago = format!("{}d ago", OPENED_DAYS);
    let now = "today";
    let mut labels: Vec<(String, String)> = vec![(tc::RST.to_string(), " ".to_string())];
    // Both ends, or only the end that says which way time runs. A chart
    // narrower than the two words together would otherwise draw a row wider
    // than the half it was given and push the figure out of line.
    if span >= ago.chars().count() + now.len() + 1 {
        labels.push((p.dim.clone(), ago.clone()));
        labels.push((
            p.dim.clone(),
            " ".repeat(span - ago.chars().count() - now.len()),
        ));
    }
    labels.push((p.dim.clone(), now.to_string()));
    left_rows.push(labels);

    let figures = figure_block(figure, stalled, label, right, tick, p);
    for (i, mut parts) in left_rows.into_iter().enumerate() {
        if right > 0 {
            let used: usize = parts.iter().map(|(_, t)| tc::display_width(t)).sum();
            parts.push((tc::RST.to_string(), " ".repeat(left.saturating_sub(used))));
            if let Some(fig) = figures.get(i) {
                parts.extend(fig.iter().cloned());
            }
        }
        rows.push(seg_owned(&parts, w - 1));
    }
    rows
}

/// Deliberately not the filtered set. Typing in the filter is a search, and
/// a search should not move the backlog it is searching: watching the age
/// median and the state bar lurch on every keystroke made them unreadable
/// and, worse, made them look like statements about the whole board when
/// they described three matching rows.
#[allow(clippy::too_many_arguments)]
fn stats_view(
    prs: &[serde_json::Value],
    total: usize,
    capped: bool,
    counts: Option<&Counts>,
    counts_err: &str,
    w: usize,
    tick: usize,
    p: &Palette,
) -> Vec<String> {
    let mut rows = vec![String::new()];
    let stalled = !counts_err.is_empty();
    // The merged series and both rolling figures come from `counts`, not
    // from the open pool. An empty board is exactly when merge activity is
    // the only signal left, so that row is built before anything that
    // needs a PR, and STATE / OPENED / DAY / AGE stay behind the guard.
    let days = chart_days();
    // A day the counts do not carry is not a day with nothing in it, so the
    // series is only handed over when every day on the axis is accounted
    // for; otherwise the bars shimmer and the caption says why.
    let merged: Option<Vec<(String, i64)>> = counts.and_then(|c| {
        days.iter()
            .map(|d| {
                c.days
                    .iter()
                    .find(|(had, _)| had == d)
                    .map(|(_, v)| (d.clone(), *v))
            })
            .collect()
    });
    if !prs.is_empty() {
        let n = prs.len();
        let mut review: HashMap<&str, usize> = HashMap::new();
        let mut checks: HashMap<&str, usize> = HashMap::new();
        let (mut drafts, mut conflicts, mut ready) = (0usize, 0usize, 0usize);
        for pr in prs {
            let decision = text(pr, "reviewDecision");
            let slot = match decision.as_str() {
                "APPROVED" => "APPROVED",
                "CHANGES_REQUESTED" => "CHANGES_REQUESTED",
                "REVIEW_REQUIRED" => "REVIEW_REQUIRED",
                _ => "",
            };
            *review.entry(slot).or_insert(0) += 1;
            let state = rollup(pr);
            let slot = match state.as_str() {
                "SUCCESS" => "SUCCESS",
                "FAILURE" => "FAILURE",
                "PENDING" => "PENDING",
                _ => "other",
            };
            *checks.entry(slot).or_insert(0) += 1;
            if pr["isDraft"].as_bool().unwrap_or(false) {
                drafts += 1;
            }
            if text(pr, "mergeable") == "CONFLICTING" {
                conflicts += 1;
            }
            if ready_to_merge(pr) {
                ready += 1;
            }
        }

        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── STATE ── ".into()),
                (p.txt.as_str(), format!("{}", n)),
                // Everything after this counts the PRs in hand. When a source
                // filled its page they are a sample of the board rather than
                // the board, and the line has to say which it is describing.
                (
                    p.dim.as_str(),
                    if capped {
                        format!(" fetched of at least {} open · ", total)
                    } else {
                        " open · ".to_string()
                    },
                ),
                (p.dim.as_str(), format!("{} draft", drafts)),
                (p.dim.as_str(), " · ".into()),
                (
                    if conflicts > 0 { p.bad.as_str() } else { p.dim.as_str() },
                    format!("{} conflicting", conflicts),
                ),
                (p.dim.as_str(), " · ".into()),
                (
                    if ready > 0 { p.ok.as_str() } else { p.dim.as_str() },
                    format!("{} ready to merge", ready),
                ),
            ],
            w - 1,
        ));
        let order: Vec<(&str, &str)> = vec![
            ("APPROVED", p.ok.as_str()),
            ("CHANGES_REQUESTED", p.bad.as_str()),
            ("REVIEW_REQUIRED", p.warn.as_str()),
            ("", p.dim.as_str()),
        ];
        let parts: Vec<(f64, String)> = order
            .iter()
            .filter_map(|(k, c)| {
                let got = review.get(k).copied().unwrap_or(0);
                if got == 0 {
                    return None;
                }
                Some((got as f64 / n as f64, c.to_string()))
            })
            .collect();
        let bar = tc::stacked_bar(&parts, w.saturating_sub(3).max(10));
        let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (colour, txt) in &bar {
            line.push((colour.as_str(), txt.clone()));
        }
        rows.push(tc::seg(&line, w - 1));
        let mut key: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (k, colour) in &order {
            let got = review.get(k).copied().unwrap_or(0);
            if got == 0 {
                continue;
            }
            key.push((colour, "▇ ".into()));
            key.push((p.txt.as_str(), review_label(k, p).0.into()));
            key.push((p.dim.as_str(), format!(" {}   ", got)));
        }
        for (k, colour, label) in [
            ("SUCCESS", p.ok.as_str(), "checks pass"),
            ("FAILURE", p.bad.as_str(), "checks FAIL"),
            ("PENDING", p.warn.as_str(), "running"),
        ] {
            let got = checks.get(k).copied().unwrap_or(0);
            if got == 0 {
                continue;
            }
            key.push((colour, "· ".into()));
            key.push((p.txt.as_str(), label.into()));
            key.push((p.dim.as_str(), format!(" {}   ", got)));
        }
        rows.push(tc::seg(&key, w - 1));

        // When the open ones arrived. Counted from the pool; the merged
        // series cannot be, because the pool is `is:open` throughout, so it
        // comes from GitHub's own aggregates in `counts` and is drawn
        // whether this block ran or not.
        let mut per_day: HashMap<&String, i64> = days.iter().map(|d| (d, 0)).collect();
        let mut inside = 0i64;
        for pr in prs {
            let key: String = text(pr, "createdAt").chars().take(10).collect();
            if let Some(slot) = days.iter().find(|d| **d == key) {
                *per_day.get_mut(slot).unwrap() += 1;
                inside += 1;
            }
        }
        let opened: Vec<(String, i64)> = days
            .iter()
            .map(|d| (d.clone(), per_day.get(d).copied().unwrap_or(0)))
            .collect();
        let peak = opened.iter().map(|(_, v)| *v).max().unwrap_or(0);
        let mut opened_cap: Vec<(String, String)> = vec![
            (p.dim.clone(), format!("last {}d · ", OPENED_DAYS)),
            (p.txt.clone(), format!("{}", inside)),
            // The bars and the figure beside them count different populations -
            // the bars only PRs that are still open, the figure everything
            // opened including what has since been merged or closed - so the
            // caption keeps saying which one it describes.
            (p.dim.clone(), format!(" of {} still open · ", n)),
            (p.dim.clone(), format!("peak {}/day", peak)),
        ];
        if stalled {
            opened_cap.push((p.warn.clone(), " · 24h count stale".to_string()));
        }
        rows.extend(day_chart(
            "OPENED / DAY",
            opened_cap,
            Some(&opened),
            &p.pr,
            counts.map(|c| c.opened_24h),
            stalled,
            FIGURE_LABELS[1],
            w,
            tick,
            &p,
        ));
    }

    let mut merged_cap: Vec<(String, String)> =
        vec![(p.dim.clone(), format!("last {}d · ", OPENED_DAYS))];
    match &merged {
        Some(series) => {
            let total: i64 = series.iter().map(|(_, v)| *v).sum();
            let peak = series.iter().map(|(_, v)| *v).max().unwrap_or(0);
            merged_cap.push((p.txt.clone(), format!("{}", total)));
            merged_cap.push((p.dim.clone(), " merged · ".to_string()));
            merged_cap.push((p.dim.clone(), format!("peak {}/day", peak)));
        }
        None if stalled => merged_cap.push((p.dim.clone(), "not counted".to_string())),
        None => merged_cap.push((p.dim.clone(), "counting".to_string())),
    }
    if stalled {
        // The list is whole and only the count request failed, so this is
        // said as a count that did not land rather than as a broken board -
        // and with the age of what is still on screen, because "stale" on
        // its own does not say whether that is a minute or an afternoon.
        // Minutes, because the count rides the list's refresh and a figure
        // that has missed one round is minutes old, not hours - `span()`
        // would round every one of those to "0h".
        let age = counts
            .map(|c| format!(" · counted {}m ago", ((tc::now() - c.at) / 60.0).max(0.0) as i64))
            .unwrap_or_default();
        merged_cap.push((
            p.warn.clone(),
            format!(" · count failed: {}{}", counts_err, age),
        ));
    }
    rows.extend(day_chart(
        "MERGED / DAY",
        merged_cap,
        merged.as_deref(),
        &p.ok,
        counts.map(|c| c.merged_24h),
        stalled,
        FIGURE_LABELS[0],
        w,
        tick,
        &p,
    ));

    if prs.is_empty() {
        return rows;
    }

    let mut ages: Vec<(f64, &serde_json::Value)> = prs
        .iter()
        .filter_map(|pr| hours_since(&text(pr, "createdAt")).map(|h| (h, pr)))
        .collect();
    ages.sort_by(|a, b| a.0.total_cmp(&b.0));
    let idles: Vec<(f64, &serde_json::Value)> = prs
        .iter()
        .filter_map(|pr| hours_since(&text(pr, "updatedAt")).map(|h| (h, pr)))
        .collect();

    let at = |pairs: &[(f64, &serde_json::Value)], frac: f64| -> Option<f64> {
        if pairs.is_empty() {
            return None;
        }
        let mut vals: Vec<f64> = pairs.iter().map(|x| x.0).collect();
        vals.sort_by(f64::total_cmp);
        Some(vals[((vals.len() as f64 * frac) as usize).min(vals.len() - 1)])
    };
    rows.push(String::new());
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── AGE ── ".into()),
            (p.dim.as_str(), "median ".into()),
            (p.txt.as_str(), span(at(&ages, 0.5))),
            (p.dim.as_str(), "  p95 ".into()),
            (p.txt.as_str(), span(at(&ages, 0.95))),
            (p.dim.as_str(), "  max ".into()),
            (p.warn.as_str(), span(at(&ages, 1.0))),
            (p.dim.as_str(), "   idle median ".into()),
            (p.txt.as_str(), span(at(&idles, 0.5))),
        ],
        w - 1,
    ));
    if !ages.is_empty() {
        // One bar per open PR, youngest left to oldest right - the x axis is
        // rank, not time. It fills the pane and carries a baseline and end
        // labels, because a sparkline that stops in the middle of the screen
        // gives no way to tell where the chart ends and the blank begins.
        let room = w.saturating_sub(3).max(10);
        let drawn: &[(f64, &serde_json::Value)] = if ages.len() > room {
            &ages[ages.len() - room..]
        } else {
            &ages
        };
        // Spread the remainder across the leftmost bars so the chart reaches
        // the right edge exactly: stopping short of it left no way to tell a
        // finished chart from a truncated one.
        let (slot, extra) = if drawn.len() >= room {
            (1, 0)
        } else {
            (room / drawn.len(), room % drawn.len())
        };
        let hi = drawn.iter().map(|x| x.0).fold(0.0f64, f64::max).max(1.0);
        let mut bars = String::new();
        for (i, (hours, _)) in drawn.iter().enumerate() {
            let wide_bar = slot + usize::from(i < extra);
            let level = ((hours / hi) * 7.99) as usize;
            for _ in 0..wide_bar {
                bars.push(tc::SPARK[level.min(7)]);
            }
        }
        let count = bars.chars().count();
        rows.push(tc::seg(
            &[(tc::RST, " ".into()), (&tc::heat(0.4), bars)],
            w - 1,
        ));
        rows.push(tc::seg(
            &[(tc::RST, " ".into()), (p.grid.as_str(), "─".repeat(count))],
            w - 1,
        ));
        let left = format!("youngest {}", span(Some(drawn[0].0)));
        let right = format!("oldest {}", span(Some(drawn[drawn.len() - 1].0)));
        let note = if drawn.len() < ages.len() {
            format!("{} of {} PRs", drawn.len(), ages.len())
        } else {
            format!("{} PRs", drawn.len())
        };
        let mid = count
            .saturating_sub(left.len() + right.len() + note.len() + 2)
            .max(1);
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!(" {}", left)),
                (p.dim.as_str(), " ".repeat(mid / 2)),
                (p.grid.as_str(), note),
                (p.dim.as_str(), " ".repeat(mid - mid / 2 + 2)),
                (p.dim.as_str(), right),
            ],
            w - 1,
        ));
    }
    if let Some(fattest) = prs.iter().max_by_key(|p| number(p, "additions") + number(p, "deletions"))
    {
        let worst = |pairs: &[(f64, &serde_json::Value)]| -> String {
            match pairs.iter().max_by(|a, b| a.0.total_cmp(&b.0)) {
                Some((hours, pr)) => {
                    format!("#{} {}", number(pr, "number"), span(Some(*hours)))
                }
                None => "--".into(),
            }
        };
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  oldest ".into()),
                (p.warn.as_str(), tc::pad(&worst(&ages), 12)),
                (p.dim.as_str(), "  untouched longest ".into()),
                (p.warn.as_str(), tc::pad(&worst(&idles), 12)),
                (p.dim.as_str(), "  biggest ".into()),
                (
                    p.txt.as_str(),
                    format!(
                        "#{} +{}/-{}",
                        number(fattest, "number"),
                        number(fattest, "additions"),
                        number(fattest, "deletions")
                    ),
                ),
            ],
            w - 1,
        ));
    }
    rows
}

#[allow(clippy::too_many_arguments)]
fn list_view(
    prs: &[serde_json::Value],
    selected: usize,
    sort_field: &str,
    newest_first: bool,
    needle: &str,
    w: usize,
    h: usize,
    waiting: bool,
    source_filter: &str,
    top: usize,
    from: usize,
    chase: bool,
    p: &Palette,
) -> (Vec<String>, usize) {
    let mut rows = vec![String::new()];
    let arrow = if newest_first { "↓" } else { "↑" };
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── OPEN PRs ── ".into()),
            (p.dim.as_str(), format!("by {} {}", sort_field, arrow)),
            (
                p.dim.as_str(),
                if source_filter != "all" {
                    format!("   from {}", source_filter)
                } else {
                    String::new()
                },
            ),
            (
                p.accent.as_str(),
                if needle.is_empty() {
                    String::new()
                } else {
                    format!("   /{}", needle)
                },
            ),
        ],
        w - 1,
    ));
    if prs.is_empty() {
        // "collecting" is only true before the first fetch: an empty filter
        // or an empty source is a result, not a wait.
        let why = if !needle.is_empty() {
            format!("  nothing matches /{}", needle)
        } else if source_filter != "all" {
            format!("  no open PRs from {}", source_filter)
        } else if waiting {
            "  collecting…".to_string()
        } else {
            "  no open PRs".to_string()
        };
        rows.push(tc::seg(&[(p.dim.as_str(), why)], w - 1));
        return (rows, 0);
    }

    // Columns are budgeted rather than guessed: the fixed ones are summed
    // and the title takes exactly what is left, so nothing runs off the
    // right edge or into its neighbour.
    let wide = w >= 96;
    let repo_w = if wide { 18 } else { 0 };
    let size_w = if wide { 12 } else { 0 };
    let fixed = 8 + repo_w + 13 + 8 + 6 + size_w;
    let title_w = (w - 1).saturating_sub(fixed).max(16);
    let mut head = format!(" {:<7}", "PR");
    if repo_w > 0 {
        head += &format!("{:<width$}", "REPO", width = repo_w);
    }
    // The time column follows the sort, so the number you ordered by is the
    // number you can see. Labelling both "AGE" had it reporting idle time
    // while the stats above reported true age, and the two disagreed.
    let when_label = if sort_field == "created" { "AGE" } else { "IDLE" };
    head += &format!(
        "{:<width$}{:>13}{:>8}{:>6}",
        "TITLE",
        "REVIEW",
        "CHECKS",
        when_label,
        width = title_w
    );
    if size_w > 0 {
        head += &format!("{:>width$}", "SIZE", width = size_w);
    }
    rows.push(tc::seg(&[(p.dim.as_str(), tc::pad(&head, w - 1))], w - 1));

    // `top` is what was drawn above this view. Without it the window is
    // sized as though the list began at the top of the screen, so it renders
    // far more rows than are visible, the caller truncates the overflow, and
    // the selection scrolls off the bottom while `first` is still 0.
    let room = h.saturating_sub(top + rows.len() + 3).max(1);
    // Centred on the cursor on a frame a key moved it, and left exactly
    // where it was on a frame the wheel did. Recentring every frame is what
    // pulled the list straight back from wherever the wheel had put it.
    let furthest = prs.len().saturating_sub(room);
    let first = if !chase {
        from.min(furthest)
    } else if prs.len() > room {
        selected.saturating_sub(room / 2).min(furthest)
    } else {
        0
    };
    for (i, pr) in prs.iter().enumerate().skip(first).take(room) {
        let here = i == selected;
        let tint = if here { tc::bg(38, 56, 76) } else { String::new() };
        let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` was measured first; a review
                // found the others after the first fix shipped saying it was
                // done, so they are here by measurement rather than by guess.
                let colour = if tint.is_empty() {
                    colour
                } else if colour == p.dim {
                    p.dim_lit.as_str()
                } else if colour == p.bad {
                    p.bad_lit.as_str()
                } else {
                    colour
                };
                format!("{}{}", tint, colour)
            };
        let (rlabel, rcol) = review_label(&text(pr, "reviewDecision"), p);
        let (clabel, ccol) = check_label(&rollup(pr), p);
        let stacked = !pr["stackEntry"].is_null();
        let mut line = vec![(
            c(if here { &p.accent } else { &p.pr }),
            format!(
                "{}{}",
                if here { "▸" } else { " " },
                tc::pad(&format!("#{}", number(pr, "number")), 7)
            ),
        )];
        if repo_w > 0 {
            // Clipped one short of the column so it never touches the title.
            let full = text(&pr["repository"], "nameWithOwner");
            let repo = full.rsplit('/').next().unwrap_or("").to_string();
            line.push((
                c(&p.dim),
                tc::pad(
                    &repo.chars().take(repo_w - 1).collect::<String>(),
                    repo_w,
                ),
            ));
        }
        let mut name = format!(
            "{}{}",
            if stacked { "⣿ " } else { "" },
            text(pr, "title")
        );
        if pr["isDraft"].as_bool().unwrap_or(false) {
            name = format!("draft · {}", name);
        }
        line.push((
            c(&p.txt),
            tc::pad(
                &name.chars().take(title_w - 1).collect::<String>(),
                title_w,
            ),
        ));
        line.push((c(rcol), format!("{:>13}", rlabel)));
        line.push((c(ccol), format!("{:>8}", clabel)));
        line.push((
            c(&p.dim),
            format!(
                "{:>6}",
                ago(&text(
                    pr,
                    if sort_field == "created" { "createdAt" } else { "updatedAt" }
                ))
            ),
        ));
        if size_w > 0 {
            line.push((
                c(&p.dim),
                format!(
                    "{:>width$}",
                    format!("+{}/-{}", number(pr, "additions"), number(pr, "deletions")),
                    width = size_w
                ),
            ));
        }
        if here {
            line.push((tint.clone(), " ".repeat(w)));
        }
        let refs: Vec<(&str, String)> = line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
        rows.push(tc::seg(&refs, w - 1));
    }
    (rows, first)
}

#[allow(clippy::too_many_arguments)]
fn detail_view(
    pr: Option<&serde_json::Value>,
    stack_rows: &[StackRow],
    stack_sel: usize,
    loading: bool,
    w: usize,
    h: usize,
    tick: usize,
    stages: &[tc::Step],
    target: &str,
    top: usize,
    p: &Palette,
) -> (Vec<String>, Option<usize>) {
    let mut rows = vec![String::new()];
    let mut cursor = None;
    let Some(pr) = pr.filter(|_| !loading) else {
        // A shimmer says "wait" and nothing else. The open really does run
        // in stages, so show them: a spinner on the one in flight, a tick
        // and a duration on the ones behind it. Honest, and it reads like a
        // machine doing something rather than a placeholder.
        //
        // The drawing moved to core so `gha` could report the same way; the
        // reasoning above is why it exists at all and stays here with the
        // widget that found it.
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── OPENING ── ".into()),
                (p.accent.as_str(), target.to_string()),
            ],
            w - 1,
        ));
        rows.push(String::new());
        rows.extend(tc::progress_rows(
            stages, w, tick, &p.ok, &p.accent, &p.txt, &p.dim,
        ));
        if stages.is_empty() {
            rows.push(tc::seg(
                &[
                    (
                        p.accent.as_str(),
                        format!("   {}  ", tc::SPINNER[tick % tc::SPINNER.len()]),
                    ),
                    (p.dim.as_str(), "connecting".into()),
                ],
                w - 1,
            ));
        }
        rows.push(String::new());
        // One sweeping line rather than four fat bars of shimmer.
        let shimmer = tc::skeleton(w.saturating_sub(6).max(10), tick * 2, 7);
        let mut line: Vec<(&str, String)> = vec![(tc::RST, "  ".into())];
        for (colour, txt) in &shimmer {
            line.push((colour.as_str(), txt.clone()));
        }
        rows.push(tc::seg(&line, w - 1));
        return (rows, None);
    };

    let draft = if pr["isDraft"].as_bool().unwrap_or(false) {
        " · draft"
    } else {
        ""
    };
    rows.push(tc::seg(
        &[
            (p.pr.as_str(), format!(" #{} ", number(pr, "number"))),
            (
                p.txt.as_str(),
                text(pr, "title")
                    .chars()
                    .take(w.saturating_sub(24).max(10))
                    .collect::<String>(),
            ),
            (p.dim.as_str(), draft.into()),
        ],
        w - 1,
    ));
    rows.push(tc::seg(
        &[
            (p.dim.as_str(), "  ".into()),
            (
                p.dim.as_str(),
                match text(&pr["author"], "login") {
                    s if s.is_empty() => "?".into(),
                    s => s,
                },
            ),
            (p.dim.as_str(), "   ".into()),
            (p.accent.as_str(), text(pr, "headRefName")),
            (p.dim.as_str(), " → ".into()),
            (p.accent.as_str(), text(pr, "baseRefName")),
        ],
        w - 1,
    ));

    let (rlabel, rcol) = review_label(&text(pr, "reviewDecision"), p);
    let (mlabel, mcol) = merge_label(&text(pr, "mergeStateStatus"), p);
    let unresolved = pr["reviewThreads"]["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|t| !t["isResolved"].as_bool().unwrap_or(false))
        .count();
    rows.push(String::new());
    let cells: Vec<(String, String, &str)> = vec![
        ("review".into(), rlabel.into(), rcol),
        ("merge".into(), mlabel.into(), mcol),
        (
            "unresolved threads".into(),
            unresolved.to_string(),
            if unresolved > 0 { p.bad.as_str() } else { p.ok.as_str() },
        ),
        (
            "size".into(),
            format!(
                "+{}/-{} in {} files",
                number(pr, "additions"),
                number(pr, "deletions"),
                number(pr, "changedFiles")
            ),
            p.txt.as_str(),
        ),
        (
            "commits".into(),
            number(&pr["commitCount"], "totalCount").to_string(),
            p.txt.as_str(),
        ),
        (
            "opened / updated".into(),
            format!(
                "{} ago / {} ago",
                ago(&text(pr, "createdAt")),
                ago(&text(pr, "updatedAt"))
            ),
            p.txt.as_str(),
        ),
    ];
    let label_w = cells.iter().map(|c| c.0.len()).max().unwrap_or(8);
    let ncols = if (w - 2) / 2 >= label_w + 3 + 18 { 2 } else { 1 };
    let cw = (w - 2) / ncols;
    let val_w = cw.saturating_sub(label_w + 3).max(6);
    for chunk in cells.chunks(ncols) {
        let mut line: Vec<(&str, String)> = vec![(tc::RST, " ".into())];
        for (label, value, colour) in chunk {
            line.push((p.dim.as_str(), format!(" {} ", tc::pad(label, label_w))));
            line.push((colour, tc::pad(value, val_w)));
        }
        rows.push(tc::seg(&line, w - 1));
    }

    // Who has looked at it. The last state per person wins: someone who
    // requested changes and later approved has approved, and showing both
    // would misreport the gate.
    let mut latest: HashMap<String, String> = HashMap::new();
    for r in pr["reviews"]["nodes"].as_array().into_iter().flatten() {
        let who = text(&r["author"], "login");
        if !who.is_empty() {
            latest.insert(who, text(r, "state"));
        }
    }
    let mut pending: Vec<String> = Vec::new();
    for n in pr["reviewRequests"]["nodes"].as_array().into_iter().flatten() {
        let who = &n["requestedReviewer"];
        let name = match text(who, "login") {
            s if !s.is_empty() => s,
            _ => text(who, "name"),
        };
        if !name.is_empty() && !latest.contains_key(&name) {
            pending.push(name);
        }
    }
    let pick = |state: &str| -> Vec<String> {
        let mut out: Vec<String> = latest
            .iter()
            .filter(|(_, v)| *v == state)
            .map(|(k, _)| k.clone())
            .collect();
        out.sort();
        out
    };
    let groups: Vec<(&str, &str, Vec<String>)> = vec![
        ("approved", p.ok.as_str(), pick("APPROVED")),
        ("changes requested", p.bad.as_str(), pick("CHANGES_REQUESTED")),
        ("commented", p.dim.as_str(), pick("COMMENTED")),
        ("awaiting", p.warn.as_str(), {
            pending.sort();
            pending.clone()
        }),
    ];
    rows.push(String::new());
    let live: Vec<&(&str, &str, Vec<String>)> = groups.iter().filter(|g| !g.2.is_empty()).collect();
    rows.push(tc::seg(
        &[
            (p.lbl.as_str(), " ── REVIEWERS ── ".into()),
            (
                p.dim.as_str(),
                if live.is_empty() {
                    "nobody has been asked".to_string()
                } else {
                    live.iter()
                        .map(|g| format!("{} {}", g.2.len(), g.0))
                        .collect::<Vec<_>>()
                        .join(" · ")
                },
            ),
        ],
        w - 1,
    ));
    for (label, colour, who) in &live {
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), format!("  {}", tc::pad(label, 18))),
                (
                    colour,
                    who.join(", ")
                        .chars()
                        .take(w.saturating_sub(24).max(10))
                        .collect::<String>(),
                ),
            ],
            w - 1,
        ));
    }

    let roll = pr["commits"]["nodes"]
        .as_array()
        .and_then(|a| a.first())
        .map(|n| n["commit"]["statusCheckRollup"].clone())
        .unwrap_or(serde_json::Value::Null);
    rows.push(String::new());
    if roll.is_null() {
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── CHECKS ── ".into()),
                (p.dim.as_str(), "none on the last commit".into()),
            ],
            w - 1,
        ));
    } else {
        let (state, scol) = check_label(&text(&roll, "state"), p);
        let ctx: Vec<&serde_json::Value> = roll["contexts"]["nodes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|c| !c.is_null())
            .collect();
        // The query asks for one page of contexts, and a repository with
        // more of them than that fits sent back a page rather than all of
        // them - so `ctx.len()` is how many arrived, not how many ran. The
        // rollup reports the true count for a point of field complexity, and
        // a run that is missing here is exactly the one worth knowing about:
        // a failure outside the page cannot be named, only counted.
        let counted = roll["contexts"]["totalCount"].as_i64().unwrap_or(0).max(0) as usize;
        // Never below what is in hand: a missing field must not turn eleven
        // checks into "11 of 0".
        let counted = counted.max(ctx.len());
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── CHECKS ── ".into()),
                (scol, state.into()),
                (
                    p.dim.as_str(),
                    if ctx.len() < counted {
                        format!("   {} of {} fetched", ctx.len(), counted)
                    } else {
                        format!("   {} total", counted)
                    },
                ),
            ],
            w - 1,
        ));
        let verdict_of = |c: &serde_json::Value| -> String {
            for key in ["conclusion", "state", "status"] {
                let v = text(c, key);
                if !v.is_empty() {
                    return v;
                }
            }
            String::new()
        };
        let is_bad = |c: &serde_json::Value| -> bool {
            let v = verdict_of(c);
            !matches!(v.as_str(), "SUCCESS" | "NEUTRAL" | "SKIPPED" | "")
        };
        // Failures first: a green wall of passing checks is not why anyone
        // opens this view.
        let ordered: Vec<&&serde_json::Value> = ctx
            .iter()
            .filter(|c| is_bad(c))
            .chain(ctx.iter().filter(|c| !is_bad(c)))
            .collect();
        for c in ordered.into_iter().take(8) {
            let name = match text(c, "name") {
                s if !s.is_empty() => s,
                _ => match text(c, "context") {
                    s if !s.is_empty() => s,
                    _ => "?".into(),
                },
            };
            let verdict = verdict_of(c);
            let (lab, col) = check_label(&verdict, p);
            let lab = if lab == "—" && !verdict.is_empty() {
                verdict.to_lowercase()
            } else {
                lab.to_string()
            };
            let took = match (
                parse(&text(c, "startedAt")),
                parse(&text(c, "completedAt")),
            ) {
                (Some(a), Some(b)) => format!("{}s", (b - a).num_seconds()),
                _ => String::new(),
            };
            rows.push(tc::seg(
                &[
                    (p.dim.as_str(), "  ".into()),
                    (p.txt.as_str(), tc::pad(&name, w.saturating_sub(30).max(12))),
                    (col, format!("{:>10}", lab)),
                    (p.dim.as_str(), format!("{:>8}", took)),
                ],
                w - 1,
            ));
        }
    }

    if !stack_rows.is_empty() {
        let native = !pr["stack"].is_null();
        rows.push(String::new());
        // The stack scrolls: eleven-deep stacks exist, and a pane that has
        // already spent its height on checks cannot show them all.
        let room = h.saturating_sub(top + rows.len() + 4).max(3);
        let first = if stack_rows.len() > room {
            stack_sel.saturating_sub(room / 2).min(stack_rows.len() - room)
        } else {
            0
        };
        rows.push(tc::seg(
            &[
                (p.lbl.as_str(), " ── STACK ── ".into()),
                (
                    p.dim.as_str(),
                    format!(
                        "{} pull requests · {}",
                        stack_rows.len(),
                        if native { "from GitHub" } else { "inferred from branches" }
                    ),
                ),
                (
                    p.accent.as_str(),
                    if stack_rows.len() > room {
                        format!(
                            "   ↑↓ {}-{} of {}",
                            first + 1,
                            (first + room).min(stack_rows.len()),
                            stack_rows.len()
                        )
                    } else {
                        String::new()
                    },
                ),
            ],
            w - 1,
        ));
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  merge bottom-up: ".into()),
                (p.txt.as_str(), "the one nearest the base branch first".into()),
                (p.dim.as_str(), "   ▸ cursor · ● on screen".into()),
            ],
            w - 1,
        ));
        let base = if native {
            text(&pr["stack"], "baseRefName")
        } else {
            text(pr, "baseRefName")
        };
        rows.push(tc::seg(
            &[
                (p.dim.as_str(), "  ".into()),
                (
                    p.accent.as_str(),
                    if base.is_empty() { "trunk".into() } else { base },
                ),
            ],
            w - 1,
        ));
        for (idx, (twig, node, is_here, position)) in
            stack_rows.iter().enumerate().skip(first).take(room)
        {
            let (lab, col) = review_label(&text(node, "reviewDecision"), p);
            let (mlab, mcol) = match text(node, "mergeable").as_str() {
                "CONFLICTING" => ("CONFLICT", p.bad.as_str()),
                "MERGEABLE" => ("ok", p.ok.as_str()),
                _ => ("…", p.dim.as_str()),
            };
            let on_cursor = idx == stack_sel;
            if on_cursor {
                cursor = Some(rows.len());
            }
            let tint = if on_cursor { tc::bg(38, 56, 76) } else { String::new() };
            let c = |colour: &str| {
                // Any colour that would not clear AA on this tint is swapped
                // for its lighter twin. `dim` was measured first; a review
                // found the others after the first fix shipped saying it was
                // done, so they are here by measurement rather than by guess.
                let colour = if tint.is_empty() {
                    colour
                } else if colour == p.dim {
                    p.dim_lit.as_str()
                } else if colour == p.bad {
                    p.bad_lit.as_str()
                } else {
                    colour
                };
                format!("{}{}", tint, colour)
            };
            let mut name = text(node, "title");
            if let Some(pos) = position {
                name = format!("{}. {}", pos, name);
            }
            // Two gutter marks, because they answer different questions: ▸
            // is where the cursor is, ● is the PR actually on screen. One
            // symbol plus a colour could not say both.
            let gutter = format!(
                "{}{}",
                if on_cursor { "▸" } else { " " },
                if *is_here { "●" } else { " " }
            );
            let name_w = w
                .saturating_sub(34 + twig.chars().count())
                .max(10);
            let mut line = vec![
                (c(if on_cursor { &p.accent } else { &p.dim }), gutter),
                (c(&p.dim), twig.clone()),
                (
                    c(if on_cursor { &p.accent } else { &p.pr }),
                    format!("#{:<5} ", number(node, "number")),
                ),
                (
                    c(if *is_here || on_cursor { &p.txt } else { &p.dim }),
                    tc::pad(&name.chars().take(name_w).collect::<String>(), name_w),
                ),
                (c(col), format!("{:>13}", lab)),
                (c(mcol), format!("{:>10}", mlab)),
            ];
            if on_cursor {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            rows.push(tc::seg(&refs, w - 1));
        }
    }
    (rows, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Word for word what `post_json` hands back when curl hits `--max-time`.
    /// Exit 28 is curl's timeout; `run_full`'s "did not answer in Ns" is a
    /// different helper and never sits on this path.
    const TIMED_OUT: &str =
        "curl: (28) Operation timed out after 45000 milliseconds with 0 bytes received";

    #[test]
    fn a_gateway_page_never_reaches_the_screen() {
        // What `post_json` hands back when nginx gives up, shortened from
        // the real body. Left whole it wrapped across four rows of the
        // pane and told the reader nothing.
        let body = "HTTP 502: <html> <head><title>502 Bad Gateway</title></head> \
                    <body> <center><h1>502 Bad Gateway</h1></center> \
                    <hr><center>nginx</center> </body> </html>";
        let said = plain_refusal(body);
        assert!(!said.contains('<'), "markup reached the banner: {}", said);
        assert!(said.contains("502"), "the status is the part worth keeping");
        assert!(
            said.contains("too slow"),
            "say which end gave up, not just the number: {}",
            said
        );

        // A status with no meaning of its own still loses the document.
        let said = plain_refusal("HTTP 418: <html>teapot</html>");
        assert_eq!(said, "GitHub returned 418");

        // A 401's body is the reason the token was refused. Dropping it
        // left the pane saying only the number.
        let said = plain_refusal(r#"HTTP 401: {"message":"Bad credentials"}"#);
        assert_eq!(said, "GitHub returned 401: Bad credentials");

        // Not a status at all - curl failed, or the request timed out.
        // Already a sentence, and it is kept.
        assert_eq!(plain_refusal(TIMED_OUT), TIMED_OUT);
        assert!(
            worth_retrying(&plain_refusal(TIMED_OUT)),
            "a timeout has to stay retryable after it is shortened"
        );
    }

    #[test]
    fn only_the_gateway_is_worth_asking_again() {
        assert!(worth_retrying("GitHub returned 502 - the search was too slow to serve"));
        assert!(worth_retrying("GitHub returned 503 - the search was too slow to serve"));
        assert!(worth_retrying("GitHub returned 504 - the search was too slow to serve"));
        assert!(worth_retrying(TIMED_OUT));
        assert!(worth_retrying("curl exited 28"));

        // A smaller page does not change GitHub's mind about any of these,
        // and asking twice more spends rate limit to reprint the message.
        assert!(!worth_retrying("Bad credentials"));
        assert!(!worth_retrying("GitHub returned 401"));
        assert!(!worth_retrying("GitHub returned 403"));
        assert!(!worth_retrying("GitHub returned 500"));
        assert!(!worth_retrying("GitHub returned 501"));
        assert!(!worth_retrying("GitHub returned 505"));
        assert!(!worth_retrying(
            "Field 'stackEntry' doesn't exist on type 'PullRequest'"
        ));
    }

    #[test]
    fn the_page_size_halves_to_a_floor_and_stops() {
        // The sequence `fetch_round` walks from the shipped default. It
        // stops at the floor rather than grinding down to one, because a
        // search that will not serve ten per page is down, not slow.
        let mut sizes = vec![25usize];
        while let Some(next) = smaller(*sizes.last().unwrap()) {
            sizes.push(next);
        }
        assert_eq!(sizes, vec![25, 12, 10]);
        assert_eq!(smaller(PAGE_FLOOR), None, "the floor is where it stops");
    }

    #[test]
    fn a_round_refused_at_the_floor_is_asked_again_once() {
        let refused = "GitHub returned 502 - the search was too slow to serve";
        // Once, which is the whole point: the round the walk gave up on is
        // usually one bad request in a minute that is otherwise answering.
        assert!(retry_again(0, refused), "the first refusal earns a retry");
        // And never twice. A second retry is a loop, and a loop against a
        // search that is genuinely down spends rate limit reprinting the
        // same message.
        assert!(!retry_again(1, refused), "one retry, not a loop");
        assert!(!retry_again(2, refused));

        // The pause is a pause, not a poll: long enough for whatever shed
        // the request to have finished, short enough that the pass a reader
        // is waiting on is still that pass.
        assert!(
            (2..=3).contains(&RETRY_PAUSE.as_secs()),
            "the pause is seconds, not minutes: {:?}",
            RETRY_PAUSE
        );

        // A refusal that means "no" rather than "later" is not slept on.
        // Bad credentials say the same thing three seconds from now.
        assert!(!retry_again(0, "GitHub returned 401: Bad credentials"));
        assert!(!retry_again(0, "GitHub returned 500"));
    }

    #[test]
    fn a_partial_pass_is_a_note_and_not_an_error() {
        // The end of a pass that fell back to ten per page and stopped
        // paging at a ceiling - built here rather than fetched, because
        // what is being tested is what such a pass leaves on the pane.
        let mut g = State::default();
        let nodes: Vec<serde_json::Value> = (0..302)
            .map(|n| serde_json::json!({ "url": format!("u{}", n) }))
            .collect();
        // One source saying it matched 686 and having handed over 302 is a
        // capped source, so the total is a floor.
        settle(&mut g, nodes, &[(686, 302)], Some(10), true, String::new());

        assert!(
            g.err.is_empty(),
            "a slow pass is not a failure and must not draw the ! banner: {}",
            g.err
        );
        assert_eq!(g.served, Some(10), "the page size served is carried");
        assert!(g.stopped, "paging that stopped short says so");
        assert!(g.capped, "a pass that stopped short reports a floor");
        assert_eq!(g.total, 686);
        assert_eq!(g.prs.len(), 302);

        // What `err` is still for: the config notes the caller composes.
        let mut g = State::default();
        settle(&mut g, Vec::new(), &[(0, 0)], None, false, "token in config".into());
        assert_eq!(g.err, "token in config");
        assert_eq!(g.served, None, "an ordinary pass has nothing to say");
        assert!(!g.stopped);
    }

    #[test]
    fn the_note_is_one_line_in_a_fifty_eight_column_pane() {
        // The wall pane the reported banner wrapped three rows in.
        let w = 58;
        let note = partial_note(Some(10), 302, 686, true, true, Some(34), w);
        assert_eq!(note.lines().count(), 1, "one line: {}", note);
        assert!(
            note.chars().count() <= w - 1,
            "{} chars in a {}-column pane: {}",
            note.chars().count(),
            w,
            note
        );
        // The two facts that cannot be dropped: the page size served and
        // how far the pass got, against a total that is a floor.
        assert!(note.contains("served 10/page"), "{}", note);
        assert!(note.contains("302 of at least 686"), "{}", note);

        // The countdown is what the narrow pane gives up, and it comes
        // back when there is room for it. Dropped whole rather than cut:
        // half a countdown teaches a reader nothing.
        assert!(!note.contains("next pass"), "no room for it at 58: {}", note);
        let wide = partial_note(Some(10), 302, 686, true, true, Some(34), 100);
        assert!(wide.contains("next pass in 34s"), "{}", wide);
        assert!(wide.chars().count() <= 99);

        // An uncapped total is a total and is not dressed up as a floor.
        let whole = partial_note(None, 302, 302, false, true, None, 100);
        assert!(whole.contains("302 of 302"), "{}", whole);

        // Nothing unusual happened, so there is no note and no row.
        assert_eq!(partial_note(None, 302, 302, false, false, Some(34), 100), "");

        // Every width a pane can be drawn at keeps it on one line.
        for w in 20..=200 {
            let note = partial_note(Some(10), 1302, 16860, true, true, Some(340), w);
            assert!(
                note.chars().count() <= w.saturating_sub(1).max(1),
                "{} chars at width {}: {}",
                note.chars().count(),
                w,
                note
            );
        }
    }

    #[test]
    fn a_search_that_cannot_be_paged_is_a_search_capped_at_one_page() {
        let qs = vec!["is:open is:pr".to_string(), "author:@me".to_string()];

        // Without pageInfo the caller's hasNextPage is always false, so
        // every source retires after its first page and the widget quietly
        // goes back to showing 50 - the exact failure this pass removed,
        // and one nothing else would notice.
        let first = list_query(&qs, 50, &[None, None]);
        assert!(first.contains("pageInfo"), "{}", first);
        assert!(first.contains("hasNextPage"), "{}", first);
        assert!(first.contains("endCursor"), "{}", first);
        assert!(!first.contains("after:"), "no cursor yet: {}", first);
        assert_eq!(first.matches("search(").count(), 2);

        // A cursor reaches the source it belongs to, and only that one.
        let next = list_query(&qs, 50, &[Some("Y3Vyc29yOjE=".into()), None]);
        assert!(next.contains(r#"after: "Y3Vyc29yOjE=""#), "{}", next);
        assert_eq!(next.matches("after:").count(), 1, "{}", next);

        // The page size stays at 50. Three searches of 100 answer 502 -
        // the ceiling is nodes per request, so more results come from more
        // rounds, never from a bigger page.
        assert!(next.contains("first: 50"), "{}", next);
    }

    #[test]
    fn ready_means_nothing_is_left_to_do() {
        let pr = |json: &str| -> serde_json::Value { serde_json::from_str(json).unwrap() };
        let green = pr(r#"{"reviewDecision": "APPROVED", "mergeable": "MERGEABLE",
            "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "SUCCESS"}}}]}}"#);
        assert!(ready_to_merge(&green));
        // A repo with no CI at all still counts: no checks is not a failing
        // check, and holding those back would empty the one actionable
        // number on the board.
        let no_ci = pr(r#"{"reviewDecision": "APPROVED", "mergeable": "MERGEABLE"}"#);
        assert!(ready_to_merge(&no_ci));
        // Every one of the four gates on its own is enough to hold it.
        assert!(!ready_to_merge(&pr(
            r#"{"reviewDecision": "REVIEW_REQUIRED", "mergeable": "MERGEABLE"}"#
        )));
        assert!(!ready_to_merge(&pr(
            r#"{"reviewDecision": "APPROVED", "mergeable": "CONFLICTING"}"#
        )));
        assert!(!ready_to_merge(&pr(
            r#"{"reviewDecision": "APPROVED", "isDraft": true}"#
        )));
        assert!(!ready_to_merge(&pr(
            r#"{"reviewDecision": "APPROVED",
                "commits": {"nodes": [{"commit": {"statusCheckRollup": {"state": "FAILURE"}}}]}}"#
        )));
        // Enrich failed: the rollup was never read. That is not "no CI".
        assert!(!ready_to_merge(&pr(
            r#"{"reviewDecision": "APPROVED", "mergeable": "MERGEABLE", "checksUnknown": true}"#
        )));
    }

    #[test]
    fn a_stack_is_inferred_from_where_the_branches_sit() {
        // main <- a <- b, and c also on a: a tree rather than a line.
        let prs: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"number": 1, "headRefName": "a", "baseRefName": "main"},
                {"number": 2, "headRefName": "b", "baseRefName": "a"},
                {"number": 3, "headRefName": "c", "baseRefName": "a"}]"#,
        )
        .unwrap();
        let (root, parent, kids) = stack_of(2, &prs);
        assert_eq!(root, Some(1));
        assert_eq!(parent.get(&2), Some(&1));
        let mut branched = kids.get(&1).cloned().unwrap_or_default();
        branched.sort_unstable();
        assert_eq!(branched, vec![2, 3]);
        // A PR that neither sits on another nor carries one has no stack,
        // rather than a stack of itself.
        let lone: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"number": 9, "headRefName": "x", "baseRefName": "main"}]"#,
        )
        .unwrap();
        assert_eq!(stack_of(9, &lone).0, None);
    }

    #[test]
    fn a_cycle_of_branches_does_not_hang_the_walk() {
        // Two PRs each based on the other's branch. It should not happen,
        // and the walk must still finish.
        let prs: Vec<serde_json::Value> = serde_json::from_str(
            r#"[{"number": 1, "headRefName": "a", "baseRefName": "b"},
                {"number": 2, "headRefName": "b", "baseRefName": "a"}]"#,
        )
        .unwrap();
        let (root, _, _) = stack_of(1, &prs);
        assert!(root.is_some());
    }

    #[test]
    fn a_page_that_filled_up_turns_the_total_into_a_floor() {
        // Nothing filled its page: the pool is the union, exactly.
        assert_eq!(union_total(&[(30, 30), (12, 12)], 35), (35, false));
        // `orgs` filled its page of 50 with 212 behind it. The sources
        // overlap, so 212 + 12 would be nonsense - but the union contains
        // the whole of `orgs`, so it holds at least 212.
        assert_eq!(union_total(&[(212, 50), (12, 12)], 58), (212, true));
        // A capped source can still be smaller than what is already pooled,
        // and then the pool is the better floor of the two.
        assert_eq!(union_total(&[(51, 50), (3, 3)], 53), (53, true));
        // One source, capped, with nothing else to add to it.
        assert_eq!(union_total(&[(60, 50)], 50), (60, true));
        // No sources is no floor, and above all not a claim of zero open
        // PRs dressed up as one.
        assert_eq!(union_total(&[], 0), (0, false));
    }

    #[test]
    fn an_empty_sources_map_lands_on_the_code_defaults() {
        // Absent and empty deliberately agree. `sources` replaces the
        // shipped three wholesale, and the settings screen will delete the
        // last entry, so an empty map is one keypress away - and it used to
        // buy a board that searched nothing and then reported "no open PRs"
        // and "0 of 0 open", totals about a search that never ran.
        let absent = sources_from(&serde_json::json!({}));
        assert_eq!(absent.len(), 3, "an absent key keeps the shipped searches");

        let empty = sources_from(&serde_json::json!({"sources": {}}));
        assert_eq!(empty.len(), 3, "an empty map falls back to the same three");
        assert_eq!(absent, empty, "the two have to be indistinguishable");

        let named = sources_from(&serde_json::json!({"sources": {"mine": "is:open is:pr"}}));
        assert_eq!(named.len(), 1, "a named map replaces the three wholesale");
    }

    #[test]
    fn only_an_emptied_map_is_worth_a_note() {
        // The note explains a file that says `{}` beside a board searching
        // three things. Saying it when nothing was emptied would be noise.
        assert!(sources_were_emptied(&serde_json::json!({"sources": {}})));
        assert!(!sources_were_emptied(&serde_json::json!({})));
        assert!(!sources_were_emptied(
            &serde_json::json!({"sources": {"mine": "is:open is:pr"}})
        ));
    }

    #[test]
    fn no_sources_would_have_meant_no_request_at_all() {
        // Why the fallback exists rather than an empty board being drawn
        // honestly: paging is `while !live.is_empty()` over one entry per
        // search, so zero searches is zero rounds and GitHub is never
        // called. There is no answer of "none" to draw - only the absence
        // of a question. `sources_from` is what now makes this unreachable.
        let none = searches(&[], "someone", &["an-org".to_string()], &[]);
        assert!(
            none.is_empty(),
            "no sources yields no queries, so the paging loop never runs"
        );

        // And with a source it does ask, so the emptiness above is the
        // sources' doing and not this function refusing to build anything.
        let one = searches(
            &[("mine".to_string(), "is:open is:pr @mine".to_string())],
            "someone",
            &["an-org".to_string()],
            &[],
        );
        assert_eq!(one.len(), 1);
        assert!(one[0].1.contains("org:an-org"), "got {:?}", one[0].1);
    }

    #[test]
    fn mine_expands_to_every_org_plus_the_account() {
        let sources = vec![
            ("orgs".to_string(), "is:open is:pr @mine".to_string()),
            ("authored".to_string(), "is:open is:pr author:@me".to_string()),
        ];
        let out = searches(&sources, "wiiiimm", &["acme".into(), "beta".into()], &[]);
        assert_eq!(out[0].1, "is:open is:pr org:acme org:beta user:wiiiimm");
        // A source that does not mention @mine is left alone.
        assert_eq!(out[1].1, "is:open is:pr author:@me");
        // Extra arguments are appended to every source.
        let with = searches(&sources, "w", &[], &["repo:x/y".into()]);
        assert!(with[1].1.ends_with("repo:x/y"));
    }

    #[test]
    fn the_filter_looks_at_everything_on_the_row() {
        let pr: serde_json::Value = serde_json::from_str(
            r#"{"number": 42, "title": "Fix the thing",
                "author": {"login": "wiiiimm"},
                "repository": {"nameWithOwner": "acme/widgets"},
                "headRefName": "fix/thing", "baseRefName": "main"}"#,
        )
        .unwrap();
        for needle in ["42", "thing", "WIIIIMM", "widgets", "fix/", "main"] {
            assert!(matches(&pr, needle), "{} did not match", needle);
        }
        assert!(!matches(&pr, "nonsense"));
        // No filter matches everything, rather than nothing.
        assert!(matches(&pr, ""));
    }

    #[test]
    fn a_span_rolls_over_before_it_stops_reading() {
        assert_eq!(span(None), "--");
        assert_eq!(span(Some(5.0)), "5h");
        assert_eq!(span(Some(72.0)), "3d");
        assert_eq!(span(Some(24.0 * 400.0)), "1.1y");
    }

    /// A drawn row with its colours taken off, which is what a reader sees.
    fn plain(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars().peekable();
        while let Some(c) = chars.next() {
            if c == '\x1b' {
                for next in chars.by_ref() {
                    if next.is_ascii_alphabetic() {
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
    fn the_counts_search_the_same_ground_as_the_list() {
        let orgs = vec!["acme".to_string(), "beta".to_string()];
        let scope = mine_scope("wiiiimm", &orgs);
        assert_eq!(scope, "org:acme org:beta user:wiiiimm");
        // The list and the counts have to describe the same board, which is
        // only true while both build the scope in one place.
        let sources = vec![("orgs".to_string(), "is:open is:pr @mine".to_string())];
        let list = searches(&sources, "wiiiimm", &orgs, &[]);
        assert!(list[0].1.contains(&scope), "{} lost the scope", list[0].1);
        let q = count_query(&scope, &["2026-01-01".to_string()], "2026-01-01T00:00:00Z");
        assert!(q.contains(&scope), "the count query searches somewhere else");
        // No account discovered yet is not "search everything".
        assert_eq!(mine_scope("", &[]), "");
    }

    #[test]
    fn the_count_query_asks_for_every_day_and_both_windows() {
        let days: Vec<String> = (0..OPENED_DAYS)
            .map(|k| format!("2026-09-{:02}", k + 1))
            .collect();
        let q = count_query("org:acme user:w", &days, "2026-09-10T11:22:33Z");
        for (n, day) in days.iter().enumerate() {
            assert!(q.contains(&format!("m{}: search(", n)), "no alias m{}", n);
            assert!(q.contains(&format!("merged:{}\"", day)), "no day {}", day);
        }
        assert!(!q.contains(&format!("m{}: search(", OPENED_DAYS)));
        // Both rolling windows are full datetimes rather than dates. A bare
        // date is "today so far", which at breakfast is three hours of
        // evidence drawn as a day of it; GitHub accepts the datetime, which
        // was verified against the live API before this was relied on.
        assert!(
            q.contains("is:merged merged:>=2026-09-10T11:22:33Z"),
            "no merged window: {}",
            q
        );
        assert!(
            q.contains("is:pr created:>=2026-09-10T11:22:33Z"),
            "no opened window: {}",
            q
        );
        // One request, which is the whole point of asking for aggregates,
        // and it reports what the request cost.
        assert!(q.contains("rateLimit { remaining limit }"));
        assert_eq!(q.matches("search(").count(), OPENED_DAYS as usize + 2);
        assert_eq!(
            q.matches("org:acme user:w is:pr").count(),
            OPENED_DAYS as usize + 2
        );
        assert!(q.starts_with('{') && q.trim_end().ends_with('}'));
    }

    #[test]
    fn a_missing_alias_is_not_a_day_with_nothing_in_it() {
        let days = vec!["2026-09-09".to_string(), "2026-09-10".to_string()];
        let full: serde_json::Value = serde_json::from_str(
            r#"{"m0": {"issueCount": 4}, "m1": {"issueCount": 0},
                "mw": {"issueCount": 2}, "cw": {"issueCount": 9}}"#,
        )
        .unwrap();
        let got = read_counts(&full, &days).unwrap();
        assert_eq!(
            got.days,
            vec![
                ("2026-09-09".to_string(), 4),
                ("2026-09-10".to_string(), 0)
            ]
        );
        assert_eq!((got.merged_24h, got.opened_24h), (2, 9));
        // An alias GitHub did not answer fails the read. A zero in its place
        // would be a bar chart asserting a quiet day that may have been the
        // busiest of the month, and the day cache would never ask again.
        for gone in ["m0", "m1", "mw", "cw"] {
            let mut short = full.clone();
            short.as_object_mut().unwrap().remove(gone);
            let said = read_counts(&short, &days).unwrap_err();
            assert!(said.contains(gone), "{} went missing and got {:?}", gone, said);
        }
    }

    #[test]
    fn a_figure_that_has_not_arrived_is_not_a_figure_of_zero() {
        let p = palette();
        let drawn = |figure, stalled| {
            figure_block(figure, stalled, FIGURE_LABELS[0], 19, 3, &p)
                .iter()
                .map(|row| plain(&seg_owned(row, 19)))
                .collect::<Vec<_>>()
                .join("\n")
        };
        let waiting = drawn(None, false);
        let real = drawn(Some(0), false);
        let dead = drawn(None, true);
        // Zero merged in a day is a real reading and is drawn as a figure.
        assert!(real.contains("█▀█"), "zero was not drawn large: {:?}", real);
        assert!(!real.contains("loading"));
        // A count still on its way says so, and draws no figure at all.
        assert!(waiting.contains("loading"), "{:?}", waiting);
        assert!(!waiting.contains("█▀█"), "a pending count drew a zero: {:?}", waiting);
        assert_ne!(waiting, real);
        // And one that has failed outright is neither: still on its way is a
        // promise, and that one has been broken.
        assert!(dead.contains("not counted"), "{:?}", dead);
        assert!(!dead.contains("loading"), "{:?}", dead);
        assert!(!dead.contains("█▀█"), "{:?}", dead);
        // Every state still says what the number would have meant.
        for state in [&waiting, &real, &dead] {
            assert!(state.contains(FIGURE_LABELS[0]), "unlabelled: {:?}", state);
        }
    }

    #[test]
    fn a_digit_is_three_rows_tall_and_every_row_the_same_width() {
        assert_eq!(big_digits(0), ["█▀█", "█ █", "▀▀▀"]);
        for value in [0i64, 7, 42, 184, 99999] {
            let rows = big_digits(value);
            assert_eq!(rows.len(), 3);
            let want = 4 * value.to_string().chars().count() - 1;
            for row in &rows {
                assert_eq!(row.chars().count(), want, "{} drew {:?}", value, rows);
            }
        }
    }

    #[test]
    fn both_halves_of_a_day_chart_fit_at_every_width() {
        let p = palette();
        let days = chart_days();
        let series: Vec<(String, i64)> =
            days.iter().enumerate().map(|(i, d)| (d.clone(), i as i64 % 7)).collect();
        let label = FIGURE_LABELS[0];
        for w in 20..=200usize {
            let (left, right) = split_halves(w);
            for figure in [Some(0), Some(7), Some(184), Some(123456), None] {
                for series in [Some(series.as_slice()), None] {
                    let rows = day_chart(
                        "MERGED / DAY",
                        vec![(
                            p.dim.clone(),
                            "last 30d · 216 merged · peak 33/day".to_string(),
                        )],
                        series,
                        &p.ok,
                        figure,
                        false,
                        FIGURE_LABELS[0],
                        w,
                        4,
                        &p,
                    );
                    // A row wider than the pane wraps, and a wrapped row
                    // costs the frame a line and scrolls the pinned title
                    // off the top.
                    for row in &rows {
                        let seen = plain(row);
                        assert!(
                            tc::display_width(&seen) <= w - 1,
                            "w={} drew {} cells: {:?}",
                            w,
                            tc::display_width(&seen),
                            seen
                        );
                    }
                    if right == 0 {
                        // The fallback row is the only place the number can
                        // live once the right half is gone, and the heading
                        // plus peak already fill a 45-column caption, so
                        // this is what would vanish if it sat at the end
                        // of that line.
                        let cap: String = rows
                            .iter()
                            .take(3)
                            .map(|r| plain(r))
                            .collect::<Vec<_>>()
                            .join("\n");
                        match figure {
                            Some(0) => assert!(
                                cap.split_whitespace().any(|word| word == "0"),
                                "w={} drew zero as something else: {:?}",
                                w,
                                cap
                            ),
                            Some(v) => assert!(
                                cap.contains(&v.to_string()),
                                "w={} lost the fallback figure {}: {:?}",
                                w,
                                v,
                                cap
                            ),
                            None => assert!(
                                cap.contains("loading"),
                                "w={} pending figure vanished: {:?}",
                                w,
                                cap
                            ),
                        }
                        continue;
                    }
                    // Every word of the label survives, on one line or two,
                    // and the figure's column is the same on every row of
                    // every chart - which is what makes the two rows
                    // readable against each other.
                    let drawn: Vec<String> = rows.iter().map(|r| plain(r)).collect();
                    for word in label.split(" · ") {
                        assert!(
                            drawn.iter().any(|row| row.contains(word)),
                            "w={} lost {:?} from the label",
                            w,
                            word
                        );
                    }
                    // Wide enough and the label is one line; otherwise its
                    // last line is what sits on the axis row.
                    let want = if tc::display_width(label) <= right - 2 {
                        label
                    } else {
                        label.split(" · ").last().unwrap()
                    };
                    let from: String = drawn
                        .last()
                        .unwrap()
                        .chars()
                        .skip(left + 2)
                        .collect();
                    assert_eq!(
                        from.trim_end(),
                        want,
                        "w={} the figure does not start at column {}",
                        w,
                        left + 2
                    );
                }
            }
        }
    }

    #[test]
    fn the_two_rows_line_up_with_one_another() {
        let p = palette();
        let days = chart_days();
        let series: Vec<(String, i64)> = days.iter().map(|d| (d.clone(), 2)).collect();
        for w in [58usize, 80, 120] {
            let one = day_chart(
                "OPENED / DAY",
                vec![],
                Some(&series),
                &p.pr,
                Some(9),
                false,
                FIGURE_LABELS[1],
                w,
                0,
                &p,
            );
            let two = day_chart(
                "MERGED / DAY",
                vec![],
                Some(&series),
                &p.ok,
                Some(9),
                false,
                FIGURE_LABELS[0],
                w,
                0,
                &p,
            );
            assert_eq!(one.len(), two.len(), "w={} unequal heights", w);
            // Identical series and identical figures, so the two blocks
            // differ only in their captions and their colours: every drawn
            // cell has to land in the same column.
            // The only difference left is which word the label starts
            // with, so it is normalised away before comparing.
            for (a, b) in one.iter().zip(&two).skip(2) {
                assert_eq!(
                    plain(a).replace("opened ·", "merged ·"),
                    plain(b),
                    "w={} the two charts do not line up",
                    w
                );
            }
        }
    }

    #[test]
    fn merge_activity_still_draws_when_nothing_is_open() {
        let p = palette();
        let days = chart_days();
        let counts = Counts {
            days: days.iter().map(|d| (d.clone(), 2)).collect(),
            merged_24h: 4,
            opened_24h: 9,
            at: tc::now(),
        };
        let rows = stats_view(&[], 0, false, Some(&counts), "", 80, 0, &p);
        let drawn: Vec<String> = rows.iter().map(|r| plain(r)).collect();
        let body = drawn.join("\n");
        assert!(
            drawn.iter().any(|r| r.contains("MERGED / DAY")),
            "empty board hid merge activity: {:?}",
            body
        );
        assert!(
            drawn.iter().any(|r| r.contains("merged")),
            "empty board lost the 24h figure: {:?}",
            body
        );
        for gone in ["STATE", "OPENED / DAY", "AGE"] {
            assert!(
                !drawn.iter().any(|r| r.contains(gone)),
                "pool section {} leaked onto an empty board: {:?}",
                gone,
                body
            );
        }
        let waiting = stats_view(&[], 0, false, None, "", 80, 0, &p);
        let wait_body = waiting.iter().map(|r| plain(r)).collect::<Vec<_>>().join("\n");
        assert!(
            wait_body.contains("MERGED / DAY"),
            "unfetched counts hid the row: {:?}",
            wait_body
        );
        assert!(
            wait_body.contains("loading") || wait_body.contains("counting"),
            "unfetched counts drew as zero: {:?}",
            wait_body
        );
    }
}
