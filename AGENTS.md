# Working on terminal-toys

Linear team: <https://linear.app/stealth-company/team/TOY/overview>
Linear project: <https://linear.app/stealth-company/project/terminal-toys-e829b47d84b8/issues>

**Everything is tracked there** — planned widgets, the per-widget port
reviews, and the decisions waiting on William. Before starting anything,
look for the issue; before proposing something, check it is not already
filed and already decided against.

## What this repo is

Terminal widgets that look like sci-fi movie panels and show only real data.
Fourteen Rust binaries sharing `toys-core`, built with `cargo build
--release` from the root.

They began as Python scripts and were ported widget by widget; the Python is
gone, and `docs/port-decisions.md` records what the port changed and why.

The founding rule, and the one worth defending: **every number on screen is
real.** Widgets that could not be wired to a true source were deleted rather
than faked. `matrix` is the sole exception and computes nothing on purpose.

## Conventions

- **What ships must carry what it needs.** Third-party dependencies are
  allowed; a dependency that has to be installed separately before a widget
  runs is not. The build step is what absorbs them - `rusqlite` is taken
  with `bundled` so SQLite is compiled in, and `ldd` on a release binary
  shows only libc, libm and libgcc. Keep it that way: a crate that wants a
  system library at run time is the one kind that cannot come in. If a
  widget needs an external *tool*
  (`curl`, `ss`, `ping`, `tailscale`, `herdr`) it says so and stops, or goes
  on without what that tool would have told it - that is a different thing
  from a library and the rule is unchanged.
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

Run `cargo test` from the root. Alongside each widget's own tests it runs
`widgets/tests/check.rs`, which checks the things the compiler cannot, and
every check in it exists because something shipped broken and looked, on
screen, exactly like "there is no data":

- **a poller that dies without recording why** — a thread that stops is
  invisible, and the pane it feeds is indistinguishable from a quiet source.
  Recording it is not enough: the reason has to reach a row, and a caught
  panic ending in `unwrap_or_default()` is flagged on its own line, because
  that shape hands the pane an empty list and draws a source with nothing in
  it. Two widgets passed this check on accidents - one on the presence of
  `catch_unwind` alone, one on a Bresenham variable called `err`;
- **a footer hint naming a key no match arm answers** — a hint bound to
  nothing says the feature is there;
- **a footer hint missing from the widget's doc**;
- **a key the `--help` text names that nothing answers** — `--help` is where
  someone looks when the footer was not enough, and until this check went in
  nothing read those files at all;
- **a config key a widget reads that is not in `config.example.json`** — an
  undiscoverable setting is not a setting;
- **a section in the example no widget reads**, and separately **a key in the
  example the widget never reads** — checking the section alone passes a
  widget that reads three of its four keys and ignores the fourth;
- **a bare `cfg.get()` with no fallback in the same statement** — `cfg_f64`
  and its siblings take a default by signature, so they cannot go wrong; a
  raw `get()` can, and then a key deleted from `config.json` lands on zero or
  on a panic instead of on the widget's own default;
- **text on the selected-row tint measuring under AA 4.5** — the convention
  above, which was prose here from before the port and went unmet in four
  widgets for as long as there were four widgets. The number of places
  the failing grey reached a tinted row grew from seventeen to twenty-three
  while it sat open, which is what prose costs. Only colours that actually
  meet are compared: the first version paired every tint in a file with every
  grey in it and flagged two widgets that were fine;
- **a widget that defines a lighter grey and never reaches for it** — the
  contrast check measures the colour and cannot see the wiring. Delete the
  substitution inside the tint closure and the light grey sits in the palette
  measuring beautifully while the dark one goes back on the tint, so the
  substitutions are counted instead, one per closure that composes a tint.

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

The help reader is cruder still and admits it. Only two shapes count, a
letter right after `press` and a letter right before a verb, because reading
every single letter took the `a` out of "with a longer" and reported a key
called `a`. So in `Enter, i or c opens`, only `c` touches the verb and a
stale `i` beside it goes unseen — catching one of the two still lands the
reader in the right sentence.

`check.py` covered the same ground for the Python, plus unbound names -
which the compiler now makes impossible. It went with the Python.

## Gotchas paid for already

- **A background thread that dies is invisible.** Wrap every poller so it
  records why it stopped; otherwise the pane shows no data and no error, which
  is indistinguishable from a source that has none. `deployments` sat like
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
- **A build script that emits any `rerun-if-*` stops watching files.** The
  rule that cargo reruns a script when any file in the package changes
  applies only to a script emitting no `rerun-if-*` directive at all. One
  `rerun-if-env-changed` is enough to override it, so deleting the
  `rerun-if-changed` watches here did not make the version stamp always
  rebuild - it made it rebuild almost never, and `--version` reported a
  four-commit-old sha with a stale `-dirty` while the tree was clean. A
  full thirty-second rebuild did not move it. Watching nothing and watching
  everything are both wrong: unconditional reruns relink all fourteen
  binaries on every no-op build, measured at 28s against 0.04s. Watch the
  git files `git rev-parse --git-path` resolves - the only form that works
  in a linked worktree, where `.git` is a file - plus the source trees.
- **The commit that removes a secret is the likeliest place to restate it.**
  "The fixture used `<the actual name>`, which is a device on this tailnet"
  is the most natural sentence to write when documenting the fix, and it
  says more than the fixture did — it confirms the string is real and
  explains what it identifies. Describe the shape, never the value: *a
  fixture named a real device* is enough.

## Layout of the code

`toys-core` holds everything shared: terminal sizing, full-frame `draw()`,
24-bit `rgb()` and the green→amber→red `heat()` ramp, `seg()` for clipping
coloured segments to a cell budget, `pack_hints()`, `follow()` for a window
that keeps a cursor in view, bar and chart helpers (`vbars`, `vbars_down`,
`stacked_bar`, `meter`, `skeleton`), `get()` and `post_json()` over `curl`,
`config_token_warning()` for widgets holding a secret, non-blocking
`Keyboard`, and OSC 52 `clipboard()`.

Braille line charts are not in there. `latency` and `link` each keep their
own `braille_canvas`, and the two are not the same function: latency's series
carries the gaps a ping can leave, link's is told how many slots the axis
holds. Copy from whichever is closer rather than expecting core to have one.

`docs/port-decisions.md` records what the port changed from the Python and
why - the keys it consolidated, the three it renamed, the charts it draws
differently, and what was built afterwards on the Rust side alone. It is
history rather than a comparison now, and it is the answer to most questions
beginning *why does this key do that*.

`docs/building-herdr-panels.md` records what was learned driving these from
Herdr: resize semantics, focus, and the layout mistakes worth skipping.
