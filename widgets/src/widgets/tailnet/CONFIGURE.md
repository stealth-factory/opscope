# Configure `tailnet`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

`tailscale status --json` and local peer traffic counters.

## Settings owned here

The owned section is `tailnet`.

Declared fields: `history`, `refresh`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm the `tailscale` command exists and is signed in. Do not copy node addresses or tailnet names into repository files.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Only history length and refresh cadence are configurable; ask before increasing polling frequency.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen, restart the widget when it says restart is required, and verify that the real source answers or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
