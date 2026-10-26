# `agent-usage`

[← all widgets](../../../../docs/README.md)

How much the coding agents on this machine have actually been used — one tab
per agent, from each agent's own local state where it has any, plus a live
quota where that agent publishes one.

The agents this widget reads:

- Claude Code
- Codex
- Cursor
- Grok
- Copilot
- Antigravity
- Notion
- Perplexity
- Devin
- Droid (Factory)

```
╺━ AGENT USAGE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 Checked 16s ago   · = detected
 [CLAUDE]· CODEX · CURSOR · GROK · COPILOT ·

 ── QUOTA ── live · account-wide, not this machine   max
 session 5h ███████░░░░░░░░░░░░░░░░░░░░  25%  resets in 3h 14m
 overall 7d ███████████░░░░░░░░░░░░░░░░  41%  resets in 15h 24m
 Fable 7d   █░░░░░░░░░░░░░░░░░░░░░░░░░░   3%  resets in 15h 24m

  extra usage A$0.00 of A$50.00 limit · A$50.00 left

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

The dim status line below the title says `Checked 16s ago`: time since the
widget completed its last collection pass, not the age of every provider's
data. A pass can reuse cached readings; each provider's quota section keeps
its own live, cached or unavailable status. Before the first pass completes,
the line says `Not checked yet`. The title's spinner shows a check in progress.

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

An agent that publishes no quota is **named at the bottom under its own heading**,
with the reason the live bar is missing — not silently dropped, and not dumped
into a `No quota published by: …` list. The agent's own tab can still be full:
those numbers are **local spend**, read from what the agent left on this
machine. `[+]` only ranks a live (or last-session) quota lane from the vendor,
so a busy tab and an empty summary row can both be true. Each of the agents
wants something different before it can publish one — a signed-in credential,
a billing reading in its own log, or `agent_usage.grok_ping` turned on — so
each quiet agent says which step is the missing one rather than leaving the
row blank.

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
reads without colour. A `·` marks an agent that is installed. Extra Claude
profiles from `claude_config_dirs` sit on that strip under their own label,
not under a second CLAUDE.

## What each tab can actually show

**Claude Code** — the real one. `~/.claude/stats-cache.json` (or the same
file under each configured `claude_config_dirs` entry) carries per-model
token counts (input, output, cache read, cache written), total sessions and
messages, and around four weeks of daily activity. All of it is spend.

The summary block mirrors Claude Code's own `/usage`, from the same file:
favourite model by output tokens, total across all four token kinds, sessions,
longest session, active days, both streaks and the most active day. Rendering
it against a `/usage` screenshot taken the same week agrees on every figure the
cache had settled — 31 sessions, a longest session of `4d 10h 52m`, a longest
streak of 21 days, Aug 1 as the busiest day.

### The cache only moves when you open `/usage`

This is the one thing to know about the four sections that read that file —
summary, by model, and the two per-day charts. **Claude Code rebuilds
`stats-cache.json` when its own `/usage` screen is opened, and at no other
time.** Not on a schedule, not on startup, and not from a headless session.
Measured here: the file sat untouched for three days across a version upgrade
and five live sessions, then refreshed the moment that screen was opened.
`/stats` and `/cost` are aliases for the same command.

Metered usage reads the project transcripts independently of this cache, so
a missing or unreadable cache does not hide recorded tokens or their cost.
When no tokens are recorded in the displayed windows, METERED says so;
missing prices are reported only for models with recorded usage.

So those four sections can be days behind while everything else on the tab is
current, and nothing this widget does can move them — the refresh is the
reader's to trigger. The tab says so in as many words at its foot, and the two
per-day charts carry the lag in their headings when there is one:

```
 ── MESSAGES / DAY ── 60d · peak 12444   cache 5d behind, to Sep 12 · /usage refreshes
```

**`lastComputedDate` is the last complete UTC day**, so the cache never holds
today and is always at least one day back. That floor is silent, because a
caveat that is permanently on is one people learn to stop reading; the
standing note at the foot of the tab covers it instead.

The count is taken **in UTC**, which is the calendar the date is stated in.
Subtracting it from the local date compared two calendars and was wrong for
part of every day: east of UTC it overstated the lag for the first hours of
each local day, and west of UTC the count reached zero and the caveat vanished
while days were genuinely missing — a chart short of data with nothing on
screen saying so, which is the founding hazard wearing the face of a pane that
is fine.

The **tokens-per-day calendar** is laid out like the contribution calendar in
`github` — weekdays down the side, weeks across — so the two read the same
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

On **`[+]`**, Claude also shows **`N reset available <expiry>`** under
each account's rows, with the same indentation and wrapping as Codex.
The date is the soonest expiry in the machine's local zone, formatted as
`23 Oct 04:58 +08:00` (or `23 Oct 04:58 AWST +08:00` when a zone abbreviation
is available), without the year or parentheses. If any counted
grant has no readable expiry, only the count is shown. Zero or unavailable
inventory adds no line. Multiple profiles keep their own counts, including
when an account has no quota lanes. Cached availability carries its age,
and a grant whose expiry has passed is no longer counted.

The Claude tab carries the same **BANK RESET** section as the Codex tab,
drawn by the same code: the count in the title, then one row per reset,
soonest first. A grant of two is two rows with the same expiry - nothing is
merged. A grant whose `type` is `full` is a `Full reset`, the name Codex's
credits carry; any other type is a plain `Reset`, and the grant's `label`, a
sentence rather than a kind, is not shown. A grant with no readable expiry is
`Reset · expiry not given` (or `Full reset · …`) and comes last. When the
reading is old the section ends with `cached 5m ago`. An inventory that
answered with no resets shows `── 0 BANK RESETS ──` and `None in the
account.`, as Codex does. A reading with no inventory in it - no quota yet,
or a block that is absent, ineligible or unreadable - draws no section,
again as Codex does when its inventory request fails, so an unread bank is
never shown as an empty one. Each profile's tab shows its own bank. The
`[+]` line agrees with the section for the same profile: the same count as
the title and, when every reset has a readable expiry, the first row's date
without its year. When any reset is undated the line gives the count alone,
and in a pane too narrow for it the line wraps onto the next rows, its date
moving below the count. A count past 1000 resets is not read
as an inventory.

This comes from the optional `cedar_ember` block on
`GET /api/oauth/usage?cedar_ember=1`, using the existing per-profile OAuth
credential, poll interval, and account-bound snapshot. There is no second
poll or redemption request. An eligible block lists grants with
`resets_left`, `usable_now`, `paused`, `starts_at`, and `ends_at`; only usable,
unpaused, unexpired grants count. Their wire fields and the CLI request
surface were checked against the published Claude Code **2.1.296** package.
The request uses its `claude-cli/2.1.296 (external, cli)` compatibility
User-Agent because the reset block is gated by surface/version; an
ineligible or absent block stays unknown. The optional query and surface
gating are also documented in
[ai-usagebar's Claude reader](https://github.com/akitaonrails/ai-usagebar/blob/main/src/anthropic/fetch.rs).

The lanes are the ones Anthropic itself marks as worth showing, and each
arrives already named — so a model-scoped weekly limit appears as **Fable**
without this widget knowing that name, and any other scope would appear the
same way. The limit that will stop you first is the lane drawn brightly, and a
severity other than normal is printed as a word beside the reset, because a
colour alone cannot say *why* a bar is red.

**The fallback is where the care went.** Claude Code keeps its own copy of the
same reading, and that is what the tab falls back to when the credential has
expired or the call fails — but it is labelled `cached 10h ago`, and any window
whose reset has already gone by says **`already reset`** instead of counting
down. A stale five-hour window otherwise describes a period that has ended,
which is precisely the kind of number this repo exists not to draw. A cached
reading can sit well under the live one, which is the whole reason it says it
is cached rather than presenting it as what is left now.

### Extra usage (the monthly cap)

Under the lanes sits **on-demand spend**: what the account has spent past the
subscription, against the monthly cap set on claude.com. Real money, billed,
and the same thing Cursor's own extra-usage line reports — so it is drawn in
the same words, because two panes side by side saying `disabled` and `off`
about one state would be two vocabularies for one fact.

```
  extra usage A$0.00 of A$50.00 limit · A$50.00 left
```

It stays **money rather than a bar**, for the reason the Cursor section gives
at length: this is spend against a denominator of its own, and the three
percentages above it are not that denominator. What is left is worked out from
the pair rather than taken as given, and a cap lowered below what has already
gone reads as an *overage* rather than as `-9.00 left`, which is arithmetic
where a reader needs a fact.

The currency is the account's, written as its own symbol. The dollar
currencies keep their letter, because `A$50` is fifty Australian dollars and
`$50` is a different claim about the money; a code the list does not name is
written as the code, since `SEK 50` costs one cell more than a symbol and
invents nothing. On a pane too narrow for the pair the symbol is the first
thing to go, before the clauses after it, and it goes from every amount at
once — one figure written `A$12.34` beside another written `50.00` would read
as two currencies on one line.

Every amount arrives in minor units with its own exponent — `5000` at exponent
2 is fifty — and reading one as the other is a hundredfold error in a figure
about money. The drawn amount keeps that exponent: one minor unit at 3 is
`0.001`, and drawing that as `0.00` would hide real spend. An empty currency
is written with no symbol rather than as `$`, which is a unit the server
never named.

Colour comes from the server's own `severity`, the same field the limit rows
above already read, and from its `spend_limit_reached`. Neither is a threshold
invented here: one pane holding two opinions about one account's health is
worse than either alone.

On `[+]` it becomes a lane labelled `extra A$50`, set apart from the windows
above it by a blank line: those three are views of one subscription and they
nest, this is money on a different clock, and a fourth bar in an unbroken run
reads as another slice of the plan. The cap rides on the label,
because 19% of an unnamed limit is not a number anyone can act on. The
percentage is **not clamped**, so a cap set under what is already spent draws
a full bar beside the true figure. Claude states no reset for this window —
there is no date anywhere in the block — so the lane carries **no countdown
and no pace** rather than a figure worked out from a date nobody sent.

That is the whole difference between this row and Cursor's, which is otherwise
the same bar:

```
   extra $50     ░░░░┃░░░░░░░░░░░░░░░░░░░░░░░    0%  +15%  25d 15h
   extra A$50    ░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%
```

Cursor's carries the mark and the countdown because Cursor sends its billing
cycle. The profile endpoint does carry a subscription date, and with Stripe
billing beside it the cycle could be inferred from the anniversary — but
whether this cap rolls on the anniversary or on the calendar month is unknown,
the two differ by up to thirty days, and a countdown that wrong is worse than
none. The bare row is the decision, not an oversight.

**The line is drawn in every state, including the ones with nothing to
report.** It used to appear only for an account with extra usage enabled and a
cap present, which meant a switched-off cap and a response nobody could parse
both drew nothing at all — and nothing reads as *this account has no extra
usage* when it may mean the opposite.

| state | the line reads |
|---|---|
| a cap, under it | `A$0.00 of A$50.00 limit · A$50.00 left`, and a lane on `[+]` |
| a cap, over it | `A$10.00 of A$1.00 limit · A$9.00 over` — the real figures, and a lane the summary draws full |
| no cap at all | `A$9.64 · no limit` — money with no denominator, and **no lane**: a bar needs a ceiling |
| switched off, nothing spent | `disabled` |
| switched off, spent earlier | `A$10.00 · disabled` — that money is billable and stays on screen; the lane goes, because there is no allowance left to be a percentage of |
| block absent, or a shape not recognised | `not reported`, with the keys that did arrive, so an unmapped shape can be read off the pane and mapped rather than guessed at |

The line also survives a quota block with **no bars at all**. It lived inside
that block, so an account answering with a spend cap and no limit percentages
lost the one figure on the tab that is real money — and lost the sentence
explaining the missing bars along with it, since an extra-usage lane counts as
a lane and rightly silences that sentence on `[+]`. The tab now says both.
An unrecognised spend shape still names its keys here; a block that arrived
empty stays silent, because the note already covers a quota that answered
nothing. A cached snapshot on this path still says `cached … ago` — the
heading that normally carries the age is the early return this stands in for,
and without the stamp the money would read as this month's.

**Only the first state has been seen on a real account.** The switched-off and
unlimited readings rest on assumptions named in the tests that cover them.

The unlimited state is there because Anthropic documents the setting —
*Set to unlimited* sits beside the monthly cap on claude.com — so the option
exists and a reading for it has to. What has *not* been seen is the shape the
response takes when it is chosen, so absence of a ceiling counts as unlimited
only where the block is recognisably a spend block: it has to say `enabled`
out loud and carry one more field a real one carries, and
`extra_usage.monthly_limit` has to be gone too — that field is the same
ceiling. A half-arrived response becoming *this account may spend without
limit* is the worst of the five to get wrong, and it is the one guarded
hardest.

**Cursor** — both quota and authorship.

The quota is the same three lanes `cursor-agent`'s own in-session Usage view
shows — total, cursor models, other models — plus what the plan includes,
what has been spent beyond it, and the billing cycle reset. It reads them the
same way the CLI does, on the credential the CLI has already signed in with —
which is **not a documented interface**, and can therefore stop working on a
day nothing here changed. So a failure says which failure it was, on the tab
and on `[+]`: no credential to read, or Cursor did not answer. It used to go
quiet and fall back to authorship alone, which read as a Cursor that had
simply not been used.

**The percentages and the dollars have different denominators**, which is
Cursor's own doing and worth stating. The three lanes are Cursor's own
percentages, unaltered. Under them sit two dollar lines, each against a
denominator of its own:

```
  spend        $400.00 of $400.00 included
  extra usage   $9.64 of $50.00 limit · $40.36 left · resets 12 Sep
```

The spend line is **the part that counts against your plan**, and only that.
Cursor also reports spend it granted you past the included amount, which is
not against the limit at all — putting the two together produced *$1794.80 of
$400.00*, a sentence that cannot be true. What is left joins the line only when
Cursor says what is left.

Nothing is rewritten here: the lanes are the bars `cursor-agent` itself draws,
and the dollar lines stay dollars rather than becoming two more bars, so a
percentage and a spend are never put on one scale. Cursor sends a summary
sentence too — *"You've used 91% of your included usage"* — which agrees with
no figure beside it, so it is not shown: a sentence that contradicts the
numbers next to it is worse than no sentence.

### Extra usage (the spend limit)

Everything past the plan's included amount is **on-demand spend**, billed, and
capped by a monthly spend limit the account sets on cursor.com.

A personal cap reads **your own** spend against your own limit. A shared team
budget reads the pool's spend against the same limit and is labelled
`team pool`, because that is the population the ceiling belongs to — the same
percentage means a different thing in each case. What is left is worked out
from the pair rather than taken as given, since a cap lowered below what is
already spent would otherwise read *"-$9.00 left"*, which is arithmetic where
a reader needs a fact.

On `[+]` it becomes a lane labelled `extra $50`, on the plan's own cycle —
extra usage resets when the cycle does — ranked with everything else, and
separated from the three plan lanes by a blank line: it shares their clock but
is not a fourth slice of them. The
label carries the cap because a percentage of an unnamed limit is not a number
anyone can act on: 19% says nothing until the reader knows it is 19% of fifty
dollars. The percentage itself is **not clamped**: a cap set below what is
already spent is a real number over 100, and the summary draws a full bar
beside the true figure, with the cap in the label to make it legible without
opening the tab.

**The limit has three states, and the account can change it mid-cycle**, so
all three have to be drawable from whatever the next response says — plus a
fourth for a response the parser does not recognise, which must never be drawn
as one of the other three.

| state | the line reads |
|---|---|
| fixed, under the cap | `$9.64 of $50.00 limit · $40.36 left · resets 12 Sep`, and a lane on `[+]` |
| fixed, over the cap | `$10.00 of $1.00 limit · $9.00 over` — the real figures, the overage rather than a negative remainder, and a lane the summary draws full |
| unlimited | `$9.64 · no limit` — dollars with no denominator, and **no lane**: a bar needs a ceiling, which is the refusal the Grok Bot allowance already makes |
| disabled, nothing spent | `extra usage  disabled` |
| disabled, spent earlier in the cycle | `$10.00 · disabled` — that money is billable and stays on screen; the lane goes, because there is no allowance left to be a percentage of |
| block absent, or a shape not recognised | `extra usage  not reported`, with the keys that did arrive, so an unmapped shape can be read off the pane and mapped rather than guessed at |

**Only the fixed state has been seen on a real account.** A cap switched off
and a cap never set look alike from outside, so the disabled reading is the
one to trust least; the rest is drawn from what Cursor actually sends, and a
shape not recognised says `not reported` rather than being drawn as one of the
others. Asking about the cap is best-effort: a refusal there leaves every
other line exactly as it was.

When no limit is set, Cursor's hard-limit call answers `2147483647` — INT32_MAX,
its way of saying *no limit*. That is read as the unlimited state and is never
drawn as a cap of two billion dollars.

### SpaceXSI (the unified Cursor and Grok plan)

Cursor and Grok now sell one plan between them, and an account moved onto it
reads differently from either product's own plan:

- **One weekly included pool**, shared by Cursor, Grok Bot and the Grok CLI.
  The usage call's billing cycle becomes that week.
- **Third-party models bill at API list price** against prepaid credits rather
  than drawing on the pool, so `other models` would sit at nought for ever and
  `cursor models` is the same number as the pool.
- **Prepaid credits** — a balance you top up, with optional auto top-up.
- **A monthly limit on additional usage**, on the monthly cycle, not the week.

Whichever vendor is on this plan is drawn under one heading, **SpaceXSI**, on
`[+]` and in the tab strip. On the same account, CURSOR and GROK fold into one
**SPACEXSI** tab: the pool once, split by product, then an extra-usage
report, the profile's activity, and the account's spend and plan.

```
 ── SpaceXSI ── live · weekly · resets in 6d 22h
  14% of the week gone
  weekly pool     ███▓▓▓░░░░░░░░░░░░░░░░░░░░░░░   10%  +9%
                  ■ Cursor    ~5%
                  ■ Grok Bot  ~5%

 ── EXTRA USAGE ── month 13 Oct → 12 Nov
  prepaid credits ░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%
                  $20.00 left of $20.00 paid · $0.00 used
                  auto top-up $10.00 when below $2.00 · payment method saved
                  top-ups 13 Oct $20.00 manual
  usage limit     ┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%
                  $0.00 of $100.00 this month · $100.00 left · resets 12 Nov

 ── ACTIVITY ── profile tokens / day · peak 937.0M on Sep 16
  streak 2 days · longest 33
     Nov Dec Jan Feb Mar Apr May Jun Jul Aug  Sep
 Mon               █ █    █ ██ ███ █     █████████ █
  …
  Less ████ More

 SPEND / DAY · SPEND · METERED · SUBSCRIPTION
```

The merged tab is one report, with no Cursor or Grok headings — the plan is one
product — and it draws **cloud figures only**. SpaceXSI is an account, so this
machine's share of it is left out: no AI-written-code counts from Cursor's local
tracking database, and no Grok Build session totals or second heatmap from
`~/.grok/sessions`. A vendor still on its own plan keeps those sections. SpaceXSI is drawn in greyscale: the brand is monochrome, its black
is unreadable on a dark pane, and a mid-grey leaves the bars a ramp to draw.

**Activity** is the heatmap cursor.com/@handle shows, and on the unified plan
grok.com shows the same one: daily token counts and streaks from the account's
own profile (`GetViewableProfileByHandle`, by the handle `GetUserProfile`
returns). It is headed as the *profile's* tokens because whether it counts
Grok's side of the plan as well as Cursor's is not settled — on the day of the
merge it was still empty while the pool had moved.

The pool bar is drawn in one segment per product, each product on a line of
its own beneath it in its segment's colour. The split is Grok's: its credits
endpoint states each product's share of the week in whole percents, leaves
Cursor's share unnamed, and omits any product that rounds to nought — so the
shares are marked `~`, and a sliver of the pool the whole percents do not
account for is drawn dim rather than handed to a product. Matched against
x.ai's own usage dashboard on 2026-10-13: Cursor and Grok Bot at about 134M
and 138M tokens came back as 5% and 5% of a 10% week, and Grok Chat and Grok
Build, under 1% of it, did not come back at all. Token counts per product are
on that dashboard only, behind a browser sign-in, so they are not shown here.

The report says everything the prepaid and limit calls state that is about
usage: what is left of what was paid and how much has been drawn, this month
against its limit on the monthly clock, the refill rule and whether it has a
payment method to charge, and the last five top-ups — a failed one in the
warning colour, since that is the rule firing and the money not arriving. The
same calls also carry cursor.com's minimum, maximum and recommended limits;
they are settings-page hints rather than usage, and on the measured account
the limit sat below the stated minimum, so they are left out.

On `[+]` the SpaceXSI group has up to three bars, named the same way on the tab,
as **percentages only** — the dollars and the refill rule are the tab's:

- `weekly pool` — the shared included allowance, on its weekly clock.
- `usage limit` — this month's additional usage against what cursor.com calls
  the *Monthly additional usage limit*, on the monthly clock.
- `prepaid credits` — what has been drawn from the prepaid balance against what
  was paid in; X calls this balance *Additional usage credits*. No countdown
  and no pace, since a balance has no clock.

A bar shows only when its feature is in use: no `prepaid credits` for an
account that has paid nothing in, and no `usage limit` without a limit. With
no limit but money spent this month, the tab still states the spend, with no
bar. A prepaid call that failed says `not reported` on the tab, because a
failure is not the same as an unused balance.

Each vendor is read on its own terms, every refresh:

| Cursor | Grok | what is drawn |
|---|---|---|
| unified | unified, same account | one SPACEXSI tab and one SpaceXSI group on `[+]`: the pool, the credits and the limit once |
| unified | unified, a different account | two sections and two tabs, `SpaceXSI · via Cursor` and `SpaceXSI · via Grok`, nothing merged |
| unified | its own plan, or absent | Cursor's tab and lanes under SpaceXSI; Grok keeps its own tab, heading and lanes |
| its own plan | unified | Grok's tab and lanes under SpaceXSI; Cursor keeps its own tab, heading and three lanes |
| its own plan | its own plan | exactly what each drew before — no SpaceXSI heading |

**How each one is told.** Cursor's plan says so outright — `isMerged`, or an
`includedUsagePeriod` an account's own plan does not carry — and the credits
and monthly cycle are asked for only then. Grok's credits answer carries a
top-up method and a `GrokBot` product that only the unified plan sends; its
`isUnifiedBillingUser` flag does not count, because the log shows it set on
readings from before the plans merged. Grok's log never carries a marker, so
**the same account** is recognised by the weekly window: Cursor's and Grok's
matched to the millisecond on the measured account, and two separate accounts
sharing a window that precisely is not a coincidence worth planning for.

Shared, Cursor's reading of the pool is the one drawn, because its usage call
is asked every two minutes where Grok's may be a log line; Grok contributes the
pool's split by product beneath the bar, and its own pool reading when
Cursor's usage call has not answered. Grok's `prepaidBalance` arrives in cents — the
measured `2000` is the $20 Cursor states as `availableCents: "2000"` — and is
read that way.

**Still open:** Cursor's monthly cycle call and its plan's own monthly end
disagreed by a day on the measured account. The monthly cycle call is the one
used, and the plan's end is the fallback.

Cursor supplies a **spend** section too: per-model input, output and cache
tokens with Cursor's own money figure — not an estimate — over the last 30
days. It is what the plan percentages are made of, and answers which model
actually spent the money.

`ai-tracking/ai-code-tracking.db` supplies the authorship half: how many edits
the agent made, across how many conversations, and how many lines in scored
commits came from the agent rather than by hand. A different question from
cost, and labelled as such.

**Codex** — real, and it took a second look to find. The counters are in the
session transcripts under `~/.codex/sessions/`, which carry a running token
total and the last turn's usage, timestamped.

That gives totals *and* an **output rate** — output tokens over the wall-clock
gap between turn boundaries, which is why the figure is a rate for this machine
rather than a benchmark of the model.

**Copilot** — it *does* keep usage locally, in `~/.copilot/`: per-turn input,
output, cache and reasoning tokens, AI credits, and — uniquely among these
agents — its own timings. It is the best-shaped usage data of the lot, and an
empty tab here means no turns have been recorded rather than a reading that
failed.

**The quota half does not depend on it.** Copilot's remaining allowance is not
in the local store at all; it is on the account, read on the credential the CLI
signed in with. One machine can be signed in to github.com and an Enterprise
host at once, and the right one is picked for the account on screen.

Each pool names itself, so the lanes are whatever the account has — premium
interactions against a monthly figure, with chat and completions `unlimited`.

An unlimited pool gets **no bar**. It has no denominator, and drawing one as an
empty gauge would invent the limit the field explicitly denies. The pools are
rendered from the list, so a new one appears without an edit, and the metered
ones sort first — an unlimited pool is not news.

GitHub reports what is *remaining*; the pane shows what is **spent**, like
every other tab, so red always means the same thing across the wall.

**The window is derived, and only when it is safe to.** Copilot says when the
quota resets and never how long the window is. A reset landing on midnight UTC
on the first of a month is what a calendar-month cycle looks like, so the span
is worked back a month and shown as `window 1 Aug → 1 Sep · monthly`. A reset
that does *not* land on a month boundary gets **no window line at all**,
because then the cadence genuinely is not known and a guessed one would be
read as a fact.

A pool with a reset of its own prints it rather than inheriting the one in the
heading.

**Antigravity** — a subscription, and no usage at all.

It leaves plenty behind in `~/.gemini/antigravity-cli`: a conversation store
per session, a prompt history, and logs. None of it counts a token. Each
conversation is its own SQLite file whose `steps` table records what the agent
did, so the tab reports conversations, agent steps and prompts — real work
done, but not cost.

Its quota never lands on disk. The process that already holds it answers on
loopback, so that is where the widget asks, and **the request never leaves the
machine**.

```
 ── QUOTA ── live · account-wide, from the local language server
  Gemini Models
   weekly ░░░░░░░░░░░░░░░░░░░░░░░░   0%   +3%  resets in 6d 18h
   5h     ░░░░░░░░░░░░░░░░░░░░░░░░   0%  +14%  resets in 4h 15m
  Claude and GPT models
   weekly ░░░░░░░░░░░░░░░░░░░░░░░░   0%        resets in 6d 23h
   5h     ░░░░░░░░░░░░░░░░░░░░░░░░   0%        resets in 4h 58m
```

Shown as **spent**, where the source reports what is remaining, so red means
the same thing here as on every other tab — and with the same pace column.
Which groups appear is whatever the source sends, and each is followed by
Google's own list of the models it covers — so "Claude and GPT models" says
underneath it that the GPT is GPT-OSS.

It is present only while Antigravity is running, which the pane does not
disguise: no process, no port, no section.

Google will answer for the same quota when the app is closed, on the same
account. The app's own answer is still preferred — it moves as the app is
used, where Google's is a record — so Google is asked only when there is
nothing running to ask, and **the heading says which was read**:

```
 ── QUOTA ── live · account-wide, from Google - the app is not running
```

So the quota survives the app being closed, for about an hour after it last
ran — the credential Antigravity leaves behind lasts that long, and this widget
deliberately does not renew it on Antigravity's behalf.

**Four sources**, each of them optional:

| order | source | works when |
|---|---|---|
| 1 | Antigravity itself — the app, or an `agy` you started | Antigravity is open and answers outside readers |
| 2 | `agy -p /usage`, run by the widget (`antigravity_start`) | the CLI is installed, signed in and 1.1.11 or later |
| 3 | Google | within an hour of the last run |
| 4 | `agy`, started under a pty by the widget (`antigravity_start`) | only an `agy` older than 1.1.11 |

The second costs a process and a few seconds where Google costs one request,
and is asked first anyway: it is the only source left with the **Gemini
group**. Google's summary has only ever carried the Claude and GPT-OSS group.

Nothing that runs for the widget outlives the fetch, and **an `agy` you started
yourself is never shut down by this** — it would have answered as source 1
long before anything was started.

**`agy` builds from October 2026 close source 1.** Their language server
refuses every outside caller with `missing CSRF token`, and hands the token
only to processes `agy` starts itself — it is in no file, log or environment
the widget can read, and the widget does not go looking. The pty start (4)
reads the same server, so it is closed too, and kept only for older builds.

What still works is `agy`'s own `/usage` command in print mode — the one
CodexBar reads. It answers from the CLI's session without starting a turn
(zero tokens) and returns both groups, each with its weekly and five-hour
window:

```
 ── QUOTA ── live · account-wide, from `agy`, started for this reading
  Gemini Models
    Models within this group: Gemini Flash, Gemini Pro
   5h     ┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%  resets in 4h 59m
   weekly ┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%  resets in 6d 23h
  Claude and GPT models
    Models within this group: Claude Opus, Claude Sonnet, GPT-OSS
   5h     ┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%  resets in 4h 59m
   weekly ┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░    0%  resets in 6d 23h
```

It runs in a private empty directory with `open` and `xdg-open` shadowed by
stubs that fail, so a background poll can never pop a sign-in page; in its own
process group, killed whole afterwards, so the language server it boots goes
with it; and with auto-update off. Releases before 1.1.11 could send an
unknown slash command on to a model as a prompt, which would spend the quota
being read, so those are never asked — the tab says to update instead.

When `/usage` cannot be read and Google's summary is what is on screen, the
tab says the app's server refused rather than reading like an app that is
shut:

```
 ── QUOTA ── live · account-wide, from Google - the app's server refused
  Antigravity's language server now refuses outside readers: it wants a
  CSRF token it gives only to processes it starts itself. This is Google's
  summary instead. Google publishes no grouped Gemini limit.
```

Under the groups, the tab also draws **each Gemini model's own allowance**,
which Google publishes per model, so it says which model is spending the
group's limit:

```
  Gemini, per model
    Each model's own allowance, from Google - not slices of one shared limit.
   gemini-3-flash        ░░░░░░░░░░░░░░░░░░░   0%        resets in 4h 58m
   gemini-2.5-pro        ░░░░░░░░░░░░░░░░░░░   0%        resets in 4h 58m
```

They are not added up or averaged into a group figure, because Google states
none. `[+]` gets one lane from them, `gemini busiest` — the model with the
least left — after the four group lanes rather than a dozen. With no Gemini
group on screen they are the Gemini quota there is, and if the per-model call
fails the reason is drawn where the rows would be; with a group, a failure is
left unsaid, since a warning under four good bars reads as though the bars
were wrong.

**Every step that can fail says which step it was**, on the tab and on `[+]`,
because "no quota" covered four situations wanting different things from the
reader:

```
 no quota either: Antigravity's token expired 51m ago - it refreshes them
 itself, so open it or run `agy` once and sign in
 no quota either: asking Google is off - set agent_usage.antigravity_remote to true — press `,` to set it here
 no quota either: Antigravity has not signed in on this machine - run `agy`
 once and sign in
 no quota either: Google refused the Antigravity token: …
```

The first three are decided before anything leaves the machine, so the
sentence keeps saying which it was even while the refusal is being held rather
than retried — otherwise a held failure degrades into "Google did not answer"
about a credential that expired an hour ago and was never sent.

When neither source answers, `[+]` names the agent with the reason rather
than sending the reader to open an app that would not have helped:

```
  ANTIGRAVITY
   no quota · neither the language server inside the app nor Google
   answered. Open Antigravity, or sign in again if its token has lapsed.
```

That sentence used to be empty whenever the tier read perfectly well, which
is most of the time — so the summary named the agent as quiet and then said
nothing about why.

**The roll-call is what is left over.** `No quota published by: …` once led
this block and named every quiet agent, with the explanations below it. That
reads backwards, and it said the same thing twice for any agent that had a
reason, since each reason already opens by saying there is no quota. Claude,
Cursor, Grok and Copilot now explain themselves the same way Antigravity does,
so the roll-call lists only a name we still have nothing to say about —
vanishing entirely when they all have. An agent that is neither detected nor
listed in `agent_usage.agents` is not on this screen at all.

The tier is read on the same credential, which lasts about an hour, so the
section is simply **absent** between refreshes rather than stale.

**Two plans are shown, not one**, and they can disagree: the Code Assist tier
this project sits on, and the Google AI plan the account holds. Here that reads
`free-tier` against `Google AI Ultra`, and neither is wrong — the paid Code
Assist tier is a Cloud licensing arrangement, wanting Cloud terms accepted and
your own project nominated, while the limits you actually run against come from
the consumer subscription. Picking one as "the" plan would have hidden whichever
question the reader was asking.

Google's own upgrade pitch is left as it arrives, inconsistencies and all: it
offers this account more requests per day beside a line saying it is already
on the best plan available.

**Grok** — real too, and the third of these I first wrote off. The session
transcripts under `~/.grok/` carry a running token total, timestamped, so each
day on the calendar is **the tokens spent that day** rather than a whole
session credited to whichever day it was read on. That drives a totals line and
a calendar in a blue ramp.

The weekly quota is real but is not in the transcripts — it arrives on Grok's
own client log, which is why the tab said "no quota" for a while. (`grok du`
reports **disk** use; the name is a coincidence worth not falling for.)

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

This is the quantity CodexBar calls **"in reserve"**.

**The sign is deliberately the opposite of CodexBar's separate pace token**,
where `+X%` means burning *too fast*. The cushion reading is the one that
matches the phrase "in reserve", so the column is labelled in the header rather
than left to be guessed at.

Nothing is fetched for it — the window length and the reset are already on
screen. It is hidden for the first **3%** of a window, because ten minutes into
a week every number looks like a catastrophe or a triumph, and a blank there
means *too early to say* rather than *on track*.

### Work days

`work_days_per_week` (default `7`) says how many days from Monday you actually
work. Below seven, both the pace figure and the `┃` mark measure a window by the
**work time** gone rather than the clock: with `5`, a weekly window that has run
Monday to Friday is all but spent, because the weekend ahead spends nothing —
where the clock would have shown two sevenths of headroom you will not use.

There is no monthly equivalent and none is needed. Every window arrives with its
own start and reset, so a monthly or billing-cycle window counts the work days
actually inside that cycle off the calendar — October 2026 has 22 weekdays
and February 2026 had 20. Windows of a day or less, such as the five-hour
session or a daily quota, stay on the clock: they are being used now, whatever
day it is, and have no weekend in them to skip. A window with no work day in it
at all also falls back to the clock rather than losing its pace. Days are this
machine's local calendar.

## METERED: today, and the last thirty days

Every tab that can cost its tokens shows the same block — two windows, each
with its money, its tokens, and the models underneath, and every model row
carrying both: what it cost and what it ran.

```
 ── METERED ── this machine · at list prices · 14 Oct 2026
  Counted from transcripts, which are written where the agent ran. Claude used
  on another machine, or on claude.ai, is not in here.
  today    $172.64    74.3M tokens
            claude-fable-5-1  $124.38   24.6M tokens
            claude-opus-5      $48.26   49.7M tokens
  30 days  $24887.52  29.9B tokens
            claude-opus-5     $20892.59   26.5B tokens
            claude-fable-5-1   $3295.78   2.8B tokens
            claude-fable-5      $518.33   221.5M tokens
            claude-opus-4-8     $145.36   149.8M tokens
            claude-sonnet-5      $35.27   103.5M tokens
            +1 more
```

Two windows because they answer different questions: a month says what an
agent costs, today says whether that is still true. A single all-time figure
answered neither, and quietly flattered a habit that changed last week.

Dollars and tokens together because either alone is half the answer: one
model's spend is a great many cheap tokens and another's is a few expensive
ones, and the rows used to say only the money. Five models per window, with
`+N more` where there were more.

### A model with no published price keeps its tokens

Where a window holds a model nobody has published a rate for, that model gets
a row of its own under an `unpriced` heading, with its tokens and **no dollar
figure** — a dash, because `$0.00` would say the vendor gives it away. Its own
five, separately from the priced five, so a costly priced model cannot push it
off a list whose absence would read as *nothing here is unpriced*.

And the window's money then says **at least**:

```
 ── METERED ── this machine · at list prices · 14 Oct 2026
  CLI rollouts only. Codex bills Cloud, Web, Desktop and the rest to the same
  account, and none of those leave anything on this disk to count.
  today    at least $9.79     27.3M tokens
            gpt-5.6-sol        $9.79   16.3M tokens
            unpriced
            codex-auto-review      —   11.1M tokens
  30 days  at least $1256.47  2.0B tokens
            gpt-6-astra        $891.76   583.0M tokens
            gpt-5.6-sol        $364.68   685.0M tokens
            gpt-5.6-luna         $0.04   677.3k tokens
            unpriced
            codex-auto-review        —   732.1M tokens
```

The token count on that row covers every model; the dollars cover only the
priced ones. Those were once the same row with nothing saying so, which is a
partial result presented as a total — `at least` is the same word `linear` and
`github-prs` use for a count that stopped counting, and *what the plan saves*
below inherits it, because that figure is this one minus the plan price.
Where **nothing** in a window priced, the money column is a dash rather than
`at least $0.00`: a floor of nothing is not a figure. A configured rate of
zero is a rate, so it stays `$0.00` — and `at least $0.00` where unpriced
tokens sit beside it. A rate that only names some of the kinds that ran is
the same floor: the kinds it knows are a number, the rest are not free.

The token column is content, so a pane too narrow drops it whole rather than
cutting it — `1.` is a wrong number where a missing column is only a narrower
pane. The dollars, the dash and the `at least` never stand down.

### Where the prices come from

Anthropic and OpenAI both **publish** their rate cards, so there is nothing to
invent. The tables are copied from
`platform.claude.com/docs/en/docs/about-claude/pricing` and
`developers.openai.com/api/docs/pricing`, with the sources and the date in the
source file and **the date on screen beside the total**. A published price is a
fact; what makes one dangerous is going stale in silence, and a date fixes
that.

An absent kind means the vendor does not charge for it or does not publish
it — inventing one would be worse than leaving it out, and a zero would say
something different again: that they publish it as free. OpenAI charged
nothing for cache writes until the 5.6 family, which publishes one, so 5.6
carries `cache_write` and the older families still do not. xAI publishes none
at all, and Google bills context caching by storage — per million tokens per
*hour* — which is not a per-request write and is deliberately not carried.

A few prices are worth knowing about because they are not what a neighbouring
model charges. Fable 5.1 and Mythos 5.1 read cache at a quarter of what Fable 5
and Mythos 5 charge, a smaller multiplier than every other Anthropic model.
`gpt-5.6-sol` is a **promotional** price, dated by OpenAI as running at least
through 21 Nov 2026, and is carried because it is the only price published and
the one the meter bills at. `gemini-4-argon` and `gemini-3.8-flash` are
**introductory**: `gemini-4-argon` is 2 / 10 / 0.10 through 30 November 2026
(with implicit-only caching at 0.05× input and no explicit cache storage) and
doubles to 4 / 20 / 0.20 on 1 December 2026, while `gemini-3.8-flash` doubles
on 1 January 2027, and the successor figures are already written down so each
row moves on the day rather than being rediscovered after a month of
half-price totals. `grok-4.7` carries the same
below-200k rates as `grok-4.6` — 2 / 6 / 0.50 — and still needs its own row:
no existing key is a substring of `grok-4.7`, so without one the model costs
nothing. `gpt-6-sol` and `gpt-6-luna` are the same shape of gap against the
5.6 rows — the short-context rates are 2 / 10 / 0.20 / 2.50 and
0.10 / 0.50 / 0.01 / 0.125 — and `gpt-6.1-sol` is the same shape of gap
against `gpt-6-sol`: that id is not a substring of `gpt-6.1-sol`, and cache
reads are $0.10 rather than $0.20. `claude-opus-5` is a prefix of
`claude-opus-5-5`, so without its own row (4 / 20 / 0.20 / 5 / 8) Opus 5.5
would inherit Opus 5, cache reads at $0.50 against the published $0.20.
Codex Fast and Ultrafast are not model ids. A rollout's `service_tier` of
`fast` bills the model's published Fast multiple of the standard row and
the row says Fast. That is 2× for most models; `gpt-5.5` is 2.5×, and
`gpt-5-mini`, `gpt-4.1`, `gpt-4.1-mini`, `gpt-4o`, `gpt-4o-mini`, `o3` and
`o4-mini` sit between 1.67× and 1.82× (`wiki/model-prices.md` has the
table). `ultrafast` bills 6× and only for `gpt-6-astra` and `gpt-6.1-sol`,
and the row says Ultrafast.
`claude-sonnet-5` is a prefix of `claude-sonnet-5-5`, and without its own row
Sonnet 5.5 would inherit Sonnet 5's cache reads at $0.20 against the
published $0.10 (2 / 10 / 0.10 / 2.50 / 4).

Fifteen models have **no published price at all** — `gpt-5.3-codex-spark`,
which is not on the API; `codex-auto-review`; `gemini-3.8-flash-lite`, never
published; and the retired and shut-down models. They report as unpriced and
are **named** rather than quietly inheriting the family rate, because a
plausible number nobody published is worse than an admitted gap.

What the table cannot express is **long context**. Above the threshold — 272k
for most OpenAI models, 200k for the 5.6 family, Grok and the Gemini Pros —
the *whole* request bills at roughly double, so one rate per kind understates
a long conversation. The published tables, with sources and what changed when,
are in [`wiki/model-prices.md`](../../../../wiki/model-prices.md).

### Setting a rate yourself

`,` → `rates` opens the rate card rather than a JSON box. One screen does the
finding and the choosing:

```
  2 with custom rates · tab or type to search all 68
   ▏ type to filter
 ▸ ✓ gpt-5.6-sol                       OpenAI
   ✓ claude-opus-5                     Anthropic
 ↵ open  tab show all  [d]efault  ↑↓ pick  esc done
```

It opens on the models you have set something on — the short list, and the one
you came back for — or on the whole card when you have none. `tab` switches
between the two, and typing searches all sixty-eight whichever view is on.

`↵` opens a model and lists its priced kinds, each showing the published price
as its **default**:

```
 ▸ o3 · input       —    2.0    unset
   o3 · output      —    8.0    unset
   o3 · cache_read  —    0.5    unset
```

`esc` goes back to the card with your search still typed, so setting three
models is three round trips and no retyping.

**Opening a model writes nothing.** An entry appears the moment you set a
number, and `[d]efault` takes it out again — the whole entry, numbers and all,
which is what putting a model back on list prices means. There is no separate
"selected" state to drift out of step with the prices: a model is yours when
it holds one.

**Set only the numbers you mean to change.** Config wins per kind, not per
model, so an override of `input` leaves output, both cache writes and cache
reads tracking the shipped card and still getting the vendor's next
correction. It used to replace the whole rate, which meant setting one number
deleted the other four — and a missing kind costs zero, so output metered as
free and the total was quietly a fraction of the real one.

`*` works the same way: it overrides those kinds for any model no other key
names, rather than replacing the card's rate for them.

The one thing this does not reach is a model with **no published price** —
`gpt-5.3-codex-spark` and the retired models. Those never inherit a family
rate however they are configured; set their kinds yourself and only those
kinds are priced, because the alternative is showing a number nobody
published.

A model the card does not carry can still be priced: add it to `rates` in the
file and it appears on the card with every kind and no defaults, which is the
one case worth editing JSON by hand for.

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

**That account-wide breakdown cannot be had honestly**, and it is worth saying
why rather than leaving the gap unexplained. The tools that show it get there
by being a browser — lifting a signed-in session out of one and loading the
dashboard page. Reaching into a browser's cookie store is a different kind of
program from one that reads an agent's own files, and this repo is not going to
become that quietly. So this pane shows what is on this machine, and says so
in the heading.

### Where each agent's numbers come from

**Cursor** uses the same dashboard event endpoint as CodexBar:
`POST https://cursor.com/api/dashboard/get-filtered-usage-events`. Both numbers
cover one snapshot, from local midnight 29 days ago through the fetch time:

- **30 days** sums `tokenUsage.totalCents` for events with tokens. Missing/null
  costs use Opscope's published list-rate table when it covers every token kind;
  explicit zero is retained and invalid costs are not estimated. Input, output,
  cache reads and cache writes are counted separately.
- **cursor meters** sums `chargedCents` from those same fetched events, including
  metered-only events with no tokens. A missing/invalid charge withholds the total.
- Request, unpriced and estimated counts and the snapshot time are shown. Unknown
  prices are not silently free. Partial priced windows say `at least`.

The fetch pages to completion (up to the same 200 × 1,000 safety ceiling as
CodexBar). Failed, inconsistent or capped histories show an error, not a total.
Only boundary duplicates proven by the API's total count are removed. The
existing Cursor CLI token supplies the subject and signed token for the dashboard
cookie, without importing browser cookies, switching accounts or refreshing
credentials. If the dashboard refuses it, the cost section says so. Other quota
and aggregate SPEND calls keep their existing Connect API transport.

This aligns the data source and arithmetic, not a guarantee of exact CodexBar
parity: accounts, refresh time, timezone and versions must match. CodexBar can
use a cached models.dev catalog and date-sensitive bundled rates; Opscope uses
its own bundled rates for omitted costs. Those estimates may differ. Neither
number is the subscription fee plus extra-usage payments.

**Claude** is costed from the **transcripts** rather than from Claude Code's
own summary file. That file holds one total per model per day, and input,
output, cache reads and cache writes differ in price by up to fifty times, so a
total cannot be costed at all. The transcripts carry the split, per message,
with a timestamp and a model — including the two cache durations, which are
priced differently, so **neither is assumed**.

Two things about reading them are worth knowing, because both once made a
figure too small or too large.

**Subagent work counts.** Subagent transcripts sit further down the tree, and
that is where Haiku and most of Sonnet actually run — an agent that only ever
appears as a subagent was missing from the costs entirely.

**A message is counted once.** Resuming or forking a session replays its
history into a new transcript, and a naive read bills the same turn twice; here
that overstated two models by double figures of percent.

With both right, the transcript totals reconcile against Claude Code's own
per-model figures. Where they differ it is because older transcripts have been
rotated away and Claude Code's summary still counts them — so the pane can read
a little under, never over.

**Copilot** groups its own local records by day and model, the same shape from
a much smaller source.

**Codex** attributes them the hard way: the model is not recorded against the
token counts, so each transcript is walked in order carrying the model forward
from the turn that named it.

That is also what makes the totals right. A Codex session spans several files,
each carrying a running total for the whole session, so taking the tail of each
file counts most sessions two or three times over. The figures here come from
the per-turn deltas instead, which reproduce Codex's own cumulative figure
exactly — and pick up the review model besides, which Codex's session total
never included at all.

**Grok** records no model against its tokens at all, so it can only be priced
by a `"*"` entry.

### What the plan saves

Cursor does not show savings: subtracting its two usage values does not account
for subscription fees or extra-usage payments. Other supported agents use a
configured plan cost:

```json
"agent_usage": {
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

The chart uses the same Cursor **dashboard event snapshot** and API-rate costs
as METERED. Partial/estimated pricing is labelled. It refreshes every half hour,
or on `r` subject to the shared refresh floor. The separate aggregate SPEND
section still uses Cursor's Connect API and can differ in coverage and freshness;
it is not used as the metered total or to calculate savings.

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
| **Claude** | the account | plan, member since, subscription status, rate-limit tier, billing type |
| **Cursor** | the account | plan name, price, included amount, who bills it |
| **Copilot** | the same account call as the quota | plan, seat date, organisation, sku, billing mode, enabled features |
| **Codex** | already in hand with the quota | plan type and credit balance — and that is genuinely all of it |
| **Grok** | the client log | tier, billing period, on-demand cap and prepaid balance |
| **Antigravity** | the account | Code Assist tier, Google AI plan, project, auth method — and no usage whatsoever |

Grok's tier moved out of its quota heading to join them, so no agent states
its plan in two different shapes.

Codex's section is three lines rather than six because three lines is all it
publishes. Its message estimates read zero here for a real reason — they are
derived from a credit balance, and the balance is zero.

A plan is held **for an hour**, not the two minutes the quotas get, because it
does not change between refreshes. A **failure is never held that long**: a
blanked subscription section looks exactly like an agent that publishes
nothing, and one rate-limited reading held for the full hour is how that
happened once.

Claude's section also degrades rather than disappearing. With the account
unreachable it falls back to what the local credential already says, which
needs no network at all, and labels itself `from credentials` so it is never
mistaken for the fuller reading.

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

**Copilot** would be exact, once there is data: it records its own timings per
turn, so nothing has to be inferred.

**Claude Code** is computed too, from a turn's output tokens over the time that
turn took. Even measured that way a few gaps come out impossible — timestamps
that plainly do not bracket generation — so **only the median and p90 are
shown, never a maximum**. The median barely moves however the outliers are
trimmed, which is the reason to trust it; the maximum moves by a factor of
twenty on the same data, which is the reason not to publish one.

It reads the most recent transcripts rather than all of them, so the figure
describes how the agent has been running lately.

**Cursor and Grok** record no tokens at all, so there is nothing to divide.

## Codex quota is the exception: it is real, live and account-wide

Every other number here is local consumption. Codex publishes an actual
**remaining quota**, and two ways to get it.

Codex leaves a quota snapshot behind in its own transcripts, which is real but
only as fresh as the last time Codex ran. The widget prefers to ask the account
directly, on the credential the CLI already holds, and **the header says `live`
or `from the last session`** so it is never ambiguous which you are looking at.

**Some features meter separately**, each with its own window and reset.
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

These extra limits are **live-only**: the snapshot left behind in the
transcripts carries the account-wide windows and nothing else, so a fallback
reading is missing the separately-metered lanes entirely — which is why the
source label matters. A lane reading 0% there is a **real zero from the
account**, not an absent number drawn as one.

Under the windows, the **BANK RESET** section is the unused rate-limit
reset credits still in the account, in the one layout the Claude tab uses
too. The title carries the count, as in `── 1 BANK RESET ──` or
`── 2 BANK RESETS ──`, and under it is one row per credit, soonest expiry
first:

```text
 ── 2 BANK RESETS ──
  Full reset · expires 7 Nov 2026 07:37 +08:00
  Full reset · expires 7 Nov 2026 07:37 +08:00
```

A row is the credit's `title` when it has one and `Reset` when it does not,
then the expiry as a date and time in this machine's local zone, the zone
named. Two credits ending at the same moment are still two rows. A credit
with no readable `expires_at` still counts, sorts after every dated one, and
says `· expiry not given`. A body that states a count and no list is that
many `Reset · expiry not given` rows, since no date was sent for any of
them. When a row is too wide for the pane its words stay together and the
datetime moves to the following lines, broken between its parts. It is not
another usage window, and it is not the `resets in` countdown on the quota
rows. Every one of these numbers and dates comes from
`GET /wham/rate-limit-reset-credits` on the same host and credential. A
credit counts only while it is still available and its expiry, when one
can be read, is still ahead. An inventory that answers `0` shows that none
are in the account. A request that does not answer leaves the section out.
The usage payload is not a source for the count or the dates. On the `[+]`
summary the Codex title stays `CODEX`. When the inventory was read and the
count is greater than zero, `N reset available` is indented under that
group, at the same inset a collapsed provider uses for a note under its
rows, with a blank line above it. The soonest expiry is added in the same compact
form as Claude (`23 Oct 04:58 +08:00`) only
when every listed credit has a readable expiry. Zero and an unread
inventory add nothing. The Codex tab's per-credit expiry keeps the full year
but also omits parentheses around the offset. Both tabs leave one empty
row between the section and whatever follows it.

The route to all this came from reading how
[CodexBar](https://github.com/steipete/CodexBar) does it — a menu-bar app that
covers twenty-odd providers, and the obvious thing to reach for if this ever
needs numbers that are not on disk.

## The live quotas, and the rule they follow

Claude, Codex and Cursor each publish a live quota, and each is fetched with a
credential the agent itself already holds — read-only, sent **only to that
agent's own host**, never printed, and never refreshed or rewritten. Claude's
OAuth token sits beside a refresh token that is deliberately left alone:
spending it would race Claude Code's own credential handling for a number that
has a local cache anyway.

Every one of them falls back rather than failing: Codex to the snapshot in its
own transcripts, Claude to a cached reading, Cursor to authorship alone. **The
header always says which you are looking at**, and where a fallback is all
there is, the reason the live reading is missing is the live reading's own —
a credential that was not there, or a service that did not answer — rather
than a sentence blaming a service that was never asked.

A reading is held for a couple of minutes, because these windows move over
hours and the pane redraws every thirty seconds. A failure is held too, so
something that is down is retried occasionally rather than on every frame.

Cursor, Copilot, Devin, Droid, Notion and Perplexity have no fallback of their
own, so a failure in transit - a timeout, a host that could not be reached, a
rate limit, a server error - keeps their **last good reading** on screen for
up to six hours rather than blanking it. The heading's `live` becomes
`kept 12m`, the row under it says `Not current` with the reading's own time
and this pass's reason, and the summary marks those lanes as cached. A refused
or missing token, or a different account, never brings an old reading back:
any failure that is not in transit drops what was kept.

### Claude's fallback is our own snapshot, not Claude Code's

Claude Code keeps a usage cache of its own, and this widget used to read it
whenever the live reading failed — with no age check, which turned out to
matter. Claude Code trusts that cache for an hour and ignores it after that;
on the machine this was found on the entry was **nine days old**, and the pane
was drawing it as a current percentage.

So there are two fallbacks now, newest wins:

1. **This widget's own snapshot**, written every time the live reading answers
   — minutes old on a machine in use, and not dependent on another program's
   cache still being maintained.
2. **Claude Code's**, but only inside the hour Claude Code itself trusts it
   for.

Either way the age is on screen, so a held figure is never mistaken for a
current one. The snapshot is marked with the account it was taken for, so
switching accounts does not show the old one's figures.

### Grok is the fourth, and it is on by default

Grok publishes no quota this widget can read without asking for it. With the
ask off, its figures come from the log its own CLI writes, so they move **only
when you use Grok on this machine**.

That failed quietly. A log left alone for nine days had the widget showing 23%
of a credit window that had closed on the 19th, while the account had spent 57%
of the window it was actually in. More than double, with nothing on screen to
say the figure was old.

Four settings, all on by default except the interval:

| key | default | what it does |
|---|---|---|
| `antigravity_start` | `true` | may the widget run the `agy` CLI to read the quota, when no running app answers. `agy -p /usage` on 1.1.11 or later - a command, not a prompt, so no tokens - in a private directory, its process group killed when the reading is taken; older builds are started under a pty instead. Never touches a CLI you started |
| `antigravity_remote` | `true` | may Antigravity's quota be asked of Google when the app is not running — same credential as its plan. Off turns only that ask off; starting `agy` is `antigravity_start`, and the tab has no app-closed quota only when both are off |
| `grok_ping` | `true` | may Grok's own service be asked for the live allowance, on the credential the Grok CLI leaves behind, **and** may that credential be refreshed so the asking keeps working |
| `grok_ping_minutes` | `15` | how often. The window moves over days, but the spend inside it moves while you work, so a quarter of an hour keeps the figure actionable. Thirty is the most it will take. A reading older than thirty-one minutes is drawn as cached, so the ceiling sits a minute under it and the freshest answer the widget can hold is always inside the window that judges it. A larger number in a hand-edited file is clamped, and the tab says the interval actually used |

### Notion AI asks Notion's web app, with a cookie you hand it

Notion meters AI on Business and Enterprise workspaces in two windows: a
rolling six hours, and the billing period. The `notion` tab draws both as
bars, each against the limit Notion states, with its reset; `[+]` draws them
beside every other agent's. The six-hour window is paced on its own length;
the billing period on the calendar month that ends where Notion says it
ends, since Notion gives the end and not the start.

- **There is no public API for it.** Notion shows this allowance only in
  its own Settings. The widget asks the two endpoints the web app uses,
  `getSpaces` and `getCreditRateLimitStatus`, which are internal and can
  change without notice. When one answers in a shape the widget cannot
  read, the tab says so rather than drawing an empty bar.
- **It needs your session cookie.** Copy `token_v2` from app.notion.com
  (developer tools, then cookies) into `notion_token`, or into the variable
  `notion_token_env` names (`NOTION_TOKEN_V2`). That cookie is a full
  sign-in to your Notion account, not a scoped key, so keep `config.json`
  `chmod 600`; the tab warns when it is not. The widget never reads it out
  of a browser or the Notion app, for the reason given above.
- **Notion is here when a token is.** It leaves nothing on this machine to
  discover, so with no token it has no tab, and with one it is asked every
  five minutes - and only while it is one of the agents you chose.
- **The allowance is per member**, in one workspace. `notion_workspace`
  picks it by id; empty takes the first on a Business or Enterprise plan. A
  Free or Plus workspace answers that it has no allowance, and the tab says
  that rather than showing a gauge.
- **Custom Agents are not in it.** Notion bills those in Notion credits,
  which this does not read.

| key | default | what it does |
|---|---|---|
| `notion_token` | `""` | the `token_v2` cookie, or a whole `Cookie` header copied from a request |
| `notion_token_env` | `NOTION_TOKEN_V2` | the variable read when `notion_token` is empty |
| `notion_workspace` | `""` | the workspace to report, by id; empty picks one that has an allowance |

These are the same two requests CodexBar makes for its Notion AI provider.

### Perplexity asks its account page's endpoint, with a cookie you hand it

Perplexity's usage page shows a weekly limit a plan includes and up to three
pools of credits: the plan's monthly credits, credits bought on top
(`credits`), and promotional bonus credits. The `perplexity` tab draws the
weekly limit first, with its reset, then one bar per pool that has anything
in it, and `[+]` draws them beside every other agent's. The heading names
the plan - Pro, Max - as the sign-in's session states it. Under the bars it lists the balance,
this period's usage and when the period ends, every credit grant on its own
row - what is left of it, when it was added, what was spent of it before
this period, and when it lapses - and the billing settings: auto top-up and
the spending limit. The monthly credits are paced on the calendar month
ending at the renewal Perplexity reports; bought and bonus credits have no
window to pace against, so neither has a pace.

- **There is no public API for it.** The widget asks
  `/rest/billing/credits`, the endpoint perplexity.ai's own usage page
  calls, which is internal and can change without notice. When it answers
  in a shape the widget cannot read, the tab says so rather than drawing an
  empty bar.
- **Amounts are credits.** The answer's fields are named in cents, but the
  usage page counts the same numbers as credits, one to one, so that is the
  unit drawn.
- **A grant that says what is left of it is believed.** Each grant carries
  `remaining_cents`, which counts every period it has been drawn on.
  `total_usage_cents` counts only this one, so spreading it over the pools
  - CodexBar's way - reads a grant bought last month and part spent then as
  untouched. Only when a grant does not say is usage spread in Perplexity's
  spending order: monthly first, then bought, then bonus. A bonus grant past
  its expiry is gone and is not counted. Usage the pools do not account for,
  and grants of a type the widget does not know, are said under the bars
  rather than dropped; beside an unknown grant no usage is spread at all,
  since that grant may hold it.
- **Four requests, each standing alone.** The credits come from
  `/rest/billing/credits`, the weekly limit from `/rest/billing/quota`, and
  the plan from `/api/auth/session`, and how it is billed - interval and
  next billing date, or the date a cancelled plan ends - from
  `/rest/stripe/subscription-billing-info`, whose retention-offer prices
  are not read. The weekly limit gives a share used and
  a reset, not the allowance's size, and is paced on the seven days before
  its reset. A plan with no weekly limit - which may be Max - answers none,
  so a quota answer of an empty object, or of its own fields set to null,
  leaves the bar out rather than reporting a fault; any other answer without
  a share is said as a fault; a session that names no plan leaves the heading bare.
- **It needs your session cookie.** To find it:

  1. Sign in at perplexity.ai and open developer tools: F12, or
     Cmd-Option-I on a Mac. Safari needs Settings, Advanced, *Show
     features for web developers* turned on first.
  2. Open the cookies for `https://www.perplexity.ai`: the **Application**
     tab in Chrome, Edge, Arc or Brave, the **Storage** tab in Firefox and
     Safari.
  3. Copy the Value of `__Secure-next-auth.session-token`. The console
     cannot read it, since the cookie is HttpOnly. If it is split into
     `.0` and `.1` cookies, copy the whole `cookie` request header from a
     request to `www.perplexity.ai` that includes the session cookie, in the
     **Network** tab instead.
  4. Press `,` and paste it into `perplexity_token`, or put it in the
     variable `perplexity_token_env` names (`PERPLEXITY_SESSION_TOKEN`, the
     one CodexBar reads too).

  The no-token panel, the expired-session panel, and the settings help for
  `perplexity_token` show these same steps; a test keeps those three in sync.
  That cookie is a full sign-in to your
  Perplexity account, not a scoped key, so keep `config.json` `chmod 600`;
  the tab warns when it is not. The widget never reads it out of a
  browser, for the reason given above.
- **`perplexity_token` wins over the variable.** The variable is read only
  when `perplexity_token` is empty, so a stale cookie left in `config.json`
  is the one sent even when the variable holds a fresh one - empty the
  field to use the variable. The tab's `token` line says which was used.
- **Perplexity is here when a token is.** It leaves nothing on this machine
  to discover, so with no token it has no tab, and with one it is asked
  every five minutes - and only while it is one of the agents you chose.
  So that discovery does not hide it altogether, the `[+]` overview ends
  with a line naming Notion AI and Perplexity while either has no token -
  unless you named your agents yourself or excluded it.
- **API (Sonar) spend is not in it.** That is billed to an API key, which
  this does not read.

| key | default | what it does |
|---|---|---|
| `perplexity_token` | `""` | the session cookie, or a whole `Cookie` header copied from a request |
| `perplexity_token_env` | `PERPLEXITY_SESSION_TOKEN` | the name of the environment variable to read the cookie from, only when `perplexity_token` is empty; `perplexity_token` wins when both are set |

The credits request is the same one CodexBar makes for its Perplexity provider.

### Devin asks app.devin.ai with a bearer token and an organization

Devin publishes a daily percent, a weekly percent, the reset instant for each,
and an extra-usage balance. `[+]` draws the two percents. Both rows carry a
pace mark against the length their key names — a day for `daily_*`, seven
for `weekly_*` — the same reading the Droid section below gives `fiveHour`.
The body sends the reset and not the length, so a reset that sits further
out than the named length draws no pace rather than a wrong one. The balance
is a number with no bar and no currency symbol, because the body names
neither a cap nor a currency. It is not a lane on `[+]`.

- **Daily disappears when `hide_daily_quota` is boolean true.** The percent
  may still be in the body. It is not drawn as 0%. A missing weekly field is
  the same: no bar, and the tab says the window was not in the answer. A 0
  that was sent is a real 0.
- **The percent is kept as sent.** A value below 1 is not multiplied by 100.
- **The credential is pasted, or the signed-in CLI's.** `devin_token` or
  `DEVIN_BEARER_TOKEN` (`DEVIN_AUTHORIZATION` is another name for that
  variable), and `devin_org`. With none of them set, the Devin CLI's own
  files are read: `windsurf_api_key` in its `credentials.toml` for the
  token and `devin.org_id` in its `config.json` for the organization -
  so a signed-in CLI is enough, and `devin_cli_credentials: false` turns
  that off. An internal id (`org-` or `org_`) is the path and the
  `x-cog-org-id` header. A slug is requested as `org/<slug>`. Nothing is
  read from a browser. Devin is here when both the token and the
  organization are set. The request goes only to `app.devin.ai`, and only
  while Devin is one of the agents shown. Keep `config.json` chmod 600;
  the tab warns when it is not.

A plan string is shown only when the body carried one at the top level.

### Droid asks Factory with an API key

Droid is Factory. Current accounts answer `GET /api/billing/limits` on
`api.factory.ai`: standard 5-hour, weekly, and monthly percents from
`usedPercent`, and the same three for Core when that pool has usage data.
The reset is `secondsRemaining` counted from the reading, or a `windowEnd`
that was still in the future. Pace is drawn on the 5-hour rows only — that
length is the key's name. Weekly and monthly have a reset and no pace. A
window the server left out is not a 0% bar. A percent that was sent is kept
even when `windowEnd` is already past; the countdown is what gets omitted.

Older accounts answer the subscription usage body instead. The heading says
**standard and premium tokens**, so those percents are not read as the
rate-limit windows. The percent comes from `usedRatio` when that ratio is
between 0 and 1, or from `userTokens / totalAllowance` when the allowance is
a positive number at or under one trillion. No ratio and no allowance is an
absent percent, not 0%. An allowance above one trillion is unlimited: no
bar, because there is no denominator. Org token totals, when the body sent
them, are a labeled count and are not added into the user percent.

Extra usage on a rate-limit account is `extraUsageBalanceCents / 100`, a
number with no bar. The line is omitted when the key was not in the body. A
legacy body has no such line. The subscription footer is the tier, the plan,
and the organization name from the auth body, when those fields were sent.

- **The key is a Factory API key.** `factory_api_key`, or `FACTORY_API_KEY`,
  or the `FACTORY_API_KEY` line in `~/.factory/.env` — the same file CodexBar
  reads. It is sent only to `api.factory.ai`, with `app.factory.ai` tried
  once when the API host fails for a reason other than a rejected key. A 401
  from the API host stays a bad key. Nothing is read from a browser, and the
  key is not decoded to invent a user id. Droid is here when a key resolves.
  The request runs only while Droid is one of the agents shown.

### Grok Bot (Cursor's weekly allowance)

Cursor grants a weekly included allowance for its Grok Bot, separate from
the monthly plan. It is not part of `total` / `cursor models` / `other
models` and does not share their reset, so it draws as a fourth bar carrying
its own countdown, and appears on `[+]` as its own lane.

It needs no configuration — it comes on the same credential as the three plan
lanes, so there is no browser session to paste into `config.json`.

It is **best-effort by contract**: a missing, refused or unparseable answer
leaves the three plan bars exactly as they were. An extra lane must never be
able to take the tab down with it.

The bar is drawn only when the account actually has an allowance, because
Cursor says so — a 0% bar for an account that was never granted one would
invent a limit that does not exist. When the lane is absent, the row says why
instead.

**One setting, not two.** The refresh was a second key for one release and
should not have been: the credential expires within days of the CLI last
running, so asking without refreshing works for a while and then silently
stops — the exact failure the refresh exists to prevent. Nobody wants the first
without the second, so turning on `grok_ping` turns on both.

**The refresh is keyed on the token, not on a session.** For a while it fired
only in the six hours after a session ended, and the token turned out to last
about six hours too — so anyone who had not run Grok since yesterday was in the
failure above with the fix switched on. It now also fires when the token is
about to lapse, whatever the last session was, subject to the same two guards:
`grok_ping` is on, and nothing is running that the CLI would start underneath.
It is attempted twice per expiry value — once shortly before the lapse and
once after — so a login that has genuinely run out costs two starts over the
life of its token rather than one every five minutes. Two, because the CLI
only renews a token that has already run out: measured on 1.0.25, started ten
minutes early it bootstrapped and left `auth.json` alone; started two hours
late it renewed at once. The early attempt is kept for a CLI that may one day
renew ahead of time. When both have been made and the token is still lapsed,
the row says so and asks for a sign-in rather than pointing at the CLI again.

**A lapse shows the last live reading, not an older log line.** The server's
last answer is kept apart from the poll slot, which every refusal overwrites,
and on a lapse or a refusal the row draws whichever is fresher — that or the
newest log line. The retained success is tagged with the token file's account
key, so a sign-in as someone else does not keep showing the previous account's
figure. Before this the log won outright, and on the machine this was found on
it put a figure from twenty-seven days back over one from two hours back, with
nothing but the cached mark to tell them apart.

**On by default**, because polling uses the credential the Grok CLI already
left, asked about your own account. A quota nobody can act on is not the
safer default. Off stays one key away and the tab names it.

**Cached or live is a question about age, not about source.** A reading is
shown as current when it was taken within the last half hour, whatever
fetched it — the same rule and the same half hour as Claude's. Marking by
source instead put a star on a figure thirty seconds old while a live one
four minutes old carried none, and here it was worse: the live answer was
being discarded (below), so the row read `not live` whether the ping was
working or not, and turning it on changed nothing a reader could see.

**A period without a percentage means nought used, not unknown.** Grok simply
leaves the figure out when it is zero, and the same answer proves it against
itself: the product that has been used carries a percentage while the two that
have not omit theirs, in the same breath. A window that had just reset reported
nothing and began reporting once anything had been spent. So nought here is the
reading rather than a guess, which is the only reason it may be drawn — and an
answer naming **no window at all** is still refused, because nought is only
knowable against a window somebody stated.

That split is drawn under the window, because the bar above is one number
for three different things and which of them is spending is the part a
reader can act on.

**The server's answer wins over the log.** Where the live answer names the
window but the log holds a percentage, the log's figure is used only if it
is about that same window — a percentage from a window that has closed is
not this one's. Before that rule the tab preferred an eleven-day-old log
line, about a window that had closed a week earlier, over the server's
current one, and rolled its reset forward with a `~`.

The screen says which state it is in, in both places it appears:

```
── WEEKLY QUOTA ── resets in ~1.1 days
 not live · from Grok's own log · window closed 5d 21h ago
 Only your own Grok sessions update it. agent_usage.grok_ping polls x.ai instead. — press `,` to set it here
```

```
── WEEKLY QUOTA ── resets in 1.1 days
 live · polled x.ai just now, every 5m
```

```
── WEEKLY QUOTA ── resets in 6d 19h
 live · polled x.ai just now, every 5m

 3%   ██┃░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░░  credits used
 12%  ████┃░░░░░░░░░░░░░░░░░░░░  on-demand $3 of $25
 window 26 Aug → 2 Sep
 by product GrokBuild 3.0% · GrokChat 0% · GrokImagine 0%
```

### On-demand is the allowance that costs money

Beside the included credits, Grok reports the **paid** usage: what has been
spent on demand, the cap it is spent against, and any prepaid balance.

It used to draw as a fragment on the window line — `on-demand 3/25` — which
meant the summary that ranks every allowance on the wall said nothing about
the only one that is billed. It now gets the credits
row's treatment on the tab, and a lane on `[+]` labelled **`on-demand $25`**:
`used / cap`, on the credits lane's own window so the two pace against one
clock, and marked stale exactly as the credits lane is, since both come out
of one reading. The cap rides in the label because a percentage of an unnamed
ceiling is not a number anyone can act on.

The lane draws **only where a cap is set**. No cap is the state of this
account rather than a zero to plot, and a 0% bar would say there is an
allowance sitting untouched when what is true is that there is none — the
refusal the Grok Bot allowance and Cursor's spend limit both make. The tab
says `no on-demand cap set` in its place, so an absent bar cannot be read as
an absent reading. Spend past the cap would be drawn as it came: full bar,
real numbers, no clamp.

The credit percentage and the cap are read independently, which matters for
accounts on unified billing: those publish no credit percentage at all, and
Grok used to drop off the summary entirely for that. An absent credit figure
now takes only the credit lane with it.

The prepaid balance is **a balance, not an allowance** — money on the account,
with no ceiling to be a percentage of — so it has no bar and stays as text
beside the window.

When asking is on and the figure still is not the server's, the row says
which of the reasons applies rather than leaving `not live` to cover all of
them — only some are the reader's to fix:

```
 not live · polled x.ai just now, every 5m · the token lapsed 3h ago - the Grok CLI refreshes it
 not live · polled x.ai 4m ago, every 5m · x.ai did not answer
 not live · polled x.ai just now, every 5m · x.ai sent no percentage for this period
```

The last of those is an answer that names the billing period but no figure
for it. The log's reading is kept, because it is the only percentage there is
— but it belongs to an earlier window, so the row stays marked `not live` and
its reset keeps the `~` that says the date is rolled forward rather than
stated.

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

Plain white would not do it — the agent hues are themselves light, so a white
mark disappears on a full bar, which is exactly where it matters. Giving the
mark's own cell a dark background instead makes it read identically on a full
bar, an empty track, or the boundary between them, and costs no width.

No mark is drawn when the window is unknown, or when a reading is **cached**
and its window may already have closed: a pace computed from a window that has
ended is arithmetic about nothing.

Cursor is the exception. Its three lanes are coloured as *categories* — total,
cursor models, other models, in the palette `cursor-agent` uses for the
same three — and a heat-coloured fill would overwrite that distinction to say
something its own dollar line already says.

## On colour

**Every quota bar is drawn in its own agent's colour**, dark at the left of the
fill and full at the right, so a row says whose it is before you read the
heading — and on the `+` tab, where every agent shares a screen, without one.
Claude keeps the terracotta of its own `/usage`, Codex its dark-grey-to-white,
Grok its blue, Cursor its green; Copilot and Antigravity have no calendar to
borrow from and were given hues clear of the amber and red reserved below.

Cursor's three lanes are **three tints of that one green** rather than three
unrelated hues. They are still categories, not a ramp, so they stay
distinguishable — but they now read as Cursor's, which three borrowed colours
never did.

All horizontal data bars use opscope-core's smooth gradient: quota lanes in
the overview and provider tabs, model/token/spend bars, and each category of
the human/AI split. The colour stops brighten across text cells, and the
fill ends to the nearest eighth of a cell. These bars are text in Kitty too:
the pace mark follows the clock, and a one-row image re-sent every frame
bought nothing visible. Core lifts darker hues to keep every filled colour above AA
4.5 against the dark terminal background and selected-row tint. Empty tracks
remain separate from the fill; the pace mark keeps its dark surround.

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

Claude Code's own summary file is spend-only — it carries no limit and no
reset. The quota block comes from somewhere else entirely, above.

## Scrolling

A tab is as long as it is — forty-five rows for Claude — and a pane on a wall
is rarely that tall. `↑` `↓` move through the body while the title, the tab bar
and the footer stay put, so you never lose which agent you are looking at.

```
 Checked 2s ago   · = detected   9-27 of 45 ▲▼
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
| `←` `→` / `tab` | switch agent. The new tab opens at its top: the tabs are different lengths and shapes, so a remembered offset opens the next one part-way down with its heading scrolled off |
| `↑` `↓` | scroll the tab |
| `pgup` `pgdn` | scroll a page |
| `home` `end` | jump to the top or bottom |
| `r` | re-read the files now, and ask each agent's endpoint again for any quota over 30s old (one that refused keeps its backoff) |
| `Ctrl-Y` `Ctrl-E` `wheel` | scroll the view a line at a time, which is what `↑` and `↓` do here too |
| `,` | open settings |
| `q` | quit |

## Cost

Small. Local files are looked at every 30 seconds. Each transcript, rollout,
session log and prompt history is read once, a line at a time, and after that
only what the agent has appended since. A live session's log can pass four
gigabytes, and none of it is ever held whole: what stays in memory is the
records each file yielded, a byte offset, and its first kilobyte. A file that
shrinks or is replaced is read again from the start, and one that is deleted
takes its records with it, so memory follows what is on disk rather than how
long the pane has been open.

The logs are also read less often than the quotas are asked. Every pass
looks only at the logs' metadata - a directory walk, no contents - and reads
them only when one has changed, at most every five minutes unless `r` asks;
the tab says how old its local figures are (`Local rollouts as of 3m ago`).
Claude's transcripts are only read if they were written within the METERED
window, `30 days` and a day's margin, since nothing older can reach those
rows; Codex and Grok show totals and a calendar over their whole history, so
every one of their logs is still read. A pass reads at most 64 MiB or 200 ms
of logs across every agent; a first read bigger than that carries on two
seconds later from the byte it stopped at, with the log it stopped in sent to
the back of the queue, and until it is done the figures say `so far` and how
many logs are left rather than calling themselves totals.

How far each log was read is written to
`$XDG_STATE_HOME/opscope/agent-usage-logs.jsonl` (or `~/.local/state/...`)
every five minutes while the logs change, and when a first read finishes, so
a restart reads only what was appended since rather than every log again. It
holds counters only - each file's identity, size and offset, a hash of its
first kilobyte, and the day, model and token counts its lines added up to,
keyed by the ids that stop a replayed turn counting twice - never a line of
any transcript. A log replaced while the widget was not running fails the
same identity check a running widget makes and is read again from the start;
a file of another format version is ignored. Deleting it costs one slow
start. A live quota
is held for a couple of minutes and a plan for an hour, so what a pane left
open all day asks of each agent is the same whichever tab is on screen.

The first paint is the slow one, because Cursor's spend history is paged
through before there is anything to draw. The local logs no longer hold it
up: the first pass reads its budgeted slice of them, and the rest fill in over
the passes that follow, labelled as partial until they are done.

## More than one Claude Max

A second Max seat is a second Claude Code config directory. This widget
does not invent that layout, does not read `$CLAUDE_CONFIG_DIR`, and does
not scan `~/.claude-*`. Name the directories:

```json
"agent_usage": {
  "claude_config_dirs": ["~/.claude", "~/.claude-overflow"]
}
```

`~` is expanded. Unset or empty is today's one directory, `~/.claude`, and
the tab and `[+]` group stay **CLAUDE** — a lone profile does not grow a
label to tell itself apart from nobody.

When the list is set, those directories are watched in that order and no
others. Duplicates collapse to the first entry.

### Naming a profile

An entry can also be an object, and then it names the label its tab and its
`[+]` group read. Both forms in one list:

```json
"agent_usage": {
  "claude_config_dirs": [
    "~/.claude",
    { "path": "~/.claude-work", "label": "work" }
  ]
}
```

`label` is optional; absent, empty or whitespace falls back to the label the
path implies. A path says where the files are, not whose seat it is, which
is the whole reason to write one.

It stays a **list**, not a map keyed by label: `serde_json` is taken here
without `preserve_order`, so a map would iterate alphabetically and the tab
order would stop being the order you wrote.

With **one** directory the label is not used at all — the tab and the group
read `CLAUDE`, exactly as they do with no list at all. A label on a
single-entry list is inert rather than an error, so one set today still
means something the day a second directory is added.

Two entries wanting the same name are told apart the same way two
directories with the same basename always were: the second becomes
`work-2`. Compared without case, because the tab strip uppercases.

**The settings screen builds these, and it is the only thing that does.**
Type a path to add an entry. Press `↵` on its row to open it as a screen of
its own — `path` and `label` as named fields — and `↵` on the empty box
opens that screen blank for a new one:

```
▸ path      "~/.claude-work"
  label     "work"
```

and the file gets `{ "path": "~/.claude-work", "label": "work" }`. In the
list that entry reads as `~/.claude-work = work` rather than as the JSON it
is stored as, because `{"label":"work","path":"~/.claude-work"}` is not a
row anyone reads at a glance.

**The box takes a path and nothing else.** It used to accept
`<path> = <name>` too, a second route to what the entry's own screen already
does — and a worse one: a punctuation rule to learn, invisible until
somebody guessed at it, in a box whose every other field takes a value
literally. A typed line carrying `=` is now refused and points at the row,
rather than being read as a path with an `=` in it — which is legal, almost
certainly not meant, and would draw on the pane as a profile that simply
found nothing.

A path cannot be listed twice under two names — the widget collapses
duplicate paths, so the loser would sit in the file forever doing nothing;
the refusal names the row to remove.

This was the one gap worth closing by hand: `claude_config_dirs` is the only
setting here whose items are objects, and the screen used to flatten the
shape to a plain string and quietly refuse half of it. A documented setting
reachable only by editing the file is not reachable from the screen that
exists to reach it.

**`~/.claude` is always CLAUDE.** It carries no label at all — an absent one
reads as the plain heading everywhere it is shown — and a label written on it
is ignored rather than refused, so one set today survives the directory being
moved down the list tomorrow. Labels are for telling the *extra* accounts
apart; the default is already told apart by being the one everybody has, and
renaming it would cost the familiar heading to buy a second name for the same
thing. No other entry may take that name either: a second one asking for
`claude` becomes `claude-2`, because two tabs reading CLAUDE would be telling
two accounts apart by nothing at all.

**What the pane does with two.** Each profile is its own tab, titled with
its label — the one the entry names, or the one the path implies: `claude`
for `~/.claude`, otherwise the directory's own name with any leading dot
dropped, because `.CLAUDE-BBI` on a tab reads as a stray character rather
than as a name. The strip uppercases them like the others and does not put
CLAUDE in the title. On `[+]` each is its own group, labelled `{label} -
CLAUDE`, ranked with the other agents; the default profile's group stays
plain `CLAUDE` rather than stuttering `claude - CLAUDE`. Two accounts never
share one bar.

Credentials, `stats-cache.json` and `projects/` are read from each
directory. The OAuth/app-state file is the sibling `{dir}.json` — the same
pairing as `~/.claude` beside `~/.claude.json` — or `{dir}/.claude.json` if
that is what is on disk. A custom dir never falls back to `~/.claude.json`.

CLI Proxy, Desktop and VS Code are out of scope.

## Which agents appear

By default it **discovers** them: an agent gets a tab when its CLI is on
`PATH` **or** it has left state behind. Both, because either alone is wrong —
a CLI installed under another name would vanish, and an agent uninstalled last
week still has history worth reading.

```json
"agent_usage": {
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

While discovery is on, those tabs stay hidden until that check has run, and
then only the agents it found are shown. An agent it did not find does not
get a tab. One it found still has its tab when the read after that comes
back empty or failed — the body says so, the tab does not go away. Turn
discovery off and name the agents, and those tabs are drawn from the first
frame, including while a read is in flight.

The header says how many detected agents the config is hiding, so discovery
stays visible rather than magic, and a name that matches no known agent is
called out — `unknown agent in config: nonsence (known: claude, codex, …)` —
rather than silently ignored. If an explicit list would leave no tabs at all
it shows everything instead, because an empty widget teaches nothing and the
likeliest cause is a typo.

The section used to be called `usage`, matching the old binary name. A
leftover section under that name is still read, and the pane says so —
`config section is still called usage; rename it to agent_usage` — so an
existing `config.json` neither goes silent nor needs a note someone might
miss.

Adding support for a new agent is one entry in `AGENTS`, giving the binaries
to look for and the paths that prove it has run.
