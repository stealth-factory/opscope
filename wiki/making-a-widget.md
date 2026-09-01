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
├── main.rs         Rust entry point, UI, and local tests
├── parse.rs        platform-independent parsers and their tests (when the widget parses source text)
├── linux.rs        Linux acquisition only (when sources differ by OS)
├── macos.rs        macOS acquisition only (when sources differ by OS)
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

## Platforms: `cfg` decides where bytes come from

Keep the parser visible to every build. The platform split has three tiers:

1. **Parsers are always compiled and always tested.** Put pure functions from
   text or bytes to values in `parse.rs`, name them `parse_*`, and take the
   input as `&str`. Never put a parser, its module, or its tests behind
   `cfg(target_os)`: the macOS CI build would otherwise never compile a Linux
   parser or its tests, so broken Linux code could sit behind a green build.
2. **Use `cfg(target_os)` for acquisition only.** Opening `/proc`, spawning a
   platform command, or calling a platform C API belongs in `linux.rs` or
   `macos.rs`. Select those files as one `host` module from `main.rs`:

   ```rust
   #[cfg(target_os = "linux")]
   #[path = "linux.rs"]
   mod host;
   #[cfg(target_os = "macos")]
   #[path = "macos.rs"]
   mod host;
   ```

   Both acquisition modules feed the same platform-independent state and
   drawing code. Keep shared wording and behaviour in `opscope-core` rather
   than duplicating it between hosts.
3. **Detect differences within an OS at run time.** Whether a tool is on
   `PATH`, whether `ping` accepts a flag, and whether a kernel exposes a
   feature cannot be decided by the build target. Probe them when the widget
   runs and either use the result or explain what is unavailable.

If an OS has no truthful source, do not draw an empty table. Return
`tc::unsupported()`, whose reason is `does not run on {os}`, and render it with
`tc::cannot_start_because()`. That makes an unsupported widget visibly
different from a supported source returning no rows. A source that exists
on this OS but failed to open is still an error, not `unsupported()`.

`ports` is the worked example: its package contains `parse.rs`, `linux.rs`,
and `macos.rs`. Linux reads `/proc/net/tcp` and `/proc/net/tcp6`; macOS runs
`lsof` (and `ps` / `nettop` for the rest). Both hosts pass that text to
parsers that compile and run their tests on every target. Copy that shape into
the widget's own folder rather than creating a shared platform crate. The
reasoning behind this boundary is recorded in
[OPS-65](https://linear.app/stealth-company/issue/OPS-65/decide-how-platform-specific-code-is-split-and-compiled-in).
This contributor-facing platform contract was added under
[OPS-71](https://linear.app/stealth-company/issue/OPS-71/document-linuxmacos-splits-in-the-widget-creation-wiki).

Two repository checks defend the boundary:

- `parsers_and_their_tests_are_not_gated_by_target_os` rejects platform-gated
  `parse_*` functions and tests.
- `a_proc_reader_has_a_macos_path_or_says_why` rejects a `/proc` reader with
  no `macos.rs`, no `unsupported()` call, and no row on `LINUX_ONLY_UNTIL`.
  Passing is not proof of a reachable unsupported screen: the check looks
  for those three shapes, and an allowlisted widget is still waiting on a
  macOS path.

## Adding one: the complete path

Follow these steps from the repository root. The examples use
`example-widget`; replace it consistently.

### 1. Choose the name and source

Confirm the widget and its decisions are already represented in the
[Linear project](https://linear.app/stealth-company/project/opscope-e829b47d84b8/issues).
Choose a lowercase hyphenated name that does not shadow a normal shell command.
Write down the real source for every figure before drawing it.

### 2. Create the owned folder

Create `widgets/src/widgets/example-widget/` with all four required files:

- `main.rs` — acquisition, state, drawing, input, and local unit tests;
- `help.txt` — the launcher's summary and explanation, plus CLI usage and keys;
- `README.md` — the maintained preview, provenance, controls, and settings;
- `CONFIGURE.md` — safe, plain-Markdown guidance for a person or AI assistant.

Add `settings.json` only when the widget is configurable. Add `parse.rs`,
`linux.rs`, and `macos.rs` when the platform section above calls for them.
`every_widget_owns_its_complete_folder` checks the four required files, the
Cargo path, the embedded configuration guide, and the settings shape.

In `main.rs`, every widget must expose its two embedded help documents before
starting the terminal:

```rust
use opscope_core as tc;

fn main() {
    tc::maybe_widget_help(
        include_str!("help.txt"),
        include_str!("CONFIGURE.md"),
        false, // true when this widget has settings
    );

    tc::setup();
    // Acquire, draw, and answer keys here.
    tc::restore_screen();
}
```

Use `matrix` as the smallest non-configurable example and `ports` as the
worked configurable, platform-split example. A real poller also needs the
failure handling described under [Polling without lying](#polling-without-lying).

### 3. Give the launcher parseable help and a preview

`help.txt` has a maintained shape because the launcher parses it:

```text
One-line summary shown on the launcher row.

One introductory paragraph explaining why the widget exists. The launcher
uses this paragraph as its aside, up to the next blank or indented line.

    example-widget [-n SECONDS]

Keys: up/down select, q quits.
```

Line one and the introductory paragraph must both be non-empty. Put the
indented usage synopsis after that paragraph; do not let `Keys:` enter the
introductory paragraph. The binary returns this whole file for `--help`, and
`every_key_the_help_text_names_is_answered` checks the key phrases it can
recognise against `main.rs`. The sample above is the non-configurable set;
a configurable widget also names comma here (`ports/help.txt` is the shape).
Do not advertise `,` unless the widget actually opens the shared settings
screen.

In `README.md`, make the **first fenced block** a static picture of the widget.
Its first line must begin with `╺━`; an earlier shell or JSON fence makes the
launcher see that instead and reject the page. This is the copyable shape:

````markdown
# `example-widget`

[← all widgets](../../../../docs/README.md)

What this widget answers in one sentence.

```text
╺━ EXAMPLE WIDGET ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 real-looking layout, clearly maintained as an example
 [q]uit
```
````

The launcher embeds that first preview without starting the data source.
`a_sample_is_a_picture_of_the_widget` checks the `╺━` opening for every
registry entry. Document every footer key in the README; the repository checks
both that a hinted key is handled and that the README names it.

`CONFIGURE.md` must identify the real sources, safe inspection steps, the
owned settings section (or explicitly say there is none), secrets and other
facts that must be asked rather than guessed, and how to verify an honest
answer. It is documentation, not executable authority.

### 4. Choose the settings path

For a **non-configurable** widget, omit `settings.json`, pass `false` to
`maybe_widget_help`, define no `SettingsSpec`, and do not call `run_settings`.
Its README and `CONFIGURE.md` should explicitly say that it has no settings.
`matrix` is the worked example.

For a **configurable** widget, add `settings.json`, pass `true` to
`maybe_widget_help`, and declare the shared screen in `main.rs`:

```rust
const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "example-widget",
    section: "example_widget",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};
```

The section is the widget name with `-` changed to `_`; it must also be the
section passed to `load_config()`. `legacy_section` is only for a real renamed
section that existing users may still have—otherwise use `None`. In the normal
navigation match, the literal comma opens core's screen:

```rust
"," => {
    tc::run_settings(&mut keyboard, SETTINGS);
    continue;
}
```

Document the settings key as literal `` `,` `` in `README.md`, mention it in
`help.txt`, and let core draw the screen. A widget must not implement its own
settings UI.

Every non-metadata field in `settings.json` is a real default and needs
field-specific help. The preferred key is `_<field>_comment`; the checker also
accepts the older `_comment_<field>` and `_<field>` forms. `_comment` describes
the whole widget and does not replace per-field help. Put validation-only
rules under the top-level `_schema`, beside the defaults:

```json
{
  "_comment": "What the widget reads and reports.",
  "_schema": {
    "hosts": { "items": "string" },
    "refresh": { "minimum": 1 }
  },
  "_hosts_comment": "Hosts to measure.",
  "hosts": [],
  "_refresh_comment": "Seconds between samples.",
  "refresh": 4.0
}
```

Every array must declare `items` (or a picker), including one that already
ships a non-empty default. The settings screen can infer a list editor from
that default, but `every_array_declares_what_it_holds` still requires the
declaration so emptying that default later cannot steal the editor. `_schema`
never appears in `config.example.json`.
Repository checks also require every declared field to be read, every read
field to be declared, code fallbacks to match the declared defaults, token
environment names to match, and dynamic catalogues to name real fields.

### 5. Register and index it

Add one `[[bin]]` to `widgets/Cargo.toml`:

```toml
[[bin]]
name = "example-widget"
path = "src/widgets/example-widget/main.rs"
```

Then add `widget!("example-widget")` to `WIDGETS` in
`widgets/src/launcher/main.rs` **in alphabetical order**. Cargo defines what
ships; `every_binary_is_on_the_menu` and
`every_widget_is_on_the_launcher_menu` enforce that every non-launcher binary
is registered as a widget, while `the_list_is_in_a_settled_order`
enforces the alphabetical launcher order.

Bump the expected `[[bin]]` count in `npm/test.js` (`the packer takes every
[[bin]], including opscope`). That number is sixteen today — the launcher
plus fifteen widgets — and it is a gate, not a reading of the manifest:
`cargo test` runs it whenever Node is on PATH, and CI always does. Skip
it and the new binary builds while the packer still expects the old
inventory.

Add the widget to the root `README.md` table and to `docs/README.md`. The
repository checks both indexes. Do not add a second copy of the summary or
preview to the launcher: it embeds `help.txt` and `README.md` from the owned
folder. [The launcher documentation](../widgets/src/launcher/README.md)
explains that boundary.

### 6. Implement input, scrolling, and honest failure

Footer hints, README controls, `help.txt`, and key match arms must agree. The
wheel moves the full-widget viewport without changing selection or focus;
`ctrl-y` and `ctrl-e` expose the same movement from the keyboard. Every new
widget handles `wheel-up` and `wheel-down` unless it genuinely has no
scrollable body. That exception requires an explicit, reviewable entry and
reason in `NO_SCROLL` in `widgets/tests/check.rs`; `matrix` is the only current
exception.

Detect external tools at run time with `tc::missing()`. Use the shared
`tc::cannot_start()` / `tc::cannot_start_with_settings()` helpers when a
required tool is absent, `tc::missing_config()` for a configurable input that
is absent, and `tc::cannot_start_because()` with `tc::unsupported()` when the
kernel has no truthful source. Do not copy a one-off failure screen or turn a
failed command into an empty result.

### 7. Generate configuration and run the gates

If settings changed, regenerate the checked-in example with the focused test:

```sh
UPDATE_CONFIG_EXAMPLE=1 cargo test -p opscope-widgets --test check \
  generated_config_example_matches_widget_settings
```

`config.example.json` generation is Rust code in `widgets/tests/check.rs`.
There is no `tools/config-example.py`, and **Python 3 is not a build or test
dependency**; the old subprocess was removed because it could fail for a
reason the test was not meant to test.

Run the new binary's focused tests while iterating, then the complete gate:

```sh
cargo test -p opscope-widgets --bin example-widget
cargo test
cargo build --workspace --bins
```

`cargo test` compiles every parser on the current target, runs widget and core
unit tests, runs the source-contract checks below, and runs the npm packaging
tests. CI repeats it on Linux and macOS. `cargo build --workspace --bins`
separately proves all sixteen executable targets build as binaries.

### 8. Smoke-test the built widget

Commands in the repository must use the build-tree launcher; a fresh checkout
does not have `opscope` on `PATH`:

```sh
./target/debug/opscope example-widget
./target/debug/example-widget --help
./target/debug/example-widget --configure-help
```

Manually verify the real source and its honest empty/error states, narrow and
wide resize, a pane short enough to scroll, wheel versus selection behaviour,
every documented key, and the shared settings screen where present. Restart
the process after editing—the running binary still holds the old code.

### 9. Hand it off

Keep the pull request to this one logical widget, use a Conventional Commit
title, and include the focused, full-suite, and manual evidence. The release
and npm publication path is separate from PR merge; it is documented in
[Releasing](../docs/releasing.md).

### What catches a missed step, and what does not

| step | caught by |
|---|---|
| `[[bin]]` name and folder path | `every_widget_owns_its_complete_folder`, then the compiler |
| the four folder files and embedded guide | `every_widget_owns_its_complete_folder` |
| `README.md` row | `every_widget_has_a_readme_row` |
| `docs/README.md` row | `every_documented_widget_is_in_the_docs_index` |
| `config.example.json` | `generated_config_example_matches_widget_settings` |
| the launcher registry entry | `every_widget_is_on_the_launcher_menu` in `widgets/tests/check.rs`, and `every_binary_is_on_the_menu` in `widgets/src/launcher/main.rs` |
| npm packer `[[bin]]` count | `the packer takes every [[bin]]` in `npm/test.js` |
| alphabetical launcher order | `the_list_is_in_a_settled_order` in `widgets/src/launcher/main.rs` |
| first README preview | `a_sample_is_a_picture_of_the_widget` in `widgets/src/launcher/main.rs` |
| `help.txt` summary and paragraph | `every_widget_describes_itself` in `widgets/src/launcher/main.rs` |

`every_widget_is_on_the_launcher_menu` is the repository check: it fails
if a widget folder is missing from the launcher registry.
`every_binary_is_on_the_menu` reads `Cargo.toml` and asserts that every
non-launcher `[[bin]]` appears in `WIDGETS`. Add the registry entry and the
compiler then insists on `help.txt` and `README.md`, because `widget!`
`include_str!`s both. Skip it and `cargo test` fails with the binary name
that is built but missing from the menu.

`widget_names_in_the_launcher_sample_are_current` runs the other way: it
catches a name in the sample listing that is *no longer* a widget — the
thing a rename forgets.

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

Use those shared helpers rather than copying them into one widget;
`shared_helpers_are_not_redefined_by_widgets` enforces the helpers whose
duplication has already caused drift.

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

`config.example.json` is **generated** from every `settings.json`. The exact
fresh-checkout command and the reason it has no Python dependency are in
[step 7 above](#7-generate-configuration-and-run-the-gates).

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

### A list of strings is filled in one entry at a time

An array of strings is filled in one entry at a time: the box composes an
entry, `↵` adds it, `[d]` on a row removes it. `items: "string"` says so
outright. The screen can also infer that editor from a non-empty default of
strings, but still declare `items` — a default that later becomes `[]` would
otherwise leave the field as a JSON box, which is why the check requires the
declaration even when the shipped default is already a list of strings.

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
