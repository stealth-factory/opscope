# `usage.py`

How much the coding agents on this machine have actually been used — one tab
per agent, from each agent's own local state, plus a live quota reading for
the four that publish one and a subscription for the five that do.

```
╺━ AGENT USAGE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 local state · live quota · read 16s ago   · = installed
 [CLAUDE]· CODEX · CURSOR · GROK · COPILOT ·

 ── QUOTA ── live · account-wide, not this machine   max
 session 5h ███████░░░░░░░░░░░░░░░░░░░░  25%  resets in 3h 14m
 overall 7d ███████████░░░░░░░░░░░░░░░░  41%  resets in 15h 24m
 Fable 7d   █░░░░░░░░░░░░░░░░░░░░░░░░░░   3%  resets in 15h 24m
  extra usage 0.00 of 50.00 AUD monthly

 ── SUMMARY ── all time · since 2026-07-16
 Favorite model  opus-5      Total tokens    20.6B
 Sessions        31          Longest session 4d 10h 52m
 Active days     28/30       Longest streak  21 days
 Most active day Aug 1       Current streak  4 days
  Input 891.2k · Output 53.6M · Cache read 20.1B · Cache written 501.6M

 ── BY MODEL ── output tokens
  opus-5                27.9M ████████████████████████████
  opus-4-8              15.0M ███████████████░░░░░░░░░░░░░
  fable-5                4.2M ████░░░░░░░░░░░░░░░░░░░░░░░░
  sonnet-5               3.0M ███░░░░░░░░░░░░░░░░░░░░░░░░░

 ── MESSAGES / DAY ── 26d · peak 6,216
                                ██
   ▃▃          ▁▁            ▇▇██▁▁▄▄
 ▃▃██▃▃▅▅▆▆▄▄▄▄██▂▂▂▂▆▆██████████████
 ─────────────────────────────────────
 07-16                           08-12

 ── TOKENS / DAY ── peak 3.3B on 08-01
      07-13 07-20 07-27 08-03 08-10
 Mon    ·     ░░    ▒▒    ▒▒    ·
 Tue    ·     ░░    ▒▒    ░░    ░░
 Wed    ·     ░░    ░░    ▒▒    ░░
 Thu    ░░    ░░    ░░    ░░    ·
 Fri    ▒▒    ▒▒    ▒▒    ░░    ·
 Sat    ░░    ░░    ██    ░░    ·
 Sun    ·     ░░    ▒▒    ░░    ·
  less ░░▒▒▓▓██ more

 ── SUBSCRIPTION ── max
  member since     8 Mar 2024 · 2.4y ago
  status           active
  rate limit tier  default_claude_max_20x
  billing          stripe subscription
```

## The `+` tab

The first tab is not an agent. It is every quota all of them publish, on one
screen, **ranked by what is spent** rather than grouped by who it belongs to:

```
 ── QUOTAS ── 14 limits across 6 agents · ranked by usage
  COPILOT
   premium reqs  █████████░░░░   71%  -20%  15d 4h
  CODEX
   7d            ████░░░░░░░░░   29%  +23%  3d 8h
   Spark 7d      ░░░░░░░░░░░░░    0%        6d 23h
  GROK
   credits       ███░░░░░░░░░░   23%  +44%  2d 6h
  CLAUDE
   session 5h    █░░░░░░░░░░░░  5.0%  +57%  1h 54m
   overall 7d    ░░░░░░░░░░░░░  1.0%   +2%  6d 18h
```

It is deliberately not a concatenation of the other tabs. Those answer *how am
I using this agent*; this answers the only question that spans them — **what
runs out first** — so the pace column matters more here than anywhere else. The
line worth finding is the negative one.

Lanes are **grouped by provider**, but the ordering still does the work: groups
are sorted by their own worst lane, and so are the lanes inside them. Structure
says who owns what; the ordering keeps answering what to worry about. Dropping
the agent column that a flat list needed also bought back enough width for the
resets to survive a 58-column pane.

An agent that publishes no quota is **named at the bottom**, not silently
missing, so an empty row and an absent agent are different things.

A lane whose reading came from a cache rather than a live call says `cached`
instead of a countdown. Claude's fallback can describe windows that have since
closed, and a passed reset rendered as `resetting` beside five live ones would
be the summary quietly disagreeing with the tab it summarises. The marker is
never dropped for width, unlike the reset itself — a number nobody labelled as
old reads as current.

The tab carries no `·`: that dot means *installed*, and this one is not an
agent to install.

## Why tabs

Because the agents do not agree on what "usage" means. One counts tokens,
another counts the lines it wrote, and several publish nothing at all outside
their own session. A single table would need a shared schema that does not
exist; the columns would be mostly empty and the empties would look like zeros.

A tab per agent lets each show its own shape — and lets an agent that exposes
nothing **say so**, which is the honest answer and more useful than a blank
gauge.

`←` `→` or `tab` switch. The active tab is bracketed as well as tinted, so it
reads without colour. A `·` marks an agent that is installed.

## What each tab can actually show

**Claude Code** — the real one. `~/.claude/stats-cache.json` carries per-model
token counts (input, output, cache read, cache written), total sessions and
messages, and around four weeks of daily activity. All of it is spend.

The summary block mirrors Claude Code's own `/stats`, from the same file:
favourite model by output tokens, total across all four token kinds, sessions,
longest session, active days, both streaks and the most active day. Rendering
it against a `/stats` screenshot taken the same week agrees on every figure the
cache had settled — 31 sessions, a longest session of `4d 10h 52m`, a longest
streak of 21 days, Aug 1 as the busiest day.

The **tokens-per-day calendar** is laid out like the contribution calendar in
`github.py` — weekdays down the side, weeks across — so the two read the same
way on one wall.

The only difference is cell width. That pane spans a year, so its cells are one
character and its columns go unlabelled; there is no room for fifty-two dates.
Four weeks of retained history can afford wider cells and a date over each
column, and the widget picks the cell width from the pane. Intensity is carried
by the shading glyph as well as the colour, and a `·` marks a day the file has
no entry for — distinct from a day that recorded zero.

The **quota block** answers a different question from everything below it —
what is *left*, account-wide, rather than what this machine spent — and it is
the same set of windows Claude Code's own `/usage` shows.

```
GET https://api.anthropic.com/api/oauth/usage
Authorization: Bearer <accessToken from ~/.claude/.credentials.json>
```

The lanes come from the response's **`limits`** array, not from the top-level
keys beside it. That array is the server's own curated list; the rest of the
response carries a dozen mostly-null pools with names like `nimbus_quill` and
`iguana_necktie` that `/usage` does not render either. Each entry names itself
— `kind`, `group`, `percent`, `severity`, `resets_at` and a `scope` — so a
model-scoped weekly limit arrives labelled **Fable** without this code knowing
that name, and an Opus-scoped one would appear the same way.

`is_active` marks the limit that will stop you first, and that lane is the one
drawn brightly. A `severity` other than `normal` is printed as a word beside
the reset, because a colour alone cannot say *why* a bar is red.

**The fallback is where the care went.** Claude Code caches the same structure
in `~/.claude.json` under `cachedUsageUtilization`, with a `fetchedAtMs`. It is
used when the token has expired or the call fails — but it is labelled `cached
10h ago`, and any window whose reset has already gone by says **`already
reset`** instead of counting down. A stale five-hour window otherwise describes
a period that has ended, which is precisely the kind of number this repo exists
not to draw. Measured here, the cache read 11% while the live call read 22%.

The `extra_usage` line is the monthly credit allowance and its currency, shown
only when it is enabled.

**Cursor** — both quota and authorship.

The quota is the same three lanes `cursor-agent`'s own in-session Usage view
shows — included, auto, api — plus spend against the plan limit and the billing
cycle reset. It is **not** the documented `cursor.com/api/usage-summary`: that
one wants a browser cookie and returns 401 to everything this machine holds.
The CLI instead speaks Connect to

```
POST https://api2.cursor.sh/aiserver.v1.DashboardService/GetCurrentPeriodUsage
Authorization: Bearer <accessToken from ~/.config/cursor/auth.json>
```

which is the credential the widget reuses. That endpoint is **undocumented**,
discovered by reading the CLI bundle, and versioned only by it — so every
failure is silent and the tab falls back to authorship alone.

**The percentages and the dollars have different denominators**, which is
Cursor's own doing and worth stating. The three lanes are the server's
`totalPercentUsed` / `autoPercentUsed` / `apiPercentUsed` verbatim; the spend
line is `totalSpend` against `limit`. On this account those read 2% and
`$48.80 of $400.00` — which is 12% — at the same moment.

The lanes match what `cursor-agent` itself draws: its bundle computes each bar
as `percentage !== undefined ? percentage : used/limit*100`, and since the
server sends every percentage, the fallback never fires. The response also
carries a `displayMessage` — *"You've used 12% of your included usage"* — which
is the spend figure in a sentence. Both numbers are real and neither is
rewritten here; the spend line stays in dollars rather than becoming a fourth
bar, so 12% and 2% are never put on one scale.

`GetAggregatedUsageEvents` on the same service supplies a **spend** section:
per-model input, output and cache tokens with Cursor's own `totalCents` — not
an estimate — over the last 30 days. It is what the plan percentages are made
of, and answers which model actually spent the money.

`ai-tracking/ai-code-tracking.db` supplies the authorship half: how many edits
the agent made, across how many conversations, and how many lines in scored
commits came from the agent rather than by hand. A different question from
cost, and labelled as such.

**Codex** — real, and it took a second look to find. `~/.codex/logs_2.sqlite`
is diagnostics with no counters, which is where the first search stopped. The
answer is in `~/.codex/sessions/**/*.jsonl`: every rollout carries
`event_msg / token_count` events with both a running total and the last turn's
usage, timestamped.

That gives totals *and* an **output rate** — output tokens divided by the
wall-clock gap between turn boundaries. On this machine: 659.9M input, 1.3M
output across 28 sessions, and a median of 30 tok/s in the newest one.

The running total is repeated on every event, so only the tail of each rollout
is read — some are 30MB and re-reading them every refresh to learn a number
printed at the end would be daft.

**Copilot** — it *does* keep usage locally, in `~/.copilot/session-store.db`.
The `assistant_usage_events` table carries per-turn input, output, cache and
reasoning tokens, AI credits as `total_nano_aiu`, a `request_multiplier`, and —
uniquely among these agents — `duration_ms`, `time_to_first_token_ms` and
`inter_token_latency_ms`. It is the best-shaped usage data of the lot.

It sat **empty** here for a long time — two sessions, zero turns — and now it
is not: two turns, 60.8k in, 701 out, 9.211 AI units, and a measured 4,370 ms
to first token. It really is the best-shaped data of the lot.

**The quota half does not depend on it**, which is the point that took a second
look. Copilot's remaining allowance is not in the session store at all; it is on
the account, and the CLI reads it from

```
GET https://api.github.com/copilot_internal/user
Authorization: token <copilotTokens from ~/.copilot/config.json>
```

That file is JSON with `//` comments at the top, so it has to be stripped
before parsing, and the tokens are keyed by host and login because one machine
can be signed in to github.com and an Enterprise host at once.

The response carries `copilot_plan`, `quota_reset_date` and a
`quota_snapshots` object — one entry per pool, each naming itself. On this
account: **premium interactions 7,103 of 10,000 used**, with chat and
completions `unlimited`.

An unlimited pool gets **no bar**. It has no denominator, and drawing one as an
empty gauge would invent the limit the field explicitly denies. The pools are
rendered from the list, so a new one appears without an edit, and the metered
ones sort first — an unlimited pool is not news.

The API reports `percent_remaining`; the pane shows what is *spent*, like every
other tab, so that red always means the same thing across the wall.

**The window is derived, and only when it is safe to.** Copilot says when the
quota resets and never how long the window is. `quota_reset_date_utc` lands on
`2026-09-01T00:00:00.000Z` — midnight UTC on the first of a month — and that
shape is what a calendar-month cycle looks like, so the span is worked back a
month and shown as `window 1 Aug → 1 Sep · monthly`. A reset that does *not*
land on a month boundary gets no window line at all, because then the cadence
genuinely is not known.

Use `quota_reset_date_utc`, never the bare `quota_reset_date`: a date with no
zone parses as local midnight, and the countdown then drifts by the machine's
UTC offset. This server runs UTC, so that bug would have sat here unseen.

Each pool also carries its own `quota_reset_at`. It is `0` on this account —
every pool is on the account-wide cycle — but when one is set and differs, that
lane prints its own reset instead of inheriting the header's.

**Antigravity** — a subscription, and no usage at all.

It leaves plenty behind in `~/.gemini/antigravity-cli`: a conversation store
per session, a prompt history, and logs. None of it counts a token. Each
conversation is its own SQLite file whose `steps` table records what the agent
did, so the tab reports conversations, agent steps and prompts — real work
done, but not cost.

Its quota never lands on disk, and getting it took a wrong turn worth
recording. The log shows `quota_manager.go` refreshing one on a loop into
memory, noting only that it happened. The binary — `~/.local/bin/agy`, 206MB
of Go — carries a plausible-looking remote method:

```
POST https://businessaicode.googleapis.com/v1beta/{parent=projects/*/locations/*}:fetchQuotaStatus
```

That is a dead end. Called with the CLI's own token it answers **404 from a
Google frontend**, and there is no `agy usage` subcommand to fall back on —
the TUI's display is a slash command.

**The quota was never a remote call to make.** The process already holding it
serves an RPC on loopback, which is how CodexBar reads it:

```
POST http://127.0.0.1:<port>/exa.language_server_pb.LanguageServerService/RetrieveUserQuotaSummary
```

The port is found by matching the running process — `agy`, the Antigravity
app, or a `language_server` — and reading its listening sockets straight out of
`/proc`. No `lsof`, no scanning a range. Note the **`http`**: the server's TLS
listener answers with a wrong-version error, so plain HTTP is not a downgrade
here, it is the protocol, and the request never leaves the machine.

```
 ── QUOTA ── live · account-wide, from the local language server
  Gemini Models
   weekly ░░░░░░░░░░░░░░░░░░░░░░░░   0%   +3%  resets in 6d 18h
   5h     ░░░░░░░░░░░░░░░░░░░░░░░░   0%  +14%  resets in 4h 15m
  Claude and GPT models
   weekly ░░░░░░░░░░░░░░░░░░░░░░░░   0%        resets in 6d 23h
   5h     ░░░░░░░░░░░░░░░░░░░░░░░░   0%        resets in 4h 58m
```

Shown as **spent**, not the `remainingFraction` the RPC returns, so red means
the same thing here as on every other tab — and with the same pace column.
Every plan reports every family it covers, so a Gemini-only account still gets
a Claude/GPT pair at 0%; those are real limits and are left in.

It is present only while Antigravity is running, which the pane does not
disguise: no process, no port, no section.

The tier comes from the endpoint the CLI authenticates against:

```
POST https://cloudcode-pa.googleapis.com/v1internal:loadCodeAssist
Authorization: Bearer <token.access_token from antigravity-oauth-token>
```

Two things about that call are worth writing down. Its access token expires
**hourly** — beside a refresh token that is deliberately left alone, as
Claude's is — so the section is simply absent between refreshes rather than
stale.

And the response is **gated on the client string**. Sent as plain
`terminal-toys` it returns no tier at all, just an `ineligibleTiers` entry
reading `UNSUPPORTED_CLIENT`. Sent as `terminal-toys (antigravity-cli)` it
answers properly — the parenthesised form names the client being spoken for
while still saying who is calling, which is the honest version of what would
otherwise be a plain impersonation.

What it returns is worth reading carefully: `currentTier` is `free-tier`
while `paidTier` is `Google AI Ultra`. Those are different questions — which
Code Assist tier this project sits on, and which Google AI plan the account
holds — and they disagree here, so **both are shown** rather than one being
picked as "the" plan. The explanation lives here rather than on the pane: it
is a paragraph, and a paragraph that never changes does not earn four lines on
every frame.

`free-tier` is not a downgrade, and the response says so itself. `allowedTiers`
offers exactly two: `free-tier`, marked `isDefault`, and `standard-tier`, which
carries `usesGcpTos: true` and `userDefinedCloudaicompanionProject: true`. The
paid Code Assist tier is **GCP licensing** — it wants Cloud terms accepted and
your own Cloud project nominated. This account is a consumer sign-in
(`auth: consumer`, `gcpManaged: false`) on an auto-provisioned project, so it
sits on the default, while the limits actually come from the consumer Ultra
subscription in `paidTier`.

One inconsistency is Google's rather than this pane's, and is left as it
arrives: `currentTier.upgradeSubscriptionText` still advertises *"Upgrade to
get 1,500 model requests per day"* with `upgradeSubscriptionType:
GOOGLE_ONE_HELIUM`, pitching a Google One upgrade to an account whose
`paidTier` already reads *"You are subscribed to the best Google AI plan."*

**Grok** — real too, and the third of these I first wrote off. `~/.grok/sessions/**/updates.jsonl`
carries a running `totalTokens` on each session event alongside an
`agentTimestampMs`. Differencing consecutive events and bucketing by the
event's own timestamp gives per-day figures; taking the running total alone
would credit a whole session to whichever day it was read on. That drives a
totals line and a calendar in a blue ramp.

The weekly quota is real but is **not** in the session transcripts: it arrives
on the client log at `~/.grok/logs/unified.jsonl`, as a `.ctx.config` carrying
`creditUsagePercent` and a `currentPeriod` of `USAGE_PERIOD_TYPE_WEEKLY`.
Looking only at `sessions/` is what made this tab say "no quota" for a while.
`grok du` reports **disk** use — the name is a coincidence worth not falling
for.

## Pace: how far ahead of the clock you are

Every quota bar carries a signed percentage after it.

```
 session 5h ██░░░░░░░░░░░░░░░░░░░░░░░░░   9%  +15%  resets in 3h 46m
 overall 7d ████████████░░░░░░░░░░░░░░░  43%  +53%  resets in 6h 46m
 premium interactions ██████████░░░░  71%  -22%
```

It is the share of the window already gone **minus** the share of the
allowance already spent. Positive is headroom: you are burning slower than the
clock and will reach the reset with room to spare. Negative means this runs out
before the window does — the Copilot line above is at −22%, which is the pane
saying those premium interactions will not last the month at this rate.

This is the quantity CodexBar calls **"in reserve"**, and it was worked out
from its own numbers before its docs were read: 98% left with 11% in reserve at
26d 23h remaining implies 13% of a 31-day cycle elapsed against 2% spent, and
all three Cursor lanes reproduced to the percentage point.

**The sign is deliberately the opposite of CodexBar's separate pace token**,
where `+X%` means burning *too fast*. The cushion reading is the one that
matches the phrase "in reserve", so the column is labelled in the header rather
than left to be guessed at.

Nothing is fetched for it — the window length and the reset are already on
screen — so it costs nothing on any tab. It is hidden for the first **3%** of a
window, because ten minutes into a week every number looks like a catastrophe
or a triumph. CodexBar gates it the same way, for the same reason: Codex's
Spark lane shows no pace at 0.6% elapsed, and that is correct.

## METERED: today, and the last thirty days

Every tab that can cost its tokens shows the same block — two windows, each
with its money, its tokens, and the models underneath.

```
 ── METERED ── at list prices · Aug 2026
  today    $645.26    688.8M tokens
             claude-opus-5    $635.17
             claude-opus-4-8   $10.09
  30 days  $13884.16  15.8B tokens
             claude-opus-5    $9640.29
             claude-opus-4-8  $2298.76
             claude-fable-5   $1931.03
             claude-sonnet-5    $14.08
  the plan saves  $13685.12
```

Two windows because they answer different questions: a month says what an
agent costs, today says whether that is still true. A single all-time figure
answered neither, and quietly flattered a habit that changed last week.

### Where the prices come from

Anthropic and OpenAI both **publish** their rate cards, so there is nothing to
invent. The tables are copied from
`platform.claude.com/docs/en/docs/about-claude/pricing` and
`developers.openai.com/api/docs/pricing`, with the sources and the date in the
source file and **the date on screen beside the total**. A published price is a
fact; what makes one dangerous is going stale in silence, and a date fixes
that.

OpenAI does not charge for cache writes, so those entries carry no
`cache_write` — inventing one would be worse than leaving it out. Two models
are listed as having **no published price at all**: `gpt-5.3-codex-spark`,
which is explicitly not on the API (`supported_in_api: false` in Codex's own
model cache), and `codex-auto-review`. Without that, prefix matching would hand
Spark `gpt-5.3-codex`'s rate — a number nobody published. They report as
unpriced instead, and are named.

### What each figure covers

The METERED header states its **scope** first and then spells it out in a
remark, because that is the thing most easily got wrong: the section sits
directly under a `QUOTA` labelled *account-wide*, and a local figure beside it
reads as the same scope unless it says otherwise.

```
 ── METERED ── this machine · at list prices · Aug 2026
  Counted from transcripts, which are written where the
  agent ran. Claude used on another machine, or on
  claude.ai, is not in here.
```

The remark names what is **excluded**, not just what is counted — "this
machine" alone leaves the reader to work out what that rules out, and the
answer differs per agent: another laptop for Claude, an editor or github.com
for Copilot, seven other surfaces for Codex.

| | scope | why |
|---|---|---|
| **Cursor** | account-wide | its numbers come from Cursor's API, which bills the account |
| **Claude** | this machine | transcripts are written where the agent ran |
| **Codex** | this machine, CLI only | rollouts are local, and only the CLI writes them |
| **Copilot** | this machine | the session store is local |

Codex's is the one worth spelling out, and the pane does. Its own dashboard
reports a 30-day total across **Desktop App, Desktop (Work), CLI, Cloud, Web,
Mobile, GitHub Code Review, Exec** and *Uncategorized* — and the account-wide
figure is several times what this pane can see, because every surface except
the CLI leaves nothing on this disk. On one account the dashboard read
`$3,400.37` where this pane read `$225.33`; both are correct, and they are
answers to different questions.

**That account-wide breakdown is not fetchable with a token**, and reading
CodexBar's source settles why rather than leaving it a guess. It gets those
numbers by **being a browser**: `OpenAIDashboardBrowserCookieImporter` lifts
session cookies out of WebView storage, `OpenAIDashboardFetcher` loads
`chatgpt.com/codex/cloud/settings/analytics#usage` in a hidden web view, and
`OpenAISubscriptionMetadata` injects JavaScript to intercept the page's own
`fetch('/backend-api/subscriptions')` — falling back to scraping the rendered
table when the API response is not enough.

A terminal widget cannot follow it there. Reaching into a browser's cookie
store is a different kind of program from one that reads an agent's own files,
and this repo is not going to become that quietly.

Probing the same endpoints with the CLI's OAuth token gives three different
answers, and the differences are the useful part:

| endpoint | with the CLI token |
|---|---|
| `wham/usage`, `wham/rate-limit-reset-credits` | **200** — quota, plan, credits |
| `backend-api/me`, `backend-api/subscriptions` | **403 HTML** — a bot wall, not an auth error |
| `accounts/<id>/spend-controls/current-user/monthly-usage` | **401 JSON**: *"Must use workspace account for this operation"* |

That last one is the interesting one: a real API answer rather than a wall, so
the endpoint **is** reachable by token — just not for a personal account. On a
workspace account it would plausibly return account-wide monthly spend. Nothing
here calls it, because there is no workspace account on this machine to see the
response shape, and writing a parser for a payload nobody has seen is how
plausible-looking wrong numbers get shipped.

### Where each agent's numbers come from

**Cursor** needs no rate card — it publishes both sides. The raw events carry
their own vendor-rate cents, `GetAggregatedUsageEvents` says what Cursor
actually metered, and the gap is the discount the plan applied. Its header
reads `at vendor rates · Cursor meters $762.64 of it`.

**Claude** is costed from the **transcripts**, not from `stats-cache.json`.
The cache does have `dailyModelTokens` — per day, per model — but only one
total per model per day, and input, output, cache reads and cache writes
differ in price by up to fifty times, so a total cannot be costed at all. The
transcripts carry the split, per message, with a timestamp and a model.

They carry more than that. A usage block's top-level counters can all read
zero while its `iterations` hold the real figures, so the iterations win where
they exist. And `cache_creation` splits `ephemeral_5m_input_tokens` from
`ephemeral_1h_input_tokens` — the two are priced differently, 1.25× input
against 2× — so both rates are carried and **neither duration is assumed**.

Two things about reading that corpus were learned the hard way, both found by
one question — *why is Haiku missing?*

**The glob has to recurse.** Subagent transcripts live two levels further down,
in `<project>/<session>/subagents/`, and that is where Haiku and most of Sonnet
actually run. A one-level glob found 38 files and silently skipped **257 more,
277MB of them** — so an agent that only ever appears in subagents vanished from
the costs entirely.

**Records have to be de-duplicated on `uuid`.** The same message appears in
more than one file: resuming or forking a session replays its history into the
new transcript, and subagent turns are written twice over. There were 38,612
duplicated `requestId`s here. Left raw, that inflated Fable by 29% and Opus 5
by 13%.

Both fixed, the transcript totals reconcile against Claude Code's own
`modelUsage` — Haiku to the token, Fable and Opus 5 within a point, Sonnet
within one, Opus 4.8 at 93% where older transcripts have been rotated away and
the cache still counts them. That agreement is the check that the extraction is
right; it is worth re-running after any change here.

The corpus is 520MB across 295 files. Each is parsed once and cached on
`(mtime, size)`, exactly as the Codex rollouts are, because a finished
transcript never changes. Cold, the whole set streams in about 2.6 seconds.

**Copilot** groups its own `assistant_usage_events` by day and model, which is
the same shape from a much smaller table.

**Codex** attributes them the hard way. The model is not on the token counts:
it arrives in a `turn_context` record, one per turn, and applies to the
`token_count` events that follow it, so the rollout is walked in order
carrying the model forward. Each event's `last_token_usage` is that turn's
delta; within it `cached_input_tokens` is the cheaper subset of
`input_tokens`, and the reasoning tokens are already inside `output_tokens`.

Reading it that way turned up **a counting bug in this widget's own totals**.
`total_token_usage` is cumulative for the *session*, and a session spans
several files — thirty rollouts here hold only **eight sessions** — so summing
one tail per file counted most of them two or three times over: 664.5M against
a true 370.0M for the primary model. The totals now come from the
de-duplicated per-turn deltas instead, which reproduce Codex's own cumulative
figure **exactly** on four of those eight sessions, and pick up the review
model besides, which the session total never included at all.

**Grok** records no model against its tokens at all, so it can only be priced
by a `"*"` entry.

### What the plan saves

Cursor computes it from its own two figures. Everyone else needs one number
that no machine here knows — what you actually pay — so it is configured:

```json
"usage": {
  "plan_cost": { "claude": 200 }
}
```

US$ per month, keyed by agent. **Nothing ships here either**, and for a
sharper reason than the rates: Anthropic lists Max as *"from $100"* because it
varies by tier, so there is no single published figure to embed even if one
wanted to. Set it and the block gains `the plan saves`, which is the month's
list cost minus what the month actually cost you. Leave it and the line is
simply absent.


## Spend per day

```
 ── SPEND / DAY ── 30d · peak $501 on Aug 5 · today $32.88
                                     ██
                                     ██              ▄▄
         ▁▁▆▆▁▁                ▁▁▂▂▁▁██▅▅▁▁    ▁▁  ▂▂██▅▅▁▁▁▁
 ────────────────────────────────────────────────────────────
 Jul 18                                                Aug 16
```

`GetAggregatedUsageEvents` totals by model and carries **no timestamp at all**,
so no per-day view can be built from it — which is why this took a second look.
`GetFilteredUsageEvents` returns the individual events, newest first, each with
a timestamp, a model and its cents, a thousand at a time. Paging stops as soon
as a page reaches past the window, so the cost is proportional to the window
rather than to the whole account: thirty days is five pages and about eleven
seconds. Held for **half an hour**, because that is far too slow to repeat on
a redraw.

It is a bar chart rather than the calendar the token tabs use. Thirty days in a
year-wide grid is six columns of colour in a field of dots; money over a month
reads better as a profile, and it is the shape Cursor's own dashboard draws.

## Which subscription, and since when

**Every tab ends with a `SUBSCRIPTION` section**, in the same shape and the
same place. A percentage without its plan is half a fact: an enterprise seat is
why two of Copilot's three pools come back unlimited, and Cursor's `$400.00`
limit means nothing until you know that is what Ultra includes for `$200/mo`.

It goes **last** rather than first because it is context for the whole tab, not
the headline. What is left of the quota, and what was spent, are what anyone
opens the pane to see; which plan those belong to is the footnote that makes
them legible, and it changes about once a year.

The section is appended in one place rather than by five tabs that each end
differently, which is also what keeps the blank line before it consistent —
some tabs already finish on a blank and would otherwise leave two.

| | where it comes from | what it says |
|---|---|---|
| **Claude** | `api/oauth/profile` | plan, member since, subscription status, rate-limit tier, billing type |
| **Cursor** | `GetPlanInfo` on the same Connect service | plan name, price, included amount, who bills it |
| **Copilot** | the same `copilot_internal/user` call | plan, seat date, organisation, sku, billing mode, enabled features |
| **Codex** | already in the usage response | plan type and credit balance — and that is genuinely all of it |
| **Grok** | the client log | tier, billing period, on-demand and prepaid balances |
| **Antigravity** | `loadCodeAssist` | Code Assist tier, Google AI plan, project, auth method — and no usage whatsoever |

Grok's tier moved out of its quota heading to join them, so no agent states
its plan in two different shapes.

Codex's section is three lines rather than six because three lines is all it
publishes. Its `approx_local_messages` and `approx_cloud_messages` read zero
here for a real reason — they are estimates derived from a credit balance, and
the balance is zero.

A **success** is held for an hour, not the two minutes the quotas get: a plan
does not change between refreshes, and the Claude usage endpoint answers `429`
if you ask at quota cadence — which it did, during testing.

A **failure is never held that long**, whatever the caller asked for. That
distinction had to be learned: one rate-limited profile call was cached for the
full hour and blanked Claude's subscription section for that hour, which looks
exactly like an agent that publishes nothing.

Claude's section also degrades rather than disappearing. With the endpoint
unreachable it falls back to `~/.claude/.credentials.json`, which needs no
network and always carries `subscriptionType` and `rateLimitTier`, and says
`from credentials` so it is never mistaken for the fuller reading.

## Empty tabs say two things and stop

An agent with no local data gets exactly two lines: what is missing, and the
command that fixes it.

```
 ── SPENT ── no local sessions

  Nothing recorded in the local session store yet.
  run copilot here and this fills in
```

They used to get a paragraph. Copilot's empty tab toured the schema it would
have used — the table, the column names, why it would have been the best data
here — which is interesting exactly once and is then a wall of text sitting
where the numbers should be. Two lines say as much, and the second answers the
only question an empty tab actually raises, which is *what do I do about it*.

Every tab uses the same two lines: `claude`, `codex`, `cursor-agent`, `grok`,
`copilot`. Antigravity gets the first line without the second, because there is
no command that would make it record tokens — it does not record them at all,
and saying "run this" would be a promise the tab cannot keep.

The text wraps rather than clipping, so a narrow pane loses no part of the
sentence.

## On wrapping rather than clipping

Long text **wraps**; charts and tables do not, and the difference is not
laziness.

A labelled value is words — `copilot_enterprise_seat_multi_quota` is one — so
it flows onto as many lines as it needs, continuation lines sitting under the
value rather than under the label. A single word wider than the column is split
rather than allowed to run off.

A bar chart broken across two lines is not a bar chart, and a table row wrapped
mid-row loses the columns that made it a table. Those **adapt** instead: columns
drop as the pane narrows, labels shorten, bars take whatever width is left.

Headers do a third thing again — they shed a *clause* before they will clip a
*number*. `· account-wide, not this machine` becomes `· account-wide` so that
`resets in 15d` survives, because losing the clause leaves a shorter true
sentence while losing two characters of the countdown leaves `resets in 1`,
which is a different and wrong number. That one shipped, and read exactly as
badly as it sounds.

## On tokens per second

Two agents can answer it, and they answer different questions.

**Codex** is computed here: output tokens over the wall-clock gap between turn
boundaries. That includes tool calls and thinking, so it is a *throughput*
figure and not raw decode speed — the pane says so under the chart rather than
letting the number imply more precision than it has.

**Copilot** would be exact, once there is data: `inter_token_latency_ms` and
`time_to_first_token_ms` are recorded per turn, so no inference is needed.

**Claude Code** is computed too, and getting it right took two attempts. A turn
is a `user` record followed by an `assistant` one, and the rate is that
assistant's output tokens over the gap between them. Measuring from *any*
previous record instead inflates it wildly — two assistant records can be
milliseconds apart while the second reports a whole turn's output, which
produced a maximum of 21,183 tokens/second.

Even with the right boundary a few gaps remain impossible — 1,073 tokens in
0.07 seconds among them — where the timestamps plainly do not bracket
generation. So **only the median and p90 are shown, never a maximum**: the
median sits at 74–75 however the outliers are trimmed, which is the reason to
trust it, while the maximum moves from 15,328 to 800 on the same data, which is
the reason not to publish one.

Transcripts run to tens of megabytes, so it samples the tail of the three most
recently touched.

**Cursor and Grok** record no tokens at all, so there is nothing to divide.

## Codex quota is the exception: it is real, live and account-wide

Every other number here is local consumption. Codex publishes an actual
**remaining quota**, and two ways to get it.

The rollouts record a `rate_limits` snapshot the server sends back with each
response — `used_percent`, the window length, and `resets_at`. That is real but
only as fresh as the last time Codex ran.

Better, and what the widget prefers: the same endpoint the Codex CLI itself
uses. Read the OAuth token from `~/.codex/auth.json` and

```
GET https://chatgpt.com/backend-api/wham/usage
Authorization: Bearer <token>
```

which returns `plan_type`, `credits`, and a `rate_limit` with primary and
secondary windows. The header says `live` or `from the last session` so it is
never ambiguous which you are looking at.

**Some features meter separately**, and arrive in the same response under
`additional_rate_limits` — each with its own `limit_name`, window and reset.
`GPT-5.3-Codex-Spark` is one: a second weekly allowance that the account-wide
percentage says nothing about, so spending all of one leaves the other
untouched.

```
 ── QUOTA ── live · account-wide, not this machine   pro
 overall 7d ████████░░░░░░░░░░░░░░░░░░░░  27%  resets in 4d 5h
 Spark 7d   ░░░░░░░░░░░░░░░░░░░░░░░░░░░░   0%  resets in 6d 23h
```

The list is rendered as it arrives rather than looking for the one name we
know, so a feature added later appears without an edit. A lane spells its
feature out when the pane is wide enough — `GPT-5.3-Codex-Spark 7d` — and falls
back to the last segment when it is not. `overall` labels the account-wide
lanes **only** when a named one sits beside them; alone, the window tells them
apart and the word would be noise.

These extra limits are **live-only**. The snapshot recorded in the rollouts
carries `primary`, `secondary` and `limit_name` but no `additional_rate_limits`,
so a fallback reading shows the account-wide windows and nothing else — which
is why the source label matters.

Spark is a *model* (`gpt-5.3-codex-spark`, "ultra-fast coding model", 128k
context, not on the API), and rollouts do record which model ran each turn in
`payload.model`. Nothing on this machine has used it, so its lane reads 0% —
a real zero, from the server, not an absent number drawn as one.

This method came from reading how [CodexBar](https://github.com/steipete/CodexBar)
does it — a menu-bar app that does this for twenty-odd providers, and documents
the endpoint. Its `codexbar-cli` would cover far more of them; it is not used
here because the widget stays dependency-free, but it is the obvious thing to
reach for if this ever needs to cover providers whose numbers are not on disk.

## The three network calls, and the rule they follow

Claude, Codex and Cursor each publish a live quota, and each is fetched with a
credential the agent itself already holds — read-only, sent **only to that
agent's own host**, never printed, and never refreshed or rewritten. Claude's
OAuth token sits beside a refresh token that is deliberately left alone:
spending it would race Claude Code's own credential handling for a number that
has a local cache anyway.

Every one of them falls back rather than failing: Codex to the rollout
snapshot, Claude to `cachedUsageUtilization`, Cursor to authorship alone. The
header always says which you are looking at.

A reading is held for **two minutes** (`LIVE_TTL`). The pane redraws every 30
seconds and these windows move over hours, so the earlier code was making six
requests a minute — three calls, twice a minute — to be told the same thing. A failure is cached too, so a dead
endpoint is retried occasionally instead of on every frame.

### Claude's fallback is our own snapshot, not Claude Code's

Claude Code keeps a usage cache in `~/.claude.json` under
`cachedUsageUtilization`, and this widget read it whenever the live call
failed. With no age check at all — which turned out to matter, because
Claude Code's own reader has one:

```js
let r = Date.now() - n.data.fetchedAtMs;
if (r < 0 || r > zNo) return null;      // zNo = 3600000
```

**It trusts that cache for one hour.** It writes it at most every five
minutes (`BNo = 300000`) and discards it past sixty. On the machine this was
found on, the entry was **9.6 days old** — Claude Code had been ignoring its
own cache for nine days while this widget drew it as a current percentage.
The block was byte-identical in a backup from the 16th, so it had not moved
since the 15th, while `~/.claude.json` itself is rewritten every few seconds.

Not a removal and not a regression: the key is present in every installed
version, `2.1.243` references it more than the older ones, and the reader is
the same logic in `2.1.233` and `2.1.243` with the same constant. Why the
writer stopped firing on the 15th is unestablished — it is gated on an
account match and fed from API response data, and finding out would mean
instrumenting Claude Code.

So there are two fallbacks now, newest wins:

1. **Our own snapshot**, written to `$XDG_STATE_HOME/terminal-toys/claude-usage.json`
   every time the live call answers — minutes old on a machine in use, and
   not dependent on another program's cache still being maintained.
2. **Claude Code's**, but only inside the hour it trusts it for.

CodexBar reached the same conclusion from the other end: its Claude sources
are the API and the CLI, never that file, and when they all fail it keeps
its own `history/claude.json` and shows the capture age rather than blanking
the bars.

The snapshot carries the `accountUuid` Claude Code records, so switching
accounts does not show the old one's figures — the usage response itself
carries no account, so the marker is borrowed. A machine whose Claude Code
never wrote the key has no marker, and then the guard is simply not applied.

### Grok is the fourth, and it is off by default

Grok publishes no quota this widget can read without asking for it. The other
five agents each answer a host — `api.anthropic.com`, `chatgpt.com`,
`api.github.com`, `api2.cursor.sh`, `cloudcode-pa.googleapis.com`. Grok makes
no call at all: its figures come from `~/.grok/logs/unified.jsonl`, the log its
own CLI writes, so they move **only when you use Grok on this machine**.

That failed quietly. A log left alone for nine days had the widget showing 23%
of a credit window that had closed on the 19th, while the account had spent 57%
of the window it was actually in. More than double, with nothing on screen to
say the figure was old.

Three settings, all off or hourly by default:

| key | default | what it does |
|---|---|---|
| `grok_ping` | `false` | GET `cli-chat-proxy.grok.com/v1/billing`, with the bearer token the Grok CLI leaves in `~/.grok/auth.json` |
| `grok_ping_minutes` | `60` | how often. The window moves over days; an hour is current without being traffic |
| `grok_ping_after_session` | `false` | run `grok agent stdio` once after a session goes quiet, purely to refresh that token |

**Off by default for two different reasons.** `grok_ping` talks to a vendor,
and a widget that reads should not start doing that because it was launched.
`grok_ping_after_session` is the stronger case: it starts somebody else's
program. It exists because the token expires — mine had lapsed 8.6 days before
I looked, on the same day the CLI last ran — and without a refresh the asking
works for a while and then silently stops, which is the failure it was added to
fix.

The screen says which state it is in, in both places it appears:

```
── WEEKLY QUOTA ── resets in ~1.1 days
 not live · ~/.grok/logs/unified.jsonl · window closed 5d 21h ago
 Only your own Grok sessions update it. usage.grok_ping polls x.ai instead.
```

```
── WEEKLY QUOTA ── resets in 1.1 days
 live · polled x.ai just now, every 1h
```

The age quoted is the **reading's**, not the file's. The CLI touches that log
whenever it starts, so a file written minutes ago can still hold a credit
figure from a fortnight back, and "written 17m ago" beside a percentage reads
as a fresh percentage.

When the recorded window has closed, its end date is rolled forward on the
window's own measured length until it covers now — the length is measured
rather than assumed to be seven days, because the server states the period type
and a fortnightly window should not be guessed weekly. That is a calculation
rather than a reading, so the countdown carries a `~`, and the **pace figure is
suppressed**: pace is usage against time elapsed, and how much of the current
window has been spent is exactly what nobody knows.

CodexBar reaches the same endpoint and hits the same wall — it reads the cached
credential and does not refresh it either, so an expired token drops it back to
local session files, as this does to the log.

## The pace mark on every quota bar

```
 premium reqs  █████████┃██░░░░░   71%  -20%
 7d            █████░░░░┃░░░░░░░   29%  +23%
```

The `┃` is where an even burn would have reached by now — the window's own
progress, drawn on the bar it belongs to. A fill **short** of it is spending
slower than the clock; a fill **past** it is not, and Copilot's above is.

It exists because a percentage cannot separate a lane 71% spent with three
weeks left from one 71% spent with three days left, and neither can colour:
both are the same red. The mark is the same quantity the `+N%` column reports,
put where the eye already is.

It **replaces** a cell rather than adding one, so it costs no width, and it is
a different glyph from the bar rather than only a different colour — it
survives with colour off.

**One colour on every bar**: white on its own dark cell. It was first tinted green when
the fill was behind it and amber when past, which was wrong twice over: a
reference line that changes colour looks like the line has a state, when the
state being reported is where the fill sits relative to it — and one amber mark
beside five green ones read as *that agent's mark meaning something different*
rather than that agent being behind. The relationship is already legible from
the geometry, and the `-20%` column states it.

Plain white would not do it — the agent hues are themselves light, so white
foreground on a full bar manages **1.04:1** and disappears exactly where it
matters. Giving the mark's own cell a dark background instead makes it read
identically on a full bar, an empty track, or the boundary between them, at
**17.7:1**, and costs no width.

No mark is drawn when the window is unknown, or when a reading is **cached**
and its window may already have closed: a pace computed from a window that has
ended is arithmetic about nothing.

Cursor is the exception. Its three lanes are coloured as *categories* —
included, auto, api, in the palette `cursor-agent` uses for the same three —
and a heat-coloured fill would overwrite that distinction to say something its
own dollar line already says.

## On colour

**Every quota bar is drawn in its own agent's colour**, dark at the left of the
fill and full at the right, so a row says whose it is before you read the
heading — and on the `+` tab, where six agents share a screen, without one.
Claude keeps the terracotta of its own `/stats`, Codex its dark-grey-to-white,
Grok its blue, Cursor its green; Copilot and Antigravity have no calendar to
borrow from and were given hues clear of the amber and red reserved below.

Cursor's three lanes are **three tints of that one green** rather than three
unrelated hues. They are still categories, not a ramp, so they stay
distinguishable — but they now read as Cursor's, which three borrowed colours
never did.

The two stops of that ramp are measured rather than chosen: `0.51` keeps the
dimmest filled cell at **3:1** against the terminal background for the darkest
agent hue, and `0.34` leaves the empty track at least as visible as the flat
grey it replaced (2.20:1 against 2.16:1). Cursor's dimmest lane clears WCAG AA
at 4.99:1.

**Red still means something is wrong, and nothing else** — it just stopped
being a gradient. The percentage is written in its agent's colour like the bar
beside it, and turns:

red at **90% or more spent**, because nearly empty is trouble whatever the
pace. A green-through-red ramp did neither job: it made every figure faintly
warm and said nothing at all about time.

Being behind the clock deliberately does **not** colour the percentage. The
pace cell beside it is already amber for exactly that, and a number turning
yellow next to its own explanation turning yellow reads as two problems rather
than one. Amber appears once per row, on the figure that means it.

The token calendars keep their own four-step single-hue ramps, and rankings are
never green-through-red: that would imply the largest is the worst, which it is
not — it is simply the largest.

Every bar carries its label and its number, so colour is never the only thing
saying what a row is, and the pace mark is a glyph before it is a colour.

## The thing this widget does not do

Nothing on this pane invents a denominator. Where an agent publishes a limit it
is shown against that limit; where it does not, the tab says what was spent and
stops. That is the whole point of the repo, and the reason an empty tab is
empty rather than full of plausible zeros.

Claude Code's `stats-cache.json` is still a spend-only file — it carries no
limit and no reset. Its quota block comes from somewhere else entirely, above.

## Scrolling

A tab is as long as it is — forty-five rows for Claude — and a pane on a wall
is rarely that tall. `↑` `↓` move through the body while the title, the tab bar
and the footer stay put, so you never lose which agent you are looking at.

```
 local state · live quota · read 2s ago   · = detected   9-27 of 45 ▲▼
```

The header says which rows you are on out of how many, and the arrows say which
way there is more. Both matter: a partial view that looks complete is the same
failure as a truncated total, and an arrow that is merely *absent* at the top of
a long tab reads identically to a tab that ends there.

Each tab keeps **its own offset**, so switching away and back returns you to
where you were reading rather than to the top.

This replaced a set of height thresholds. Sections used to disappear on a short
pane — the token calendar below 26 rows, Cursor's spend below 30 — which was
the right call when anything past the fold was gone for good. Now that the
content is reachable, hiding it would be the only thing making it unreachable.

The scroll hint appears **only when there is something to scroll**, but the
space for it is reserved either way, so the fold does not move under you when a
refresh makes a tab a row longer.

## Keys

| Key | Action |
|---|---|
| `←` `→` / `tab` | switch agent |
| `↑` `↓` | scroll the tab |
| `pgup` `pgdn` | scroll a page |
| `home` `end` | jump to the top or bottom |
| `r` | re-read the files now |
| `q` | quit |

## Cost

Small. Local files are read every 30 seconds; Codex rollouts are parsed once
each and cached on mtime and size, because one is 29MB and a finished rollout
never changes. There are five quota calls — Claude, Codex, Copilot and two for
Cursor — each held for two minutes, three subscription reads (Claude, Cursor
and Antigravity) held for an hour, and Cursor's five-page event fetch every
half hour. A pane left open all day makes about 163 requests an hour between
them, whichever tab is on screen.

Two slow things block the first poll, so a freshly started widget takes
roughly fifteen seconds to paint anything on any tab: Cursor's event fetch,
and the first pass over Claude's 520MB of transcripts (about 2.6 seconds).
Both are cached afterwards — the events half-hourly, the transcripts per file
on mtime and size — and neither is paid again.

## Which agents appear

By default it **discovers** them: an agent gets a tab when its CLI is on
`PATH` **or** it has left state behind. Both, because either alone is wrong —
a CLI installed under another name would vanish, and an agent uninstalled last
week still has history worth reading.

```json
"usage": {
  "agents": [],
  "exclude_agents": [],
  "refresh": 30
}
```

| | |
|---|---|
| `agents: []` | discover whatever this machine has — the default |
| `agents: ["codex", "claude"]` | exactly these, in this order, installed or not. Listing them is also how you turn discovery off |
| `exclude_agents: ["copilot"]` | drop one either way |

Naming an agent is how you say *"keep the tab even though it is not installed
yet"* — if you listed it, you want it. That is the same
empty-means-discover idiom as `github.accounts` and `linear.exclude_teams`, so
it needs learning once.

The header says how many detected agents the config is hiding, so discovery
stays visible rather than magic, and a name that matches no known agent is
called out — `unknown agent in config: nonsence (known: claude, codex, …)` —
rather than silently ignored. If the settings would leave no tabs at all it
shows everything instead, because an empty widget teaches nothing and the
likeliest cause is a typo.

Adding support for a new agent is one entry in `AGENTS`, giving the binaries
to look for and the paths that prove it has run.
