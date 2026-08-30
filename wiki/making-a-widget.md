# Making a widget

[← wiki](README.md)

How to add a widget to opscope. Nearly every rule here exists because
something shipped broken, and most are enforced by `widgets/tests/check.rs`.

## Before anything: does it have a real source?

**Every number on screen is real.** That is the founding rule and the one
worth defending. Widgets that could not be wired to a true source were deleted
rather than faked; `matrix` is the sole exception and computes nothing on
purpose.

So the first question is not "what would look good" but "what tells the truth
here, and can this binary reach it?" If the answer is a plausible-looking
simulation, stop.

Second question: **is it already filed?** The
[Linear project](https://linear.app/stealth-company/project/opscope-e829b47d84b8/issues)
tracks planned widgets and decisions already made.

## What ships must carry what it needs

Third-party crates are fine. A dependency the user must install separately
before the widget runs is not. The build absorbs them — `rusqlite` is taken
with `bundled` so SQLite compiles in, and `ldd` on a release binary shows only
libc, libm and libgcc. **A crate that wants a system library at run time is the
one kind that cannot come in.**

External *tools* are different and allowed. A widget needing `curl`, `ss`,
`ping`, `tailscale` or `herdr` either says so and stops (`tc::missing`,
`tc::cannot_start`) or carries on without what that tool would have told it.
It never pretends.

## One folder owns the widget

```text
widgets/src/widgets/<name>/
├── main.rs         Rust entry point and local tests
├── help.txt        `--help` summary and controls
├── README.md       data provenance, preview, keys, and configuration
├── CONFIGURE.md    plain guidance for an AI helping a user configure it
└── settings.json   defaults, field order, and field help (when configurable)
```

Private Rust modules stay in the same folder; `agent-usage` is the largest
example. `every_widget_owns_its_complete_folder` enforces the four required
files.

The public `opscope` binary is different. Its source is
`widgets/src/launcher/`: it launches widgets and embeds their help and README
previews, but is not counted or packaged as a widget. Its own `,` screen holds
only shared terminal behaviour such as mouse reporting.

## Adding one

1. Add the folder and its required files.
2. Add one `[[bin]]` entry in `widgets/Cargo.toml`.
3. Add `widget!("<name>")` to the launcher's registry in
   `widgets/src/launcher/main.rs`.
4. Add its row to the root `README.md` and to `docs/README.md`.
5. If configurable, declare `SETTINGS`, open it on `,`, and add
   `settings.json`.
6. Regenerate `config.example.json` with `tools/config-example.py`.
7. Run `cargo test`.

### What catches a missed step, and what does not

| step | caught by |
|---|---|
| `[[bin]]` entry | the compiler — it is how it builds |
| the four folder files | `every_widget_owns_its_complete_folder` |
| `README.md` row | `every_widget_has_a_readme_row` |
| `docs/README.md` row | `every_documented_widget_is_in_the_docs_index` |
| `config.example.json` | `generated_config_example_matches_widget_settings` |
| **the launcher registry entry** | **nothing** |

**The registry entry is the keystone, and nothing forces you to add it.** Add
it and the compiler then insists on `help.txt` and `README.md`, because
`widget!` `include_str!`s both. Skip it and everything still builds — the
widget simply never appears in `opscope`, which looks exactly like a widget
nobody wrote.

`docs/opscope.md` has a check, but it runs the other way:
`widget_names_in_the_launcher_sample_are_current` catches a name in the sample
listing that is *no longer* a widget — the thing a rename forgets. A new
widget missing from that listing is not caught.

So after `cargo test` passes, **run `opscope` and look for it in the menu.**
That is the only thing that proves the keystone went in.

## Polling without lying

Anything slow goes on a thread. **A background thread that dies is invisible**
— the pane shows no data and no error, indistinguishable from a source that
genuinely has none. `vercel-deployments` sat like that for a day.

Every poller records *why* it stopped, and that reason has to reach a row.
`a_poller_that_dies_records_why` flags a caught panic ending in
`unwrap_or_default()` on its own line, because that shape hands the pane an
empty list and draws a source with nothing in it.

Related: **never let a bare catch-all swallow a programming error.**
`discover_teams` turned a type error into "no teams found" and the board
quietly showed 3 projects instead of 21.

## Drawing

`opscope-core` holds the shared kit: `rgb()`, `bg()`, `mix()`, `heat()`;
`seg()` to clip a coloured segment to a cell budget and `pad()` to pad by
*plain-text* length; `pack_hints()` for footers; `follow()` for a window that
keeps a cursor in view; `vbars`, `vbars_down`, `stacked_bar`, `meter`,
`skeleton`; `get()`/`post_json()` over `curl`; `run()`/`run_full()` for bounded
commands; `clipboard()` over OSC 52.

Use `seg()` and `pad()` rather than `len()` — `len()` counts escape bytes and
produces ragged borders.

Braille line charts are **not** in core. `latency` and `link` each keep their
own `braille_canvas` and the two differ: latency's series carries the gaps a
ping can leave, link's is told how many slots the axis holds.

Three rules the tests measure:

- **Spend extra width on more content, not padding.** Add columns as a pane
  grows, drop them as it shrinks. **Never truncate.**
- **Never truncate a key hint.** `pack_hints()` wraps footers without
  splitting one, because `[±]25` teaches a key that does not exist.
- **A letter is not a verb on a screen with a text box.** Binding `d` to drop
  made every model with a `d` in its name unsearchable. Guarding it on an
  *empty* box is not enough either: empty is exactly when somebody types the
  first character of a new entry, so `docs.example.com` deleted something on
  its first keystroke. Where the box composes rather than filters, give the
  rows their own focus — `tab` crosses, typing crosses back, and the caret
  stops blinking on the box that is not listening.
- **Measure contrast, do not eyeball it.** Every text colour must clear WCAG AA
  against the terminal background *and* the selected-row tint. This was prose
  in `CLAUDE.md` and went unmet in four widgets for as long as there were four
  widgets — the failing grey spread from seventeen places to twenty-three
  while the issue sat open. That is what prose costs.

## Keys, scroll and the mouse

**The mouse scrolls the whole widget. Selection is keys.**

The wheel never moves a selection, changes a tab or picks a row — it scrolls
the body, as it would scroll a document. `every_widget_answers_the_wheel` and
`the_wheel_is_turned_off_on_every_way_out` enforce the mechanics, and the
first reads match arms rather than the file, so a comment claiming it scrolls
will not satisfy it.

| key | does |
|---|---|
| `wheel-up` / `wheel-down` | scroll the body one line |
| `k` / `j`, `up` / `down` | move the selection where there is one |
| `ctrl-y` / `ctrl-e` | scroll one line without moving a cursor |
| `pgup` / `pgdn` | scroll a screen |
| `home` / `end` | jump to either end |

Two traps, both paid for:

**Turn the mouse off on every way out** — not just `q`, the panic path too.
`Keyboard::restore()` sends `MOUSE_OFF` and `SCREEN_RESTORE` leads with it. A
widget that exits without it leaves the terminal emitting escape noise on
every scroll.

**Do not let a chart eat the slack.** `link` and `latency` scrolled nowhere
because their charts were sized `h - rows.len() - N`, absorbing every spare
row so the body was always exactly one pane tall. The same bug hid `latency`'s
event log and four of `github`'s sections. If scrolling does nothing, suspect
the chart before the scroll code.

Underneath it: **a pane too short is a pane you scroll, not a pane that hides
things.** A section that is not drawn looks exactly like a section with
nothing in it, and those are opposite readings of the same screen.

## Settings

Every configurable widget answers `,` in its normal navigation state. **The
widget ships data, core owns the screen** — `only_core_draws_the_settings_screen`
enforces that boundary, so a widget cannot grow its own settings UI.

The shared screen shows the current value and declared default, whether the
value is set or inherited, field help from `settings.json`, the resolved
config path `load_config` reads, and a warning when a legacy section is in use.

Arrows and `j`/`k` move the selected field; `ctrl-y`/`ctrl-e` and the wheel
move the viewport without changing what `↵` edits. `↵` edits or toggles, `d`
removes an override, `r` reloads, `a` opens a picker where there is one, and
`esc`, `q` or `,` returns.

**A widget that wrote something restarts itself on the way out.** A widget
reads its config once and builds everything from it — poll intervals, hosts,
which tabs exist — so a value written here cannot reach a process already
running. The screen used to say so and leave it to the reader. `run_settings`
now re-execs the binary when anything reached the file, which is one
behaviour for every widget rather than fourteen half-implementations, and it
happens at the one safe moment: on the way out, with the terminal being
handed back anyway. A widget needs no code for this and should not tell
anyone to restart.

**A token is masked and there is no key to unmask it.** There was one, and
nothing needed it: the value is in the file for anyone who has to read it,
while a settings screen that can put a live credential on a shared terminal
is a screen with a footgun on it. A declared *default* is never masked — it
ships in the repo, and hiding it would make an unset token read as though a
value were already there.

**In settings mode the footer shows only settings keys** — the widget's own
keys are not offered there, because a key that does nothing in this mode
teaches a control that does not exist.

The writer re-reads immediately before each mutation, changes only the
selected effective JSON value, validates the result, writes a private
same-directory temporary file with exclusive no-follow creation, then renames
it. Unrelated concurrent edits survive.

`config.example.json` is **generated** from every `settings.json`:

```sh
tools/config-example.py            # write it
tools/config-example.py --check    # part of cargo test
```

An optional `_schema` object beside the defaults carries UI-only constraints —
choices, element types, numeric bounds, nesting, units. The generator omits it
from `config.example.json`; it exists to stop the settings screen accepting a
value the widget would silently ignore.

### A field with a set of answers offers them

Declare `choices` on a single-valued field and `↵` offers them rather than
asking for one to be typed:

```json
"_schema": { "aggregate": { "choices": ["median", "mean", "min", "max", "p95"] } }
```

The set was already worth declaring — `validate_value` refuses anything
outside it — so a field that had one was rejecting wrong answers without ever
saying what a right one looked like. Choosing writes the value and returns,
because there is nothing further to say once one is picked.

An **array** with choices keeps its checklist. Ticking several is a different
act from choosing one, and `clocks.work_days` wants the first.

### A list of strings needs no declaration

An array of strings is filled in one entry at a time: the box composes an
entry, `↵` adds it, `[d]` on a row removes it. Nothing has to be declared —
`items: "string"` says so outright, and a shipped default that is a non-empty
array of strings says it just as well.

An array of *numbers* keeps the JSON box on purpose. `pomodoro_flash_rgb` is
one colour in three parts, not a list anybody adds a fourth entry to, and
offering to delete a row of it would be offering a two-component colour.

### When the options live in code

Some maps are keyed by something the widget already knows and cannot sensibly
restate in `settings.json` — `agent-usage`'s rate card is sixty-eight models
with their published prices. Copying that into the schema would be two records
of one fact, and the copy would go stale the first time a vendor moved a price.

So the widget hands the table over instead:

```rust
const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    ...
    catalogues: &[("rates", LIST_RATES)],
};
```

Each entry is `(key, who publishes it, the numbers each field defaults to)`.
The middle field is drawn beside the key and is **carried, not derived** — a
prefix rule reads fine over today's table and mislabels the first entry that
does not follow it. `o3` and `codex-mini-latest` are OpenAI's, and nothing in
either string says so.

`↵` on that field then opens one screen carrying both halves of the job:
the keys the reader has set something on, `tab` for the whole table, and
typing to search all of it. `↵` on a row opens that key's numbers, each with
the widget's own figure as the **default**, and `esc` comes back to the table
with the search still typed.

**Nothing is written until a number is.** There is no membership to keep in
step with the values — a key is the reader's when it holds one — and opening a
row to look at it leaves the file alone.

**Never seed an entry with the values.** Writing today's numbers into
somebody's config pins them there, and the correction the widget ships next
month never reaches them. The value stays absent until they change it, which
is also what makes "using default" on screen true.

That in turn requires the *reader* to merge per key rather than take the
configured object wholesale — `agent-usage` did the latter, so overriding one
price silently deleted the other four and those tokens metered as free.

Two things follow, and both have to be built deliberately:

- **An empty entry must be invisible to the reader.** One can still reach the
  file by hand, and it answers "configured" for a row of shipped defaults, or
  hands a key nobody filled in an empty rate — which prices at zero rather
  than reporting as unpriced. Both are the pane stating something untrue.
- **A widget that refuses to guess must keep refusing.** `agent-usage` names
  models with no published price so prefix matching cannot hand them a
  family's rate. Merging reopened that door: naming the model in config let
  the lookup run, and the substring match found the family behind it. The
  merge has to skip the card entirely for those, not just skip the guard.

A row whose widget has no figure to offer takes a `null` default, and
validation has to read that as "no type declared" rather than "null is the
type wanted" — otherwise the one case config exists for is the one case that
cannot be configured.

## Config

**Config, never hardcoded.** Hostnames, cities, tokens and account lists live
in `config.json` (git-ignored) via `load_config()`.

Read through `cfg_f64`, `cfg_usize`, `cfg_str`, `cfg_strings` — they take a
default by signature and cannot go wrong. A bare `cfg.get()` with no fallback
in the same statement is flagged: delete that key from `config.json` and the
widget lands on zero, or panics, instead of on its own default.

**Secrets never enter the tree.** This repo is public: no tokens, no internal
hostnames, no LAN addresses — in code, docs *or commit messages*. A secret scan
of the diff cannot see commit messages; scan `git log origin/main..HEAD`
separately.

The subtlest version: **the commit that removes a secret is the likeliest
place to restate it.** "The fixture used `<the actual name>`, which is a
device on this tailnet" is the most natural sentence to write while
documenting the fix, and says *more* than the fixture did. Describe the shape,
never the value.

`CONFIGURE.md` is documentation, not an agent skill and not authorization. It
ships inside the binary (`opscope <name> --configure-help`) and names real
data sources, safe inspection, secrets, and facts that must be asked rather
than guessed. It must never contain a credential, internal hostname, LAN
address, or a fabricated successful reading.

## Say what a number means

Label the window. Note when a counter resets. Never present a partial result
as a total. [Model prices](model-prices.md) is a worked example: an absent
rate and a zero rate are the same number for opposite reasons, and the page
says which is which.

## When a check fires, read the flag before believing it

The hint reader sees `[k]` wherever it falls, with rules keeping `[{}]`,
`[::1]`, `[[bin]]` and `args[0]` out; the glyphs `↵ → ← ↑ ↓`; the names
`esc tab enter backspace pgup pgdn home end`; and, inside a footer, a bare
single letter — which is what catches `or i to close`.

What it cannot see: a key named in prose in a non-footer string, or a key
answered anywhere other than a match arm or a `key ==` comparison. **Both
halves have been wrong before.** Three versions of this check cried wolf in one
day, and a checker that cries wolf gets turned off. When it is quiet, that is
not proof either.

The help reader is cruder and admits it: only a letter right after `press` and
a letter right before a verb count, because reading every letter took the `a`
out of "with a longer" and reported a key called `a`.

## Traps worth knowing before you hit them

- **A grep that finds nothing is as often a wrong pattern as an absent
  thing.** The most repeated mistake of the Rust port: `[a-z0-9]+` could not
  match the uppercase half of `"q" | "Q"` and reported 48 widgets broken; a
  config audit read line-by-line and skipped every multi-line `cfg\n.get(...)`
  chain, which is most of them. **Before believing a zero, run the pattern
  against a case you know it should match.**
- **A new check is green against your working tree, not against the repo.**
  One shipped passing here and failed on a clean checkout of its own commit,
  because the stale line it was written to catch had already been fixed in
  another session's dirty tree. Run it against `HEAD` — stash, or
  `git show HEAD:<path>` the files it reads.
- **A key can be wrong in a way only a real name exposes.** `agent-usage`
  priced Haiku 3.5 under `claude-haiku-3-5` for its whole life; the id is
  `claude-3-5-haiku-20241022`, so it matched nothing and priced nothing — and
  an unpriced model reads on screen as a model nobody used. Assert real
  recorded strings, not the names you assume.
- **Restart the pane after editing.** A running widget keeps the old code;
  compare process start time against file mtime before believing a fix works.
- **Do not edit by string slicing.** `s[:a] + new + s[b:]` has silently
  duplicated whole definitions here more than once.
- **`pgrep -f <pattern>` matches the shell running it** if the pattern appears
  in its own command line — which kills your own session. Use `pgrep -x`.
- **GitHub search returns at most 100 nodes per page.** Anything counting
  records must paginate or ask for `issueCount` aggregates — one rate-limit
  point per *request*, not per alias.

## Before you commit

```sh
cargo test    # from the root: widget tests plus check.rs
```

Then run `opscope` and confirm the new widget is in the menu.

## Where to read next

- [`docs/design.md`](../docs/design.md) — the visual language.
- [`docs/internals.md`](../docs/internals.md) — how core fits together.
- [`docs/port-decisions.md`](../docs/port-decisions.md) — what the port changed
  from the Python and why.
- [`docs/building-herdr-panels.md`](../docs/building-herdr-panels.md) — resize,
  focus, and the layout mistakes worth skipping.
- [`docs/releasing.md`](../docs/releasing.md) — read before touching
  `.github/workflows/`.
