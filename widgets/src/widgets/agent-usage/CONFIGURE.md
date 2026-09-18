# Configure `agent-usage`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Local state left by installed coding agents, plus only the optional remote quota calls described in the README.

## Settings owned here

The owned section is `agent_usage`. A legacy `usage` section is used only when `agent_usage` is absent.

Declared fields: `auto_detect_agent`, `agents`, `exclude_agents`, `refresh`, `rates`, `plan_cost`, `antigravity_remote`, `antigravity_start`, `grok_ping`, `grok_ping_minutes`, `claude_config_dirs`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

`rates` is the rate card the widget ships, not a JSON field. `↵` on it opens one screen listing models — the ones already set, or the whole card when none are; `tab` switches, typing searches all of them. `↵` on a model opens its priced kinds with the published price as each default, and `esc` returns to the card with the search intact. Opening a model writes nothing; an entry appears when a number is set, and `[d]efault` removes it whole. **Set only the kinds that are actually wrong.** Config wins per kind, so an untouched kind keeps tracking the shipped card and still receives the vendor's next correction; writing all five pins that model to today's prices for good. Never copy the published numbers in to "make them explicit" — that is the stale-price failure, arranged by hand.

## Safe configuration process

1. Inspect which supported agents are installed or have local state. Do not read or print their credential contents.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask before fixing the agent list, naming extra Claude config dirs, setting personal plan costs/rates, turning `grok_ping` off, or changing whether another CLI may be started. Extra Claude directories are a list on `claude_config_dirs`; unset or empty keeps the single `~/.claude` tab. An entry is either a path or `{"path": "…", "label": "…"}`, where the label is what the tab and the `[+]` group read; absent, empty or whitespace falls back to the directory's own name. With one directory the label is unused and both read `CLAUDE`. **The settings screen cannot write a label** — it has no picker for an array of objects, so it lists every entry, adds and removes plain paths, and leaves labelled entries alone. Setting a label is an edit to `config.json`, so propose that edit explicitly rather than routing it through the screen.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
