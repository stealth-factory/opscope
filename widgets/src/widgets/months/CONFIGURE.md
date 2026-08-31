# Configure `months`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

The machine clock and the IANA timezone database compiled into the binary. Nothing is fetched, nothing is stored, and no credential exists to leak: both settings change how the same computed dates are read, never which dates they are.

## Settings owned here

The owned section is `months`.

Declared fields: `week_start`, `timezone`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Ask which day the user wants weeks to start on. Sunday is the default; `monday` is the other answer, and the settings screen offers both rather than asking for one to be typed. Do not infer it from locale data on the machine — it is a reading preference, not a property of the host.
2. Leave `timezone` empty unless the user wants the grid reckoned somewhere other than this machine. Only then set it, and only to a name they gave: read `/etc/localtime` or `$TZ` if it helps you *ask a better question*, never to decide the answer, and never from an IP address.
3. Read the resolved path shown by the settings screen and the current values before proposing changes.
4. Change only this widget's declared section. A zone name is not a secret, but the same rule holds as everywhere: keep anything private out of chat, logs, shell history, source files, and screenshots.
5. Save through the settings screen and leave it — the widget restarts itself on the way out, so no manual restart is needed. Then check the pane: the second line names the zone it reckoned in and the day the weeks start on, so a wrong answer is visible immediately.

## Verifying it took

The grid says what it did. After a change, confirm on screen that the reckoning line names the zone you meant and the week start you meant, and that today's square is the date the user would call today where they are. A `timezone` the database does not know is not silently accepted: the pane says the name was not found and that it fell back to this machine's zone.

## Boundaries

Do not fabricate a timezone name, a locale, or a user's whereabouts. Do not invent hostnames, account names, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
