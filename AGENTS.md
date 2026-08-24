# Working on terminal-toys

Linear team: <https://linear.app/stealth-company/team/TOY/overview>
Linear project: <https://linear.app/stealth-company/project/terminal-toys-e829b47d84b8/issues>

**Everything is tracked there** — planned widgets, the per-widget port
reviews, and the decisions waiting on William. Before starting anything,
look for the issue; before proposing something, check it is not already
filed and already decided against.

## What this repo is

Terminal widgets that look like sci-fi movie panels and show only real
data, in two implementations: Python 3 scripts sharing `common.py`, and a
Rust port under `rust/` sharing `toys-core`. The Python has no package and
no build step; the Rust builds fourteen binaries with `cargo build
--release`.

The founding rule, and the one worth defending: **every number on screen is
real.** Widgets that could not be wired to a true source were deleted rather
than faked. `matrix.py` is the sole exception and computes nothing on purpose.

## Conventions

- **What ships must carry what it needs.** Third-party dependencies are
  allowed; a dependency that has to be installed separately before a widget
  runs is not. The Rust has a build step that can absorb one - `rusqlite`
  is taken with `bundled` so SQLite is compiled in, and `ldd` on a release
  binary shows only libc, libm and libgcc. The Python has no build step, so
  in practice it stays on the 3.9+ standard library: there is nowhere for a
  pip install to be absorbed into. If a widget needs an external *tool*
  (`ping`, `tailscale`, `herdr`) it degrades gracefully when that tool is
  absent - that is a different thing from a library and the rule is
  unchanged.
- **Config, never hardcoded.** Hostnames, cities, tokens and account lists go
  in `config.json` (git-ignored) via `load_config()`. Add new keys to
  `config.example.json` in the same commit — and **use the section name the
  widget actually reads**; a mismatched key is silently ignored.
- **Secrets never enter the tree.** This repo is public. No tokens, no
  internal hostnames, no LAN addresses — in code, docs or commit messages.
- **Spend extra width on more content, not padding.** Add columns as a pane
  grows; drop them as it shrinks. Never truncate.
- **Never truncate a key hint.** `pack_hints()` wraps footers across lines
  without splitting a hint, because `[±]25` teaches a key that does not exist.
- **Measure contrast, do not eyeball it.** Every text colour must clear WCAG AA
  against both the terminal background *and* the selected-row tint.
- **Say what a number means when it is not obvious.** Label windows, note when
  a counter resets, and never present a partial result as a total.
- Every widget has a doc in `docs/` and a row in the README table.

## Before you commit

Run `cargo test` from `rust/`. Alongside each widget's own tests it runs
`widgets/tests/check.rs`, which checks the things the compiler cannot, and
every check in it exists because something shipped broken and looked, on
screen, exactly like "there is no data":

- **a poller that dies without recording why** — a thread that stops is
  invisible, and the pane it feeds is indistinguishable from a quiet source;
- **a footer hint naming a key no match arm answers** — a hint bound to
  nothing says the feature is there;
- **a footer hint missing from the widget's doc**;
- **a config key a widget reads that is not in `config.example.json`** — an
  undiscoverable setting is not a setting;
- **a section in the example no widget reads**.

The hint reader sees `[k]` wherever it falls, four rules keeping `[{}]`,
`[::1]`, `[[bin]]` and `args[0]` out; the glyphs `↵ → ← ↑ ↓`; the names
`esc tab enter backspace pgup pgdn home end`; and, inside a footer, a bare
single letter — which is what catches `or i to close`. On the other side it
reads match arms and `key ==` / `key !=` comparisons alike.

What it still cannot see: a key named in prose in a string that is not a
footer, and a key answered anywhere other than those two forms. Both halves
have been wrong before — three versions of this check cried wolf in one day,
and a checker that cries wolf gets turned off — so when it fires, read the
flag before believing it, and when it is quiet, that is not proof.

The Python keeps `python3 check.py` while it exists; it covers the same
ground for `*.py`, plus unbound names, which the Rust compiler makes
impossible.

## Gotchas paid for already

- **A background thread that dies is invisible.** Wrap every poller so it
  records why it stopped; otherwise the pane shows no data and no error, which
  is indistinguishable from a source that has none. `deployments.py` sat like
  that for a day.
- **Never let a bare `except` swallow a programming error.** `discover_teams`
  turned a `TypeError` from passing the wrong type into "no teams found", and
  the board quietly showed 3 projects instead of 21.
- **Restart the pane after editing.** A running widget keeps the old code;
  compare process start time against file mtime before believing a fix works.
- **Verify syntax with `compile()`, not `ast.parse()`.** Errors like `global X`
  after an assignment surface only in the symbol-table pass.
- **Do not edit by string slicing.** `s[:a] + new + s[b:]` silently duplicated
  whole class definitions here more than once. Use targeted replacements on
  unique anchors, then check for duplicate top-level defs.
- **`pgrep -f <pattern>` matches the shell running it** if the pattern appears
  in its own command line — that kills your own session. Use `pgrep -x`, or
  write PIDs to a file first.
- **Pad coloured strings by plain-text length.** `len()` counts escape bytes
  and produces ragged borders; that is what `seg()` and `pad()` are for.
- **GitHub search returns at most 100 nodes per page.** Anything counting
  records must paginate or, better, ask for `issueCount` aggregates — which
  cost one rate-limit point per *request*, not per alias.
- **A secret scan of the diff cannot see commit messages.** Scanning every
  added line before a push is right and still misses half the rule, because
  the rule covers code, docs *and* messages, and a message is not a diff
  line. Scan `git log origin/main..HEAD` separately.
- **A grep that finds nothing is as often a wrong pattern as an absent
  thing.** This was the most repeated mistake of the Rust port, on both
  sides: `[a-z0-9]+` could not match the uppercase half of `"q" | "Q"` and
  reported 48 widgets broken; a config audit read line by line and silently
  skipped every multi-line `cfg\n.get(...)` chain, which is most of them;
  another assumed the config variable was named `cfg` and declared six keys
  unread that are reached through `&gh` and `&raw`; a claim that netwatch
  ignored two keys came from grepping only for `tc::cfg_*`. Before believing
  a zero, run the pattern against a case you know it should match.
- **A new check is green against your working tree, not against the repo.**
  A check written beside an uncommitted fix is measured against the fix. One
  shipped passing here and failed on a clean checkout of its own commit,
  because the stale line it was written to catch had already been corrected
  in another session's dirty tree. Run a new check against `HEAD` — stash,
  or `git show HEAD:<path>` the files it reads — before believing it.
- **The commit that removes a secret is the likeliest place to restate it.**
  "The fixture used `<the actual name>`, which is a device on this tailnet"
  is the most natural sentence to write when documenting the fix, and it
  says more than the fixture did — it confirms the string is real and
  explains what it identifies. Describe the shape, never the value: *a
  fixture named a real device* is enough.

## Layout of the code

`common.py` holds everything shared: terminal sizing, full-frame `draw()`,
24-bit `rgb()`, `seg()` for clipping coloured segments to a cell budget,
`pack_hints()`, bar and chart helpers (`vbars`, `vbars_down`, `braille_plot`,
`stacked_bar`, `meter`, `skeleton`), `config_token_warning()` for
widgets holding a secret, non-blocking `Keyboard`, and OSC 52
`clipboard()`.

`docs/building-herdr-panels.md` records what was learned driving these from
Herdr: resize semantics, focus, and the layout mistakes worth skipping.
