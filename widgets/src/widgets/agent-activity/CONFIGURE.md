# Configure agent-activity

This widget reads local processes, agent child shells, and the shared
`tc::agent_work` session readers. It needs no credentials, hooks or multiplexer.
It never contacts an agent vendor API and never installs tools.

The owned settings section is `agent_activity`. Press comma in the widget
for `refresh` (seconds, default 2) and `finished_per_agent` (default 3; zero
shows no finished children). Saved settings apply immediately. `-n SECONDS`
overrides the refresh setting for that launch.

Claude uses `CLAUDE_CONFIG_DIR` or `~/.claude`; Codex uses `CODEX_HOME` or
`~/.codex`; OpenCode uses `$XDG_DATA_HOME/opencode/opencode.db` or
`~/.local/share/opencode/opencode.db`. Ask where an agent actually stores
its sessions before changing environment variables; do not invent sessions
or copy transcript contents into public issues. Process commands and local
paths can contain private information.

Safe inspection: run `ps -axww -o pid=,ppid=,comm=,args=` locally and compare
the named agent PID with the widget. Linux reads its own cwd via core's
procfs helpers; macOS uses a single `lsof` read. An unreadable cwd or
malformed store must show an error next to that agent, never an empty list
that claims no work. OpenCode V1 stores without persisted liveness show
the shared reader's limitation rather than inferred running children.

Use Enter to inspect the matched session ID and its guess warning. The
newest transcript for a cwd is a guess, especially when multiple agents
share that cwd. A live process does not prove an active model turn, and a
log state is not a heartbeat. Finished rows cover only the matched sessions
of currently discovered parents, not historical totals or exited parents.
