---
name: measure-data-usage
description: "Measure how many bytes per second an opscope widget sends in text mode, Kitty graphics mode, and under a relay like Herdr, and check it against the repo's pass/fail data budget. Use when building or reviewing a widget or core rendering change, running the data-usage smoke test from wiki/making-a-widget.md, checking whether a chart or bar re-uploads Kitty images unnecessarily, investigating terminal or SSH bandwidth floods, or measuring live output under Herdr."
metadata:
  author: stealth-factory
  co-author: wiiiimm
  version: "1.0.0"
---

# Measure Widget Data Usage

Measure a widget's terminal output rate (`total KB/s`, `text KB/s`, `image KB/s`, `uploads/s`, and decoded `raw-RGBA KB/s` where `1 KB = 1024 B`) across `text`, `kitty`, and `herdr` modes, and report pass/fail against the opscope data budget.

## Why both wire bytes and raw-RGBA bytes matter

Kitty image uploads (`a=T`, `a=f`) are zlib-compressed on the local PTY (`o=z`), so a full chart can look small on the wire (~2–4 KB) while decoding to `width * height * 4` bytes (~0.6–1.5 MB of raw RGBA). A host that relays decoded images to a remote client (such as Herdr over SSH before `#469`, or with `OPSCOPE_GRAPHICS=kitty`) pays the **raw RGBA size** on every change. Typical usage is 2–6 widgets on one tab and ~12 across tabs, so the budget must hold for a dozen concurrent panes.

## Default workflow (local PTY — nothing over SSH)

Always measure locally first using `scripts/measure-output.py` under isolated Python (`python3 -I`, stdlib only; Python 3 is a tool for this skill only, never a Cargo build or test dependency):

```sh
# 0. Check the harness itself still parses Kitty output and applies the budget.
#    Python is not a test dependency, so nothing else runs this.
python3 -I scripts/measure-output.py --self-test

# 1. Build the widget binary you are testing (for example, latency or clocks)
cargo build --bin latency

# 2. Run 60 s per mode (text, kitty, herdr) after the default 6 s warm-up
python3 -I scripts/measure-output.py ./target/debug/latency --seconds 60

# For meter/table/static-chart widgets that must upload zero images at rest:
cargo build --bin clocks
python3 -I scripts/measure-output.py ./target/debug/clocks --seconds 60 --steady
```

### What `scripts/measure-output.py` does

- Spawns the widget on a local `57×40` PTY (`10×20` px cells by default — a standard multi-pane slot within `opscope-core`'s valid raster range; override with `--cols`, `--rows`, `--cell-width`, `--cell-height`).
- Scrubs ambient `TMUX`, `TMUX_PANE`, `NO_COLOR`, `OPSCOPE_GRAPHICS`, and `HERDR_ENV` from the child environment, so your current multiplexer never contaminates the test, then runs three modes by default (`--mode text|kitty|herdr|kitty-herdr`):
  - **text** (`--mode text`): `OPSCOPE_GRAPHICS=text` (`TERM=xterm-256color`).
  - **kitty** (`--mode kitty`): `OPSCOPE_GRAPHICS` unset, `HERDR_ENV` unset (`TERM=xterm-kitty`, answering Kitty graphics queries).
  - **herdr** (`--mode herdr`): `HERDR_ENV=1`, `OPSCOPE_GRAPHICS` unset (`TERM=xterm-kitty`, still answering Kitty queries as Herdr does — verifying `auto` selects text under `HERDR_ENV=1`).
  - **kitty-herdr** (opt-in via `--mode kitty-herdr`): `HERDR_ENV=1` + `OPSCOPE_GRAPHICS=kitty`, verifying opt-in pixels under Herdr never emit `a=p` 5 s heal re-placements.
- Answers every startup and runtime query `opscope-core` sends: Kitty graphics query (`i=2147483646` → `OK`), animation/frame-edit probe (`q=0` → `OK`, `q=1` silent just like real Kitty), POSIX shared-memory probe (`i=2147483645` → `EBADF` so images stay inline `t=d` on the PTY and no `/dev/shm` segments leak), `CSI 16 t` cell size, `CSI ? 2026 $ p` sync updates, `CSI ? u` keyboard protocol, OSC 10/11 colours, OSC 66 text-sizing probe, XTGETTCAP underlines, and Primary DA (`CSI c`).
- Splits output arriving after `--warmup` into `text KB/s` and `image KB/s` (`ESC _ G … ESC \`), counts `a=T`/`a=f`/`a=p`/`a=d` commands (`a=T/f/p/d`, excluding startup capability probes), and computes `raw-RGBA KB/s` (`s * v * 4` behind each `a=T`/`a=t`/`a=f` pixel upload).
- Exits `0` when every mode passes (or `EXEMPT`), `1` when any mode violates the budget or exits early.

> **Check that the widget is actually polling real data.** A configurable widget with missing credentials or empty targets sits on a static `missing_config()` / `cannot_start()` screen and writes ~0 KB/s without exercising its charts or pollers. Verify the widget has working inputs (or pass `-- <args>`) before trusting a near-zero reading.
>
> **It runs with your real config and credentials.** The child keeps your `HOME`, so a network widget reads your `config.json`, calls its real APIs once per mode, and reads and writes the real shared readings store in `~/.local/state/opscope`. That is what makes the reading real; know it before running an API widget repeatedly.
>
> **`mux-activity` cannot be measured by this harness, and its budget stays unverified.** The harness scrubs `TMUX` and `HERDR_ENV`, and the PTY is not a Rex, Herdr or Luvus pane, so `mux-activity` finds no multiplexer and exits at once (`exited early … (status 2)`). `--proc-io` can show a live process's total `write()` rate, including state-file writes. It does not report text, image, upload, or raw-RGBA rates, so it cannot pass or fail a mode in the table below.

## Pass/fail budget (per widget after warm-up; `1 KB = 1024 B`)

Every limit is set for **a dozen panes at once over SSH on mobile data**: 2–6 widgets on a tab and about 12 across tabs. A per-widget figure that looks harmless multiplies: 15 KB/s a pane is ~650 MB an hour. The table's `x12 MB/h` column does that multiplication for you; quote it in the report.

The limits were calibrated on 2026-11-18 against every local widget on `main` (20 s per mode). Text output ran 1.0–6.9 KB/s (`latency` the highest). In `kitty`, `latency` made ~5.6 `a=f` edits/s (40–66 KB/s raw) and `ports` ~0.4 whole `a=T`/s. `tailnet` made **4 whole `a=T`/s** and fails, correctly: an image re-uploaded on almost every frame is the defect this skill exists to catch.

| Mode / case | Wire `total KB/s` | `uploads/s` | `raw-RGBA KB/s` | Notes |
| --- | --- | --- | --- | --- |
| `text` | `<= 8 KB/s` (`<= 5` with `--steady`) | `0` | `0` | Any Kitty graphics byte fails |
| `herdr` (`HERDR_ENV=1`) | `<= 8 KB/s` (`<= 5` with `--steady`) | `0` | `0` | Must draw text (`0` `a=T`, `a=f`, and `a=p`) |
| `kitty` — steady screen / meter-only (`--steady`) | `<= 5 KB/s` | `0` | `0` | Meters, stacked bars, and loading placeholders are eighth-cell text. Compare `clocks` in `kitty` with its `text` rate by hand in the report; the script judges each mode on its own and does not compare them. Periodic `a=p` every 5 s (`WHOLE_EVERY` heal) places cached images without uploading pixels (`uploads = 0`) |
| `kitty` — live-updating pixel chart (`latency`, `link`, `ports`, `disk`, `procs`) | `<= 8 KB/s` | `<= 8.0/s` total (`a=f` bands), `<= 0.5/s` whole `a=T` | `<= 100 KB/s` | Small changes emit narrow `a=f` changed-band rectangles; a whole `a=T` more often than once per 2 s means an image is being rebuilt instead of edited. Never upload an image on every frame |
| `kitty-herdr` (opt-in: `HERDR_ENV=1` + `OPSCOPE_GRAPHICS=kitty`) | `<= 8 KB/s` | as `kitty` | **`<= 16 KB/s`** | Herdr relays Kitty images **decoded**, so raw RGBA is what crosses SSH. OPS-199 was this case at up to 4.9 MB/s. `latency` (~32–57 KB/s raw) fails here: opting a chart widget into pixels under Herdr costs about 1.4 GB/h across 12 panes. Also fails any `a=p` re-placement |
| `matrix` (documented exception) | `<= 500 KB/s` (`EXEMPT`) | `0` | `0` | ~240 KB/s text at 30 fps by design; still `0` image bytes in every mode. Judged by the binary's file name — pass `--name matrix` for a renamed copy |

## Warm-up times per widget (`--warmup SECONDS`)

Default `--warmup 6` settles startup negotiation, initial paint, initial chart/icon uploads, and fast local pollers (`clocks`, `latency`, `link`, `ports`, `disk`, `procs`, `months`, `mux-activity`). Increase `--warmup` when a network poller takes longer to reach steady state:

| Widget(s) | Recommended `--warmup` | Why |
| --- | --- | --- |
| Local / fast widgets (`clocks`, `latency`, `link`, `ports`, `disk`, `procs`, `months`, `mux-activity`) | `6` (default) | First local samples and initial Kitty upload finish within a few seconds |
| `github` | `30` | Animates text placeholder bars while its PR-flow day-chunks load on a cold cache; measure after the chart settles |
| `agent-usage` | `120` | Needs ~2 minutes for its first full vendor usage read before reaching its steady screen |
| Other API widgets (`linear`, `github-actions`, `github-prs`, `vercel-deployments`, `google-*`) | `15` | Lets the initial API round-trip complete when the `tc::readings` store is cold |
| Cold-start inspection | `0` | Includes capability queries and the initial full-screen paint/upload |

## Sampling `/proc/PID/io` `wchar` (and the launcher trap)

On Linux, you can cross-check a live process's `write()` rate without wrapping it in a PTY. Identify the live widget PID first (`pgrep -x latency` or `ps -eo pid,comm,args` — never `pgrep -f`, which matches your own shell), set `WIDGET_PID`, and run:

```sh
WIDGET_PID="$(pgrep -n -x latency)"
python3 -I scripts/measure-output.py --proc-io "$WIDGET_PID" --seconds 15
```

Two caveats apply whenever you read `/proc/<PID>/io`:

1. **Never measure the `opscope` launcher PID expecting live widget traffic.** The launcher spawns widgets as **child processes sharing its tty**, so `/proc/<launcher_pid>/io` stays around ~0–1 KB/s while a live child widget floods megabytes per second (Linux only folds a child's `wchar` into its parent after the child exits and is reaped). `--proc-io` warns and lists live child PIDs if pointed at a parent.
2. **`wchar` counts every `write()`/`pwrite()` syscall**, including SQLite `tc::readings` and statefile writes — not only terminal stdout. Use the local PTY harness for exact terminal text vs image breakdown, and `--proc-io` for watching a live multiplexer client bridge.

## Live Herdr measurement (explicit opt-in only)

**Do not run live Herdr measurements unless the user explicitly asks.** Over a remote SSH session, an unguarded image flood can saturate the link and lock the user out of terminal control.

When asked to measure live inside Herdr:

1. **Check your detach escape before launching anything.** Confirm how to detach the Herdr client (`herdr --help` or your configured Herdr detach keybinding; default tmux-style prefix detach if configured) so you can drop the client stream immediately if a backlog forms.
2. **Test one widget at a time, starting it paused so monitoring is armed before its first frame:**
   - In the visible Herdr pane, launch the widget suspended after printing its PID:
     ```sh
     sh -c 'printf "WIDGET_PID=%s\n" "$$"; kill -STOP "$$"; exec ./target/debug/latency'
     ```
   - In your monitoring shell, identify `HERDR_CLIENT_PID` (`ps -eo pid,comm,args`), start the watchdog, and then resume the widget. The watchdog opens a pidfd for each `--kill-pid` when it arms and signals only that fd if bridge output stays `>= 300 KB/s` for `2 s` sustained. If pidfd signalling is unavailable it exits without signalling, rather than calling `kill` on a PID that may have been reused:
     ```sh
     HERDR_CLIENT_PID="..."   # set from ps -eo pid,comm,args
     WIDGET_PID="..."         # printed in the Herdr pane above
     python3 -I scripts/measure-output.py \
       --watchdog-pid "$HERDR_CLIENT_PID" \
       --kill-pid "$WIDGET_PID" \
       --threshold-kb 300 \
       --sustained 2 \
       --seconds 60 &
     WATCHDOG_JOB=$!
     sleep 0.2
     kill -CONT "$WIDGET_PID"
     wait "$WATCHDOG_JOB"
     ```
3. **Always stop the test widget as soon as the watchdog window finishes** (when the window ends cleanly with `OK`, the watchdog exits and is no longer guarding the link):
   ```sh
   if [ "$(cat "/proc/$WIDGET_PID/comm" 2>/dev/null)" = "latency" ]; then
     kill "$WIDGET_PID"
     while kill -0 "$WIDGET_PID" 2>/dev/null; do sleep 0.1; done
   fi
   ```
4. **When the watchdog is not enough — stop the flood by detaching the client:**
   Killing the widget stops *new* frames, **not** frames Herdr has already queued. Once multi-megabyte decoded images sit in Herdr's outbound buffer, the client bridge keeps draining that backlog at full SSH capacity (~7–8 MB/s) for 30+ seconds after the widget is dead.
   - **First response:** immediately **detach the Herdr client** using the detach binding/command verified in step 1 so Herdr drops the client stream instead of draining the queued backlog over SSH.
   - **If SSH itself is wedged and will not accept the detach key:** press `<Enter>~.` (OpenSSH's client escape sequence at the start of a line) to sever the SSH connection locally, reconnect in a fresh shell, verify `/proc/$WIDGET_PID/comm` still names the test widget before sending `kill "$WIDGET_PID"`, and confirm `/proc/$WIDGET_PID` is gone before continuing.

## Reporting format

Always include the command run, pane geometry, warm-up and window durations, one row per mode actually measured (`text`, `kitty`, and `herdr` on a standard run, or the specific `--mode` requested) including its `x12 MB/h` figure, any standalone `--proc-io` or `--watchdog-pid` observations, and a plain pass/fail summary naming any anomalies (for example whether `clocks` in `kitty` matched `text`, or why a widget needed a longer `--warmup`).
