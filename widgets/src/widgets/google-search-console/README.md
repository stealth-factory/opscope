# `google-search-console`

[← all widgets](../../../../docs/README.md)

Which pages and queries moved in Google Search, what people search to reach
the site, and which of the pages that earn the traffic are broken in the
index.

```text
╺━ SEARCH CONSOLE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sc-domain:example.com   site 1 of 2   fetched 4m ago   hourly to 11:00 (Google: a few hours behind)
 final to Oct 18   inspections 27/2,000 today

 ── LAST 24 HOURS ── · clicks by hour, hourly_all · local time; Google reports Pacific
 Google marks hours from 12:00 as still collecting
 clicks  ▂▂▁▁▁▁▂▃▅▆▇█▇▆▆▅▅▄▃▃▃░░░
 impr    ▃▃▂▂▂▂▃▄▆▇██▇▇▆▆▅▅▄▄▄░░░
         12:00                 11:00
 ░ still collecting or not reported · a complete hour with nothing
 21 complete hours:  1,284 clicks  same hours last week 1,190  +8%  impressions 38.2k vs 35.9k +6%

 ── MOVERS · PAGES ── · impressions · 7 complete days (Oct 12–18) vs prior 7 (Oct 5–11)
 final data, 2-3 days behind · 1-10 of 184
  PAGE                          IMPR    PRIOR       Δ   CLICKS      Δ   POS      Δ  INDEX
▸ /pricing                    41,200   29,800    +38%    3,120   +41%   4.1   ▲0.6  ok · crawled 2d
  /docs/getting-started       12,100   17,400    −30%      640   −35%   9.8   ▼2.1  ok · crawled 9d
  /blog/launch                 8,300        0     new      410    new   6.2      –  not inspected
  /docs/old-guide                  0    4,100    gone        0   gone     –      –  SOFT_404 · crawled 1d

 ── TOP SEARCH TERMS ── · top 25 query and page pairs by clicks · Oct 12–18
  QUERY                                   CLICKS    IMPR    CTR   POS      Δ
  example widget pricing                     312   4,410   7.1%   3.2   +12%
    → /pricing  +2 pages

 ── INDEX HEALTH ── · 25 top pages + 2 dropped, inspected daily · 27/2,000 today
 not the Page Indexing report (no API)
  FAIL     /docs/old-guide
            Submitted and indexed · SOFT_404 · crawled 1d ago · inspected 3h ago
  NEUTRAL  /de/pricing
            Duplicate, Google chose different canonical than user · Google chose /pricing
  PASS   23 pages   crawled within 9d

 ── SITEMAPS ── · 1 shown
  /sitemap.xml (index)
    1,214 urls · read 2d ago · 0 errors · 3 warnings · not pending
 ↑↓ select  tab section  ←→ site  ↵ open  [m]etric clicks  [w]indow 28d  [d]ata fresh
 [t]ype image  [i]nspect  [/]filter  [c]opy  [r]efresh  [,] settings  [q]uit
```

That picture is maintained by hand and every value in it is made up. On a
real pane every number is Google's.

## Where the numbers come from

All of it is the [Search Console API](https://developers.google.com/webmaster-tools/v1/api_reference_index),
read with the `webmasters.readonly` scope through the shared Google sign-in
(`core/src/google.rs`; [Google setup](../../../../docs/google-setup.md)).
A round asks, per property:

| request | what for |
|---|---|
| `searchAnalytics.query`, `dimensions: ["hour"]`, `dataState: hourly_all` | LAST 24 HOURS, and the same hours a week earlier from the same answer |
| `searchAnalytics.query`, `dimensions: ["date"]`, `dataState: all` | where final data ends (`first_incomplete_date`), and the property's own daily totals for SITES |
| `searchAnalytics.query` by `page`, then by `query`, each for both windows, `rowLimit` 1000 | the movers - page and query are never grouped in one movers request, which Google says costs more |
| `searchAnalytics.query` by `query` and `page`, both windows, `rowLimit` 25 | TOP SEARCH TERMS |
| `sitemaps.list` | SITEMAPS |
| `sites.list` | which properties there are, and the permission on each |
| `urlInspection/index:inspect` | INDEX HEALTH, bounded as below |

That is ten requests per property per refresh (every 600 seconds by
default), against Google's 1,200 a minute per property. Opening a page or a
query asks two more, once.

## Sections

**LAST 24 HOURS** is as close to live as the API gets. Hours at or after
Google's `first_incomplete_hour` are drawn hatched (`░`) and labelled *still
collecting*, so an hour Google has not finished counting never reads as a
drop. A complete hour with no clicks is a dot (`·`), which is a real zero.
The comparison uses complete hours only, against the same hours seven days
earlier from the same answer; when Google's lookback (about ten days) does
not reach back that far, it says so instead of comparing. Google reports
hours in Pacific time; they are drawn in this machine's local time.

A property with no hourly rows at all - usual for a new one - says so in
words, which is a different screen from a complete hour with nothing in it.

**MOVERS · PAGES** compares a window of complete *final* days ending at the
latest final date with the equal window before it, sorted by the size of the
change in either direction, with `new` and `gone` rows. Impressions by
default; `m` cycles impressions, clicks, CTR and position. Position falls as
it improves, so `▲` means it got better. The `INDEX` column (wide panes) is
the page's last inspection. When either window hit the 1,000-row limit the
header says `top 1,000 of more`.

**MOVERS · QUERIES** is the same for queries. Google leaves rare and
anonymised queries out of query rows, so they add up to less than the
property's totals, and the header says so.

**TOP SEARCH TERMS** is the queries bringing the most clicks, each with the
page it lands on: the top 25 query-and-page pairs, grouped by query, with
clicks, impressions, CTR, position and the change in clicks against the same
pair in the prior window's top 25 (`–` when the pair was not in it, which is
not the same as zero). When a query lands on more than one page the others
show as `+N pages`, and `↵` lists them - often a sign of pages competing
for one query. GA4 cannot supply search terms, so this is the one place in
the collection that shows them.

**INDEX HEALTH** is built from URL Inspection, because **the API has no
Page Indexing (Coverage) report**. The header says what was inspected - the
top `inspect_top` pages by impressions plus as many pages that dropped to
zero - and how much of the day's budget is spent. Rows that are not `PASS`
name Google's own values (`coverageState`, `pageFetchState` such as
`SOFT_404` or `BLOCKED_ROBOTS_TXT`, `indexingState`, a canonical Google chose
over the page's own) and the crawl age; `PASS` rows collapse to a count. A
URL Google refuses to inspect - a page outside the property is a 400 - keeps
Google's refusal on its own row.

**SITEMAPS** gives each sitemap's URL count, how long since Google read it
(`lastDownloaded`; flagged after 7 days), errors, warnings and whether it is
pending.

**SITES**, when there is more than one property: each one's own clicks,
impressions and position against the prior window (Google's property totals,
not a sum of rows), pages not passing inspection, and sitemap errors.

## Inspections and the budget

Google allows 2,000 inspections a day per property, on Pacific days. This
widget inspects each page in its set at most once a day and keeps the answers
for 14 days in
`$XDG_STATE_HOME/opscope/google-search-console/<property>.inspections.json`
(falling back to `~/.local/state`). The count against 2,000 is this widget's
own - Google does not report usage - and stops at the limit. Once it is spent
the section says *budget spent* and every row shows how long ago it was
inspected, rather than passing an old answer off as fresh.

`↵` never spends an inspection; it shows the last one. Only `i` does, on the
selected page, and it is refused when the budget is spent or `inspect_top` is
0.

## Fresh data

`d` switches the movers and terms from `final` to `fresh` (`dataState: all`),
which reaches the last day with any data. The status line and every header
then say *Google may still change this*. `d` again goes back.

## Detail view

`↵` opens the selected row:

- **a page**: its numbers in both windows, 28 days of clicks, impressions,
  CTR and position, its top queries, and its full last inspection - verdict,
  coverage, fetch, indexing, robots.txt, crawl time and crawler, both
  canonicals, the sitemaps and pages Google found it through, and rich-result
  issues. `l` copies the page's Search Console inspection link;
- **a query**: its 28-day series and the pages ranking for it;
- **a search term**: the pages it lands on in the top pairs, then the same as
  a query;
- **a sitemap**: its counts by content type and, for an index, its child
  sitemaps;
- **a property**: the permission level `sites.list` gives the account, the
  inspections used and left today, the hourly lookback Google actually
  returned, and where final and fresh data end.

## What it says when something is wrong

| on screen | means |
|---|---|
| `no credential: set google_search_console.credentials_file …` | nothing to sign in with |
| `cannot read the key file …`, `… is readable by others; chmod 600 it` | the key file, worded by core |
| `Google refused the sign-in: invalid_grant - …` | Google's own description; a clock hint when it is about `iat`/`exp` |
| `API not enabled: turn on searchconsole.googleapis.com …` | the Search Console API is off in the key's Cloud project |
| `403 no access: add <email> as a Restricted user on <property> …` | the service account is not a user on the property |
| `<property> is not among the N properties this account can see …` | a configured property the account cannot see, with the form it should take when it is written wrong |
| `siteUnverifiedUser: …` | the account is an unverified user, who gets no data |
| `429 quota: <bucket> exhausted (<when it refills>)` | a quota bucket is empty |
| `showing the reading from 14m ago, because the newest failed: …` | a refresh failed; the last reading stays, behind that banner |
| `no hourly rows yet …` | Google has no hourly data for the property |
| `every hour in the last 24 is still collecting …` | nothing complete to compare |
| `no final days yet …` | a new property with no finalised day |
| `top 1,000 of more` | Google's row limit cut a movers window |
| `budget spent …` | today's 2,000 inspections are used |
| `the poller stopped: …` | the background reader died, and why |

## Keys

| key | does |
|---|---|
| `↑` `↓` | select a row (`k` `j` too) |
| `tab` | jump to the next section |
| `←` `→` | switch property, when there is more than one |
| `↵` | open the selected row; a second click on a selected row does the same |
| `m` | cycle the movers metric: impressions, clicks, CTR, position |
| `w` | window: 7 or 28 complete days |
| `d` | data: final or fresh |
| `t` | search type: web, image, video, news, discover |
| `i` | inspect the selected page now (spends one inspection) |
| `/` | filter rows by text; `esc` clears it |
| `c` | copy the selected URL, query or sitemap |
| `r` | refresh now; in a detail view, ask again for its data |
| `l` | in a page's detail, copy its Search Console link |
| `esc` | close a detail view (`←` too) |
| `,` | settings |
| `q` | quit |

The footer names what the next press of `m`, `w`, `d` and `t` moves to;
the status line and the section headers say what is in force. The mouse
wheel, `ctrl-y` and `ctrl-e` scroll the view and never move the selection;
`pgup` `pgdn` scroll by a page and `home` `end` select the first or last row.

## Configuration

The section is `google_search_console`. Press `,` for the settings screen.

| key | default | |
|---|---|---|
| `credentials_file` | `""` | service-account key file, the same one `google-analytics` uses |
| `token` / `token_env` | `""` / `GOOGLE_OAUTH_ACCESS_TOKEN` | a pasted token for an hour of testing |
| `sites` | `[]` | `sc-domain:example.com` or `https://example.com/`; empty reads every property the account can see |
| `refresh` | `600` | seconds, at least 120 |
| `window_days` | `7` | complete days per movers window |
| `inspect_top` | `25` | 0-200; 0 turns inspection off and the section says so |
| `search_type` | `web` | web, image, video, news or discover |

Out of scope: anything that writes (sitemap submission included), Core Web
Vitals, Links, Manual Actions and Security Issues (none has an API), the
aggregate Page Indexing report (no API), and other search engines.
