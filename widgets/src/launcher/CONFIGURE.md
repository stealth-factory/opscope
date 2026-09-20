# Configure the `opscope` launcher

This is plain documentation for a person or AI assistant. It is not an
executable skill and not permission to change files.

## Settings owned here

None. The launcher declares no settings and shows no settings action.

It used to own the one shared key, `terminal.mouse`, which decided whether
widgets asked the terminal for mouse reports. Reporting is unconditional
now: the key was on by default and only ever turned off by mistake, so it
was a way to break a pane rather than a way to make anything easier.

## Safe process

1. There is nothing here to inspect or configure.
2. Each widget owns its own settings, its own `,` screen and its own
   `CONFIGURE.md`; configure a widget there, not here.
3. While a widget is running, a drag belongs to the host rather than to the
   terminal's own selection. The way to copy a value out of a widget is
   that widget's own copy key - eight of them carry one.

## Boundaries

Do not fabricate hostnames, account names, team names, repositories, prices,
ports, credentials, or successful readings. If a value cannot be established
from the local environment or the user's explicit instruction, ask.
