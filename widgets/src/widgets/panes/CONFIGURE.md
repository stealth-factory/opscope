# Configure `panes`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The multiplexer `panes` runs in, found from the pane's environment (`REX_SESSION` and `REX_BLOCK`, `HERDR_ENV=1`, `LUVUS_ENV=1`); when several are set, the nearest multiplexer above it in the process tree. Only that multiplexer's CLI runs.

- **Herdr:** the local `herdr` command's pane and process state.
- **Luvus:** a running [Luvus](https://luvus.dev) session, read over Universal Harness Protocol 1.0 through the local `luvus` command. Eight read-only calls: `uhp snapshot`, `agent list`, `task list`, `lease list`, `task next`, `agent sessions`, `git status`, and `worktree list`. A bounded local `ps` scan also joins process names and full commands to the snapshot's root PIDs for the Opscope filter. Argument text is not displayed or saved. Run `panes` on the same machine as the session; if `ps` fails, the pane warns and filters only reported command labels.
- **Rex:** the running Rex server, read through the local `rex` command: `rex do` with a read-only script for the inventory, `rex events --all --json` for wake-ups, and `rex block focus`, `rex zoom` and `rex capture` for the three keys that act or read on demand.

## Settings owned here

The owned section is `panes`. A `rex_panes`, `herdr_panes` or `luvus_panes` section left from before is read when there is no `panes` section — `server` taken as `rex_server`, `session` as `luvus_session` — and a notice asks for it to be renamed.

Declared fields: `backend`, `rex_server`, `luvus_session`, `refresh`, `agents`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm the multiplexer the user runs `panes` in answers, read-only: `rex server status` under Rex, `herdr status` under Herdr, `luvus ping` and `luvus session list` under Luvus. Do not start a server on the user's behalf; `panes` deliberately never starts one. No hostname, socket path, token, or workspace name belongs in a tracked file.
2. Leave `backend` at `auto`: it follows the pane `panes` runs in. Set it only when the user wants one multiplexer read wherever `panes` starts.
3. Leave `rex_server` empty unless the user names another Rex server. A non-empty value is a host label from `rex hosts` or an address the user gives you; never guess one, and never write a hostname or address into a tracked file.
4. `luvus_session` matters only under Luvus, and only on a machine running more than one Luvus server. The name must be one `luvus session list` reports; a name with no server behind it draws the "no luvus server" screen rather than an empty board.
5. `refresh` is the maximum idle delay between full reads; events wake a read sooner. Keep the default unless the user has a concrete reason.
6. `agents`, under Rex, is a list of command names. Add a name only when the user runs that agent and it is landing in PROCESSES instead of AGENTS. A program that reports its own status with OSC 7501 is shown with that status wherever it lands.
7. Save through the settings screen and leave it — the widget reloads itself on the way out. Then verify that the pane lists the user's sessions, or names a specific reason it cannot (no server, a server that did not answer, a session that could not be read).

## Boundaries

Under Rex, the embedded script only reads: it calls `session.list`, `session.attach`, `session.view`, `session.list_blocks`, the `process`, `title` and `program_status` block methods, `server.status`, `server.version` and `client.list`, and a repository check fails the build if it calls anything else. The two keys that change anything, focus and zoom, change only which pane is focused or zoomed.

Agent state is never inferred. If the user asks why an agent shows `unknown`, the answer is that it has not reported a status; Claude Code's chat reports on its own, `claude agents` does not. Do not suggest editing titles or installing hooks to make a state appear.

Under Herdr, the board is read from full CLI snapshots, while a bounded Unix-domain socket subscription wakes refreshes for workspace-label changes, pane lifecycle changes, and status changes on every known pane. Events never patch the display. CPU and memory sampling remains periodic, and `r` wakes the CLI poller even while the event socket is waiting. Socket errors are shown separately from CLI errors.

Under Luvus, only one bounded event wait is used, for agent status changes, with a one-second minimum and a thirty-second maximum; the poller keeps the configured ceiling independently. Event payloads never patch the display; the next full read remains authoritative. Wait failures are shown separately from source-read failures. Nothing is written except the pane focus `↵` performs.

Do not fabricate hostnames, account names, session names, workspace names, agent names, branches, task ids, reserved paths, pane ids, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
