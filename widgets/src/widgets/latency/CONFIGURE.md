# Configure `latency`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

ICMP replies produced by the installed `ping` command. The widget detects the
installed command's dialect at runtime: iputils supplies `no answer yet`,
while BSD ping supplies native timeouts or exposes loss through sequence gaps
and bounded total silence. The same configuration works on Linux and macOS.

## Settings owned here

The owned section is `latency`.

Declared fields: `hosts`, `interval`, `seconds_per_column`, `window`, `spike_factor`, `aggregate`, `strip_suffixes`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `ping` exists, then ask which public or user-owned hostnames should be measured. Never invent an internal hostname.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask for targets, sampling cadence, graph window, aggregation, and any suffixes the user explicitly wants hidden.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
