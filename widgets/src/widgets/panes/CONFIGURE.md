# Configure `panes`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The multiplexer `panes` runs in, found from the pane's environment (`REX_SESSION` and `REX_BLOCK`, `HERDR_ENV=1`, `LUVUS_ENV=1`). Rex is the backend read today. Under Rex: the running Rex server, read through the local `rex` command: `rex do` with a read-only script for the inventory, `rex events --all --json` for wake-ups, and `rex block focus`, `rex zoom` and `rex capture` for the three keys that act or read on demand.

## Settings owned here

The owned section is `panes`. A `rex_panes` section left from before the rename is read when there is no `panes` section, with its `server` taken as `rex_server`, and a notice asks for it to be renamed.

Declared fields: `backend`, `rex_server`, `refresh`, `agents`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `rex` exists and a server answers: `rex server status` (read-only). Do not run `rex server start` on the user's behalf; the widget deliberately never starts one.
2. Leave `backend` at `auto`: it follows the pane `panes` runs in. Set it only when the user wants one multiplexer read wherever `panes` starts.
3. Leave `rex_server` empty unless the user names another Rex server. A non-empty value is a host label from `rex hosts` or an address the user gives you; never guess one, and never write a hostname or address into a tracked file.
4. `refresh` is the maximum idle delay between full reads; events wake a read sooner. Keep the default unless the user has a concrete reason.
5. `agents` is a list of command names. Add a name only when the user runs that agent and it is landing in PROCESSES instead of AGENTS. A program that reports its own status with OSC 7501 is shown with that status wherever it lands.
6. Save through the settings screen and leave it — the widget reloads itself on the way out. Then verify that the pane lists the user's sessions, or names a specific reason it cannot (no server, a server that did not answer, a session that could not be read).

## Boundaries

The embedded script only reads: it calls `session.list`, `session.attach`, `session.view`, `session.list_blocks`, the `process`, `title` and `program_status` block methods, `server.status`, `server.version` and `client.list`, and a repository check fails the build if it calls anything else. The two keys that change anything, focus and zoom, change only which pane is focused or zoomed.

Agent state is never inferred. If the user asks why an agent shows `unknown`, the answer is that it has not reported a status; Claude Code's chat reports on its own, `claude agents` does not. Do not suggest editing titles or installing hooks to make a state appear.

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
