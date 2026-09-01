# Working on opscope

Linear team: <https://linear.app/stealth-company/team/TOY/overview>
Linear project: <https://linear.app/stealth-company/project/opscope-e829b47d84b8/issues>

**Everything is tracked there** — planned widgets, the per-widget port
reviews, and the decisions waiting on William. Before starting anything,
look for the issue; before proposing something, check it is not already
filed and already decided against.

For a new widget, follow the ordered contributor guide in
[`wiki/making-a-widget.md`](wiki/making-a-widget.md). It is the source of truth
for the owned folder, launcher registration, settings, platform split, tests,
manual smoke test, and release handoff.

## What this repo is

Terminal widgets that look like sci-fi movie panels and show only real data.
Fifteen widget binaries plus the `opscope` launcher share `opscope-core`;
build all sixteen with `cargo build --release` from the root.

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
  in `config.json` (git-ignored) via `load_config()`. A widget owns its
  defaults and field help in `settings.json`; regenerate
  `config.example.json` with `UPDATE_CONFIG_EXAMPLE=1 cargo test` in the
  same commit.
  **Use the section name the widget actually reads**; a mismatch is silently
  ignored.
- **Secrets never enter the tree.** This repo is public. No tokens, no
  internal hostnames, no LAN addresses — in code, docs or commit messages.
- **`cfg` decides where bytes come from; nothing else.** Three tiers, because
  gating a parser with `cfg(target_os)` hides its tests from the macOS CI
  run and a broken Linux parser sits behind a green build:
  1. **Always compiled, always tested — every parser.** Pure functions from
     text or bytes to values, named `parse_*`, taking a `&str`. They live
     in `widgets/src/widgets/<widget>/parse.rs` so they compile on every target.
     The cost is a few KB of unused parser in each binary. Worth it.
  2. **`cfg(target_os)` — acquisition only.** Which file to open, which
     command to spawn, a call into a platform C API. `linux.rs` / `macos.rs`
     beside `main.rs` in the widget's package folder, gated on `mod host`.
     Shared wording lives in
     `opscope-core`: `unsupported()` is `does not run on {os}`, drawn by
     `cannot_start_because()`.
  3. **Runtime detection — anything that varies *within* a platform.** A
     tool on `PATH`; whether `ping` accepts `-O`; whether the kernel has
     PSI. `cfg` cannot see any of it, and a build-target check would be
     wrong on the machine that matters.
- **Spend extra width on more content, not padding.** Add columns as a pane
  grows; drop them as it shrinks. Never truncate.
- **The mouse moves the view. Keys move the selection.** The wheel does a
  full-widget scroll and nothing else: the viewport slides, `selected` stays
  exactly where it is even when that takes it off screen, and a widget with
  sections does not change focus either. Scrolling to look at something must
  never change what `↵` opens. `Ctrl-Y` and `Ctrl-E` do the same from the
  keyboard, as in vim, so the feature is reachable without a mouse. Keys move
  the cursor and the window follows it — but only on the frame a key moved it,
  or the follow drags the view straight back from wherever the wheel put it.
  `every_widget_answers_the_wheel` in `check.rs` fails when a widget does not,
  and it reads match arms rather than the file, so a comment saying it scrolls
  will not satisfy it.
- **A pane too short is a pane you scroll, not a pane that hides things.**
  Every frame is a window onto a body built at whatever height it needs, with
  the title pinned above it. Nothing stands down for want of rows: a section
  that is not drawn looks exactly like a section with nothing in it, and those
  are opposite readings of the same screen — *nothing has gone wrong* against
  *you cannot see whether anything has*. Four widgets sized a chart to
  whatever the pane had left, which hid `latency`'s event log, `link`'s chart
  and four of `github`'s sections, and left the body exactly one pane tall so
  there was nothing for the wheel to reach.
- **Never truncate a key hint.** `pack_hints()` wraps footers across lines
  without splitting a hint, because `[±]25` teaches a key that does not exist.
- **Measure contrast, do not eyeball it.** Every text colour must clear WCAG AA
  against both the terminal background *and* the selected-row tint.
- **Say what a number means when it is not obvious.** Label windows, note when
  a counter resets, and never present a partial result as a total.
- Every widget is one folder under `widgets/src/widgets/<name>/`, containing
  its `main.rs`, `help.txt`, `README.md`, `CONFIGURE.md`, and `settings.json`
  when configurable. It also has a row in the root README table.
- Every configurable widget opens the shared settings screen with `,`.
  `CONFIGURE.md` is plain AI-facing documentation, not an executable skill.
- `widgets/src/launcher/` is the source for the public `opscope` command.
  It is the launcher, not a widget, and does not count as one.

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
- **a parser or a test gated by `cfg(target_os)`** — CI runs `cargo test` on
  the macOS runners. Anything behind `cfg(target_os = "linux")`, including
  its tests, does not compile there, so it is not merely unrun, it is
  invisible. Parsers are named `parse_*` so the check can see them.
- **a widget that opens `/proc` with no macOS path and no explanation** — an
  empty table on a Mac looks like a machine with nothing listening. The
  widget needs a `macos.rs` beside `main.rs`, a call to `unsupported()`, or a row on the
  allowlist that names the issue still open.

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
  is indistinguishable from a source that has none. `vercel-deployments` sat like
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
- **A row wider than the pane is worse than a row that was cut.** Anything
  built through `seg()` is clipped and safe; a row assembled from prose is
  not. `months`' reckoning line overflowed a 26-column pane, the terminal
  wrapped it, and the frame's row count was then a row short — which scrolled
  the *pinned title* off the top and looked like a widget that had lost its
  header rather than a sentence that was too long. `pack_hints()` wraps a
  sentence as happily as a footer if each word is handed to it as a hint, and
  a single word longer than the pane still has to be broken by hand. Measure
  every row of a frame against the width it was built for, at every width —
  the bug appears only below about thirty columns, which is not where anyone
  looks.
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
- **A tool-installer action can break on a day nothing here changed.**
  `taiki-e/install-action` had no manifest for the pinned git-cliff, fell
  back to whatever `cargo-binstall` it could fetch, and that one was old
  enough not to parse a `Cargo.toml` saying `edition = "2024"`. The release
  job died before reading a commit. A pinned release URL and a `sha256sum
  -c` in the same step cannot drift like that, and they say exactly what
  ran.
- **`$( )` strips every trailing newline, so you cannot pad one on.** A PR
  body came out as `---## [0.1.1]`, and the fix — appending empty strings
  to the `printf` inside the substitution — could not possibly have worked
  and shipped anyway, because it was read rather than run. Join the pieces
  explicitly: `printf '%s\n\n%s\n' "$a" "$b"`.
- **A `#` comment inside a line continuation is an argument, not a
  comment.** The backslash joins the lines first, so the `#` and everything
  after it reach the command. It looks completely ordinary in a diff.
  `bash -n` over every `run:` block in every workflow catches it in about a
  second.
- **`gh pr view <branch>` finds the most recently *merged* PR when no open
  one exists.** So a create-or-update that asks it takes the update path
  after the first release, rewrites the closed PR, opens nothing, and
  reports no error — the next release is simply never offered. Ask
  `gh pr list --state open` and act on the number. `--jq '.[0].number'`
  needs `// empty` beside it, or jq prints the four characters `null`,
  which is a non-empty string that lands on the same wrong branch.
- **`gh pr edit` and `gh pr create` ask GraphQL for the org's `login`,
  `name` and `slug`.** A classic PAT with `repo` can force-push the
  release branch and still die on those two commands, wanting `read:org`.
  The job that first used `GH_TOKEN` pushed, then failed, leaving the
  branch moved and the PR title untouched. `PATCH`/`POST` `/pulls` do
  not ask for the org.
- **Under squash merge a branch is permanently ahead of main.** The
  squashed commit is a different object, so comparing commits can never
  tell you a branch merged — a cleanup written that way refuses to delete
  anything and reports every branch as unmerged. Ask the pull request.
- **Name the shell by path in a test.** A release build failed on a macOS
  runner with `sh: No such file or directory`; the shell was there and the
  PATH was not. Three targets built, one test failed for a reason it was
  not testing, and the tag was left with no release on it. A test that can
  fail for a reason it does not test teaches people to press the button
  again until it goes green, which is how a real failure gets waved past.

## Layout of the code

`opscope-core` holds everything shared: terminal sizing, full-frame `draw()`,
24-bit `rgb()` and the green→amber→red `heat()` ramp, `seg()` for clipping
coloured segments to a cell budget, `pack_hints()`, `follow()` for a window
that keeps a cursor in view, bar and chart helpers (`vbars`, `vbars_down`,
`stacked_bar`, `meter`, `skeleton`), `get()` and `post_json()` over `curl`,
`config_token_warning()` for widgets holding a secret, non-blocking
`Keyboard`, OSC 52 `clipboard()`, and `unsupported()` /
`cannot_start_because()` for a widget that has no source on this kernel.
It also owns the shared per-widget settings screen; widgets provide only
their section name, optional legacy alias, and owned `settings.json`.

A widget that acquires per OS lives in its package folder
`widgets/src/widgets/<widget>/{main,parse,linux,macos}.rs` — the same
folder that holds `help.txt`. Parsers always compiled (`mod parse`);
`mod host` gated by `cfg(target_os)` onto `linux.rs` / `macos.rs`. `ports`
is the worked example; drop the same three files beside `main.rs` to give
another widget a second source.

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

`docs/releasing.md` is the release pipeline: what decides the version, what
merging the release pull request sets off, and the parts that look broken
but are not. Read it before changing anything under `.github/workflows/` -
the four files there depend on each other in ways that are not obvious from
any one of them.
