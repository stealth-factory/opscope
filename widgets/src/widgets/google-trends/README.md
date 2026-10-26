# `google-trends`

[← all widgets](../../../../docs/README.md)

What a region is searching for right now — Google's own Trending Now feed,
shown as Google states it, with what changed since the last look.

```text
╺━ TRENDS US ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸ v1.9.0
 [US]·1  GB ·
 fetched 2m ago · Google's Trending Now feed, every 5 min

 TRENDING NOW
 Google's "approx traffic" is a bucket, not a count · 4 of 4 · 1 new since 14:05
  SEARCH              TRAFFIC STARTED   SEEN NEWS
▸ zorblax festival    5000+   13:10 65m 4×   Example News · Sample Gazette
  flumbo cup final    2000+   13:40 34m 3×   Example Times
  quux launch         500+    14:02 12m new  Sample Gazette
  widgetcon keynote   200+    12:15 2h  6×   Example News

 LEFT THE FEED
 this widget's own observation · gone since the fetch before, kept an hour
  old example term  was 1000+ · in 9 fetches over 40m · gone 7m ago
 ↑↓ select  tab section  ←→ region  [↵] news  [/] filter  [c] copy link
 [r] refresh  [,] settings  [q]uit
```

Every trend, bucket and story above is made up; the layout is the widget's.

The strip under the title is the regions: the open one is in brackets and
tinted, and `←`/`→` or a click on a tab switches to it. When the regions wrap
past the pane, the strip keeps the open tab on screen and leaves the feed a
row; the arrows still reach every region. The cell after each name says how
the last fetch went — `·` it succeeded, `!` it failed or would not parse (the
rows shown for that region are from the fetch before), `×` Google rejected
the region code outright (HTTP 400) — and is blank until the first fetch
lands. A number after the marker is how many trends are new
since the fetch before: the widget's own observation, the same count as
`N new since HH:MM` in the open region's header, absent when zero or on a
first check.

## The source

One request per region per poll to Google's public **Trending Now RSS feed**,
`https://trends.google.com/trending/rss?geo=<GEO>`. No account, no key, no
scraping. Nothing else is fetched.

Each item in the feed carries:

| on screen | from the feed | what it means |
|---|---|---|
| SEARCH | `<title>` | the search term, as Google writes it |
| TRAFFIC | `<ht:approx_traffic>` | Google's bucket — `200+`, `5000+`, `2M+` — **shown verbatim**. It is a band, not a count, and is never summed, ranked or turned into a number |
| STARTED | `<pubDate>` | when Google says the trend started, in local time, and how long ago |
| NEWS | `<ht:news_item_source>` | the outlets of the stories Google attaches, in feed order |

The list is in **Google's order and never re-ranked** — the order is part of
what the feed says.

## What the widget adds, and labels as its own

Google's feed says what is trending now and nothing about what was trending
before. Three things on screen are the widget's own observation, counted only
while it was running, and each one says so:

- **SEEN** is how many consecutive fetches *this widget* has seen a trend in.
  `new` means it was not in the fetch before; a trend that drops out and comes
  back starts again at `new`. On the very first fetch there is nothing to
  compare with, and the header says `first check` instead of `K new since`.
- **`K new since HH:MM`** counts the trends that were not in the previous fetch,
  which was at `HH:MM`.
- **LEFT THE FEED** lists trends that were in the previous fetch and are not in
  this one, with their last bucket, how many fetches they were seen in and how
  long ago they went. A departure stays listed for **one hour** (one fetch was
  too short to notice at a five-minute cadence) or until it comes back.

That history is kept per region in
`$XDG_STATE_HOME/opscope/google-trends/<GEO>.json` (falling back to
`~/.local/state/...`), and trends not seen for **seven days** are pruned. A
missing file is a first check. A file that will not read is replaced, and the
pane says the history started fresh rather than quietly resetting the counts.
A previous fetch more than three refresh intervals old - the widget was closed,
asleep or offline in between - is not compared with either: the widget cannot
say a trend ran on through the gap or when one left it, so that fetch is a
first check too, and the header says `previous fetch <age> ago, too long to
compare`.
Two panes watching the same region share that file and will each count a
fetch, so run one per region.

## When it cannot say

Every failure is a named row, never an empty table:

| what happened | what the pane says |
|---|---|
| Google answers `400` for the region code | `Google rejects geo "ZZ" (HTTP 400)` — fix `google_trends.geos` with `,` |
| `429`, `5xx` or no network | the last rows stay up behind `fetch failed: …`, with their age and when it retries |
| the feed will not parse | `Google's feed would not parse:` and the parser's reason, with the line |
| the feed parses and is empty | `Google's feed for US has no items right now` — distinct from a failed fetch |
| the poller dies | `poller stopped:` and why, at the top of the pane |

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select a row; the cursor walks across sections |
| `tab` | jump to the next section: TRENDING NOW, LEFT THE FEED |
| `←` `→` | switch region (also a click on its tab) |
| `↵` | open the selected trend's news |
| `/` | filter by search term, story headline or news source; `↵` keeps it, `esc` clears it |
| `esc` | clear the filter |
| `c` | in the list: copy the trend's Google Trends **Explore** link (`trends.google.com/trends/explore?q=…&geo=…`) — copied, never fetched. In the news view: copy the selected story's URL |
| `r` | fetch every region now |
| `pgup` `pgdn` `home` `end` | scroll a screen, or go to either end |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time — the pane moves, the selection stays where it is |
| `,` | open settings |
| `!` | notices: read warnings not tied to a number, and dismiss them for this session |
| `q` | quit |

In the news view, `↑` `↓` select a story, `c` copies its URL, and `esc`, `←` or
`backspace` go back. Every headline, source and URL is shown whole; a long URL
wraps rather than being cut.

Copying uses OSC 52, so the terminal you are sitting at performs it, even over
SSH. Where the terminal will not, the link is printed on screen instead.

Clicking a row selects it, and clicking the selected row opens it — the same as
`↵`. Clicking a footer hint presses its key.

## Width

Columns are spent on content: at full width NEWS names the outlets; as the
pane narrows NEWS goes first, then SEEN, then STARTED. SEARCH and TRAFFIC
always stay, and a long term wraps onto more lines rather than being cut.

## Configuration

Section `google_trends`, edited with `,`:

| key | default | meaning |
|---|---|---|
| `geos` | `["US"]` | region codes as the feed's `?geo=` takes them, e.g. `"US"`, `"GB"`, `"JP"` |
| `refresh` | `300` | seconds between fetches; minimum 60 |

Region codes on the command line (`google-trends GB JP`) replace `geos` for
that run. There are no secrets in this section.
