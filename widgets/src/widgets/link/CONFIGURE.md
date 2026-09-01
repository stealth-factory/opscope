# Configure `link`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Kernel TCP accounting exposed by `ss` on Linux and `nettop` on macOS; the
widget sends no probes. macOS does not expose Linux's achieved delivery-rate
estimate, and `nettop`'s `re-tx` is a segment count rather than retransmitted
bytes, so those two fields say so instead of substituting a different
measurement.

## Settings owned here

The owned section is `link`.

Declared fields: `ports`, `refresh`, `history`, `windows`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Confirm `ss` exists on Linux or `/usr/bin/nettop` exists on macOS, and inspect listening ports only if the user authorises it. Do not persist LAN addresses.
2. Read the resolved path shown by the settings screen and the current values before proposing changes.
3. Ask whether all inbound listening ports or an explicit port set should count, and which chart windows are useful.
4. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the real source answers, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices, ports, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
