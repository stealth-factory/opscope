# `google-analytics`

[← all widgets](../../../../docs/README.md)

Google Analytics 4 on a wall: is traffic normal right now, and did the last
change hurt key events?

```text
╺━ GOOGLE ANALYTICS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 Example Site (properties/123)   ←→ 1 of 2   realtime 9s ago   breakdowns 40s ago   reports 3m ago   live 15s
 quota realtime used 1.2k/14k h 31k/200k d

 ── RIGHT NOW ── last 30 min · GA realtime · hits land within seconds
 active users 71   baseline 48 · from 2 of 4 weeks   +48% ▲
 baseline = median active users at this minute of the week, from this widget's own samples
 per minute ▁▂▂▃▂▃▄▄▃▅▅▆▅▆█▇▆▅▅▆▇▇█▇▆▅▄▃▃▂
 views last full minute 18   views 30m 540   key events 30m: purchase 3 (base 2)   sign_up 1 (base 1)
 PAGE (title)               NOW  DEVICE                     NOW  COUNTRY                    NOW
 Pricing                     34  mobile                      52  United States               41
 Home                        19  desktop                     16  Germany                      9
 sources: not in GA realtime - see TODAY below (2–6 h behind)

 ── TODAY VS SAME WEEKDAY ── sessions by hour · GA says 2–6 h behind, not final for 24–48 h
 today    ▂▁▁▁▁▂▃▅▆▇█▇▇▆▆░░░░░░░░░
 last Tue ▂▁▁▁▁▂▃▄▆▇▇▇▇▆▆▆▅▅▄▃▃▂▂▁
 ░ = hour not yet processed, not zero · · = no sessions that hour
 to 15:00: today 1,842  last Tue 1,690 +9%  3-week median 1,710 +8%
 top sources today: google / organic 1,120 (+6%) · (direct) / (none) 410 (+15%) · newsletter / email 86 (−40%)

 ── KEY EVENTS ── today to 15:00 vs same weekday · same lag as above
 EVENT                         TODAY LAST TUE  Δ            RATE/SESSION  LAST 7 D
▸purchase                         23       31  −26% ▼       1.2% (1.8%)   ▅▆▅▇▆▇░
 sign_up                          41       38  +8%          2.2% (2.2%)   ▃▄▃▅▄▅░

 ── TOP ORGANIC LANDING PAGES ── Google Search visitors · sessions · last 7 days vs prior 7
 GA4 does not give the search terms - Google withholds them as "not provided" and the Data API has
 no Search Console query dimension. The google-search-console widget shows the terms and the page
 each lands on.
 LANDING PAGE                                         SESSIONS  KEY EV  Δ
 /pricing                                                1,204      19  +12%

 ── MOVERS ── views · last 7 days vs prior 7 · by change · pages 1-8 of 212
 PAGE                                          7D    PRIOR  Δ
 /pricing                                   9,120    6,480  +41% ▲
 /docs/getting-started                      2,210    2,890  −24% ▼
 /blog/launch                                 890        0  new
 ↑↓ row  tab section  ←→ property  ↵ detail  [m]etric users  [w]indow 14d  [s]ort size  [/]filter
 [l]ive 30s  [c]opy URL  [r]efresh  [,] settings  [q]uit
```

Every figure is Google's - realtime, processed reports, or the property's own
quota - except the realtime baseline, which is **numbers this widget measured
itself** and wrote to its sample file. GA keeps no realtime history and its
processed reports run hours behind, so no API can answer "is now normal";
the widget has to have been watching.

## Sections

**Header.** The property, how old the realtime totals, the realtime
breakdowns and the reports are, the
realtime interval in force and why it is not the one chosen, and the
property's quota (`propertyQuota`) for whichever of the realtime and core
quotas is nearer its limit, by hour and by day. A poller that stopped, a key
file others can read, a pasted token that will expire, and every refusal from
Google are rows here, in Google's own words.

**Right now** - GA realtime, the last 30 minutes. Active users against the
baseline; a 30-slot per-minute series; views in the last full minute and over
30 minutes; key events in the window, each against its own baseline; top
pages (by title - realtime has no page path), devices and countries. Realtime
has no traffic-source dimension, and the section says so rather than leaving
a gap.

**Today vs same weekday** - processed sessions by hour, today drawn over the
same weekday last week. An hour GA has not processed yet is hatched `░`,
never drawn as zero; a processed hour with no sessions is `·`. The totals are
cumulative to the last processed hour on both sides, against last week and
against the median of the last three weeks (four date ranges is the most one
request takes), so a half-processed day is never set against a whole one.
Top sources and channels follow with the same comparison.

**Key events** - one row per key event: today to the last processed hour,
the same weekday last week to the same hour, the change, the rate per
session for both, and the last seven days with today hatched. Three states
read differently: *no key events configured*, `0` today beside last week's
figure with `none today`, and `░` / `not processed` while GA has not processed
any of today.

**Top organic landing pages** - the pages Google Search visitors land on
(`landingPage` where `sessionDefaultChannelGroup` is `Organic Search`), with
sessions, key events and the change against the previous window. GA4 does
not give the search terms themselves: Google withholds them as "not
provided", and the Data API has no Search Console query dimension. The
`google-search-console` widget shows the terms and the page each lands on.

**Movers** - pages, then channels, then sources, comparing the window with
the equal window before it, sorted by absolute change (or size, or
percentage), with `new` and `gone` rows. When GA folds rows into `(other)`,
withholds rows below its privacy thresholds, samples the data, or returns
fewer rows than it counted, the section says so in one line.

**Properties** (when more than one is shown) - one row each: active users
now, the change against the baseline, sessions and key events today against
the same weekday, and the poller's state.

## The baseline

The median active users at this minute of the week over the last
`baseline_weeks` weeks, taken from samples this widget records at every
realtime reading in `$XDG_STATE_HOME/opscope/google-analytics/<property>.samples`
(`~/.local/state/…` when the variable is unset), pruned past
`baseline_weeks + 1` weeks.

The widget does not run around the clock, so the file has gaps, and the
baseline is built for them:

- **Only weeks that were recorded count.** A past week counts only when it has
  a sample within five minutes of this minute of the week, in the property's
  timezone - so a week across a clock change still compares 14:35 with 14:35.
- **Coverage is on screen**: `baseline 48 · from 2 of 4 weeks`.
- **No week covering this minute** says
  `no baseline for Tue 14:35 yet - the widget was not running then`, never a
  made-up figure.
- **Under a week of samples** says `baseline: collecting since <date>`; a
  history shorter than `baseline_weeks` adds `collecting since <date>` beside
  the figure it has.
- **The coarser fallback**, whenever there is no realtime baseline, sets
  today's last processed hour against the same hour on the same weekday last
  week - labelled as hourly processed data, not realtime.

Key events in the realtime window are sampled the same way, so each gets its
own baseline - but as readings of their own, written when the once-a-minute
event breakdown arrives and stamped with that time, not copied onto every
totals reading. A reading that did not ask for events is not a reading of
zero events, so a week counts toward an event's baseline only if events were
read near that minute. Two panes on one property share the file; appends and
the daily prune take a lock beside it, and the prune works from what is on
disk, so neither pane drops the other's readings.

## Detail views

`↵` opens the selected row:

- **a page** (a mover or an organic landing page): 28 days of views and users
  by day with the previous 28 days drawn under them, and the page's sources,
  countries, devices and key events. Realtime names pages by title, not path,
  so a page is not matched to a live count - the detail says so;
- **a key event**: by day over 28 days, by source today, and by page;
- **a property**: the full quota table for realtime and core reports,
  timezone and currency, whether each processed report was thresholded,
  sampled or folded into `(other)`, and where the baseline file is and how
  many weeks it holds.

Channels and sources have no detail. Details are fetched on demand and asked
for again after five minutes or on `r`. `esc` goes back.

## Keys

| Key | Action |
|---|---|
| `↑` `↓` `PgUp` `PgDn` `Home` `End` | move between rows |
| `tab` | jump to the next section with rows |
| `←` `→` | previous / next property |
| `↵` / `Enter` | open the selected page, key event or property |
| `esc` | close a detail view |
| `m` | metric for the page movers: views → users → sessions |
| `w` | window: 7 → 14 → 28 days |
| `s` | movers sort: change → size → percent |
| `/` | filter rows by text - `enter` keeps it, `esc` clears it |
| `l` | realtime interval: 15 → 30 → 60 s |
| `c` | copy the property's GA realtime URL (OSC 52) |
| `r` | refresh every reading now |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time - the pane moves, the selection stays where it is |
| `,` | open settings |
| `q` | quit |

Each footer hint names what the next press moves to: `[l]ive 30s` while
realtime is every 15 s, `[w]indow 14d` while the window is 7 days. What is in
force is in the body - the interval in the header, the window and sort in
the movers heading, and a filter as `n of m shown · /text`.

## Width

Under 70 columns the realtime breakdowns stack into one column and the
movers drop PRIOR; above 120 the movers gain a 14-day sparkline. Names that do
not fit their column wrap onto the next line. Nothing is cut.

## Quota

Every request asks for `returnPropertyQuota`, and the header shows what is
used of each bucket. Google's answer gives only what the one request cost
and what is left after it, so the bucket's size comes from Google's
documented limits (200k a day, 40k an hour, 14k an hour per project for a
standard property, ten times that for Analytics 360), and *used* is that
limit less what is left - everything the property has spent, by every app
reading it. At the defaults a property costs about 720 realtime requests and 24
batches of reports an hour - roughly 0.8-2.5k of GA's 14k tokens an hour -
which is why the widget reads the quota and slows down: under a tenth of a
token bucket left, realtime goes to 120 s and reports to twice their
interval. A 429 names the bucket and when it refills, pauses that kind of
request for five minutes, and leaves the last data on screen under the
banner.

## Configuration

```json
"google_analytics": {
  "credentials_file": "~/.config/opscope/google-key.json",
  "token": "",
  "token_env": "GOOGLE_OAUTH_ACCESS_TOKEN",
  "properties": ["123"],
  "realtime_refresh": 15,
  "refresh": 300,
  "window_days": 7,
  "baseline_weeks": 4,
  "key_events": []
}
```

| key | default | |
|---|---|---|
| `credentials_file` | `""` | path to the service-account key; the screen warns if it is not `chmod 600` |
| `token` / `token_env` | `""` / `GOOGLE_OAUTH_ACCESS_TOKEN` | a pre-minted one-hour token, for testing (secret) |
| `properties` | `[]` | numeric GA4 property ids; empty lists every property the account can see |
| `realtime_refresh` | `15` | 15, 30 or 60 seconds; anything else means 15 |
| `refresh` | `300` | seconds between processed reports, at least 60 |
| `window_days` | `7` | movers and organic landing page window |
| `baseline_weeks` | `4` | weeks the realtime baseline is the median of |
| `key_events` | `[]` | event names to count; empty uses every event GA flags as a key event |

Setup on Google's side - the Cloud project, the APIs, the service account and
its Viewer access - is [Google setup](../../../../docs/google-setup.md). The
settings screen opens with `,`.
