# Shared readings

Several panes of one widget on one machine should ask a service once between
them, not once each. `tc::readings` (`core/src/readings.rs`) does that. It
began inside agent-usage (OPS-274), moved to core (OPS-283), and gained what a
fetch made of many requests needs (OPS-284). This page is for a widget
adopting it.

## The model

- **One store file per widget**, `store::path("<widget>")` under
  `tc::state_dir()`, mode `0600`. An existing file is restricted to that
  mode when it is opened. Widgets never share a file, so a widget
  can change what it stores without coordinating with another.
- **Leadership per reading, not per pane.** A stale reading is claimed with a
  short lease. The pane that wins fetches; the others keep drawing what they
  have and pick up what it publishes.
- **No heartbeat.** Publishing renews and releases the lease. A pane that dies
  leaves a lease that runs out; the next pane to find the reading stale takes
  over.
- **The newest reading wins**, by when its answer arrived, not by who wrote
  last.
- **A refusal is shared.** Every pane waits out a backoff, or a rate limit's
  reset, that one pane was told. A later gate does not shorten one still in
  force; the later deadline stays, and a longer one replaces it.
- **Followers read nothing until something changed.** `PRAGMA data_version`
  says whether another pane wrote; only then are rows read.

## The calls

| Call | What it does |
|---|---|
| `share(cache, name, caps, on, now)` | Open, keep or drop the store as `share_readings` says. Retries a store that would not open after `SHARE_RETRY` (10 min); reopens at once if the file was replaced under it. Call it at the start of each pass. |
| `follow(cache)` | Note whether another pane wrote. Call every `FOLLOW_EVERY` (5 s) on the poller thread. No network and no row reads, and it shows nothing by itself: run `take` after each follow tick, so a reading another pane fetched shows up within seconds rather than a full `refresh` later. |
| `key(cache, unit, shape, plain, secrets)` | `unit/shape:plain…:%sfingerprint`, with `:` and `%` in a plain part escaped so two sets of parts never make one key; the `%s` mark keeps a plain part from passing for a fingerprint. Tokens go in only as a SHA-256 fingerprint, and are registered so no reading containing one is ever written, in the raw form or as JSON escapes it. Only tokens of eight bytes or more are looked for: a shorter one would match ordinary text, so it is not a secret this check can keep. Bump `shape` when what is stored under `unit` changes. |
| `take(cache, key, Policy)` | `Serve(value)`, `Fetch(ticket)` or `Waiting { by, have }`. Never blocks. |
| `claim_many(cache, asks, now)` / `answer_many(cache, done, now, claimed_at)` | The same for a pass of many readings, in one commit each. `done` is `(key, answer, policy, fetched_at)`: each answer keeps the time its own fetch finished. `claimed_at` is when those leases were taken; an answer superseded after that is not written. An empty `done` writes nothing. `Claimed::{Serve, Fetch, Waiting}`, `Answer::{Value, Refused}`. |
| `renew(cache, key, lease_for)` / `renew_all(cache, key, lease_for)` / `release(cache, key)` | Between the two, for a fetch that is several requests. `renew_all` extends every lease this pane still holds and reports whether `key` is one of them. |
| `record(cache, key, value, at, held)` | Share a figure already in hand, at the time it was observed, without a lease. A newer stored reading is left alone. |
| `ticket.renew()` | Before each request of a long fetch. `false` means another pane took over: stop. |
| `ticket.publish(value)` | Hold it for this pane's `ttl` and share it. Returns why it stayed with this pane, if it did (too large, it carried a token, or its lease ran out and another pane took over - that pane's answer is the one shared). |
| `ticket.refuse(why, until)` | The service said no. Every pane waits until `until` if the service named a time, else the doubling backoff. |
| `gate(cache, bucket)` / `shut(cache, bucket, until, why)` | A rate limit that covers every reading of the service. `shut` when the service says the limit is spent; check `gate` before asking anything. A gate `shut` could not write because the store was busy is written by this pane's next `gate`. |
| `provenance(cache, key, now)` / `provenance_rows(found, now, width)` | Where the readings on screen came from. Empty when every one is this pane's and fresh. |
| `sharing_rows(cache, found, now, width)` | The line a widget draws: `not sharing readings: <why>` when this pane is polling alone, else `provenance_rows`. |

A dropped `Ticket` gives the lease back, including while a panic unwinds, so
another pane does not wait out a lease nobody is using.

`Policy::lease_for` is one request's timeout plus a margin, not a whole
pass: renewed before each request, a dead pane costs one request's wait. It is
capped at `lease::MAX_LEASE` (300 s).

agent-usage still uses the older `cached` call, which waits for another pane
when it has nothing to show. New widgets use `take`. `provenance` reads a
`cached` reading the same way: a pane that serves what it has while another
asks notes who is asking, and a refusal held as `{"why": ...}` is said with
the time its backoff ends (OPS-290).

## What the pane says

One row, wrapped to the pane, under the status line - never squeezed onto it.
Draw it with `sharing_rows`, so a pane that cannot share says
`not sharing readings: <why>` in the same place, not on the `!` screen.
Built from every reading on screen, and dated by the reading it speaks for:
the oldest one that is stale, refused or being fetched by another pane, so
the pane never claims to be fresher than a part that is behind; else, when
it only says who fetched, the newest such reading. A reading still inside
its hold - an hourly plan beside a quota read every minute - does not make
the line read old (OPS-290).

- `fetched 2m ago by pane 48213`
- `cached 7m ago · pane 48213 asking now`
- `cached 7m ago · too many requests, next ask in 3m`
- `… · too large to share`

A store started again, compacted or emptied says so on the next row for one
pass; the next `share` clears it.

The "updated" time a widget shows must be the reading's own `taken_at`, not
when this pane drew it.

## What it costs on disk

A reading is written when it changes, and a pass costs a few pages whatever
it holds (OPS-298):

- **A reading whose text is unchanged is not rewritten.** Its fetch time is
  kept in the `checks` table beside it, so a large reading costs a few bytes
  a minute rather than a copy of itself. `load` reports the later of the
  reading's own time and its check.
- **A widget's readings are claimed in one transaction and answered in
  another.** `claim_many` and `answer_many` (or `take` and `Ticket` for one
  reading at a time) are two commits a pass; one commit per reading was the
  whole cost of a quiet pass. A pass that fetches nothing writes nothing.
- **A good reading that is the value itself is stored once.** `publish`
  keeps the "last good" copy as a time, and `row` reads the value back for
  it. A later check of that same value moves the time, in the check stamp,
  so a follower's `kept` sees when it was last confirmed. `annotate` puts
  the old value back before it writes a reason over it, and matches the
  check time when the row's own time was left where it was.

Measured on github-actions, 18 repos, a 60 s refresh: 69 MiB an hour before,
with every reading rewritten every pass; about 2 MiB an hour quiet and
proportional to what changed after. A measurement test lives at
`core/src/readings/store.rs` (`write_bench`, run with `--ignored`).

## Rules that keep old and new builds together

Panes run for days, so an older and a newer build share a file after every
upgrade.

- A store of another `store::VERSION` is never deleted. The pane goes on
  alone and says why; the panes on the other build keep their file.
- A column that is added, nullable, never bumps `VERSION`. A change an older
  build cannot read takes a new file name instead.
- Only a file SQLite calls damaged is deleted and started again.
- A busy store means "not this pass", never "stop sharing".

## What is stored

Whatever the widget publishes, as JSON, in a file only its owner can read.
For a list widget that includes titles - pull requests, issues, runs - so the
widget's README says so, sets `Caps::forget_after` to hours (rows nobody has
published for that long are deleted, except a gate or refusal still holding
every pane off; it is the age at which routine cleanup deletes a row, not a
deadline, since the deleting rides on the next publish, and not a promise
either, since a file still over its size limit after vacuuming is emptied of
everything but gates in force; agent-usage keeps a day), and `share_readings: false` keeps everything in the pane.
`store::Caps` sets the file and per-reading size limits per widget.

## Observed format

`readings(key, taken_at, held, value, fails, good_value, good_at,
fetched_by)`, `leases(key, holder, expires_at)`, `meta(k, v)` with
`version = 1`. Unchanged since OPS-274.
