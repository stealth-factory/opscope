# Working on terminal-toys

Linear team: <https://linear.app/stealth-company/team/TOY/overview>
Linear project: <https://linear.app/stealth-company/project/terminal-toys-e829b47d84b8/overview>

## What this repo is

Dependency-free Python 3 terminal widgets that look like sci-fi movie panels
and show only real data. Each widget is a single executable script sharing
`common.py`; there is no package, no build step and no third-party dependency.

The founding rule, and the one worth defending: **every number on screen is
real.** Widgets that could not be wired to a true source were deleted rather
than faked. `matrix.py` is the sole exception and computes nothing on purpose.

## Conventions

- **Python 3.9+, standard library only.** No pip installs, ever. If a widget
  needs an external tool (`ping`, `tailscale`, `herdr`) it degrades gracefully
  when that tool is absent.
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

Run `python3 check.py`. It checks the things `compile()` cannot, and every
check in it exists because something shipped broken and looked, on screen,
exactly like "there is no data":

- **unbound names** — a missing import only raises when the line runs, and in a
  poll thread that means silence;
- **unguarded pollers** — a daemon thread that raises simply stops;
- **dead config keys** — a key in the example no widget reads;
- **missing docs / README rows**, and **footer keys absent from the doc**.

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

## Layout of the code

`common.py` holds everything shared: terminal sizing, full-frame `draw()`,
24-bit `rgb()`, `seg()` for clipping coloured segments to a cell budget,
`pack_hints()`, bar and chart helpers (`vbars`, `vbars_down`, `braille_plot`,
`stacked_bar`, `meter`, `skeleton`), `config_token_warning()` for
widgets holding a secret, non-blocking `Keyboard`, and OSC 52
`clipboard()`.

`docs/building-herdr-panels.md` records what was learned driving these from
Herdr: resize semantics, focus, and the layout mistakes worth skipping.
