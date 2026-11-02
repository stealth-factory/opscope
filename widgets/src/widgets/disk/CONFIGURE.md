# Configure `disk`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux reads `/proc/self/mountinfo` and then `statvfs` on each mount that is
shown. macOS reads `getmntinfo` with `MNT_NOWAIT`, which returns the same
figures in one call. A mount that does not answer is named on its row. The
widget does not invent a zero for a figure it does not have.

Device rows on Linux come from `/proc/diskstats` each poll and from
`/sys/block` at most once a minute. Sectors in `diskstats` are 512 bytes.
macOS device rows come from `iostat` (`iostat -d -K -w`), which is
recommended rather than required: without it the filesystem table still
runs and the disk section says the tool is missing. `/proc/pressure/io` is
Linux-only. An absent file is named; it is not drawn as zero.

Samples this widget measured itself are kept in `usage.json` under the
shared state directory (`disk/usage.json`). A series is the mount, the
filesystem type and `f_blocks`. A resize or a reformat starts a new series.
A gap longer than the window on screen restarts the projection for that
window; the file still keeps the older points for a longer window. `w`
cycles 7, 14 and 30 days for the session. `window_days` is which of those
the pane starts on.

## Settings owned here

The owned section is `disk`.

Declared fields: `refresh`, `hide_mounts`, `hide_devices`, `warn_percent`, `alert_percent`, `window_days`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Read the resolved path shown by the settings screen and the current values before proposing changes.
2. Ask which mount points to hide, how often to sample, and where the bar should start to heat. Paths are local data; do not copy them into tracked examples.
3. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
4. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the figures agree with the kernel, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, mount paths from someone else's machine, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
