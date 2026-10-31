# Configure the `opscope` launcher

This is plain documentation for a person or AI assistant. It is not an
executable skill and not permission to change files.

## Settings owned here

None. The launcher declares no settings and shows no settings action.

Starts of this menu and of each widget are counted by one shared library.
The first launch asks. Accepting or declining writes `analytics.enabled`
in config.json, and that choice can be edited later. The time of the choice
is stored in the state directory, not in config.json. Collect URLs and ids
for Vercel Analytics and Google Analytics live in the same `analytics`
section; a destination left empty is skipped. An event carries the widget
or launcher name and that a start happened, and the first choice also says
opted in or opted out. Nothing in an event is a hostname, an account, the
contents of config, or an identity.

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
