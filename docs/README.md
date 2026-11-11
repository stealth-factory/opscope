# Documentation

[← opscope](../README.md)

Each widget owns one folder containing its code, README, help, and AI
configuration guide, plus a settings declaration when it is configurable.

## Launcher

[`opscope`](../widgets/src/launcher/README.md) is the front door: every
widget, what it does, and its static preview. It is packaging and
navigation, not itself a widget.

## Widgets

| | |
|---|---|
| [`latency`](../widgets/src/widgets/latency/README.md) | Continuous latency to a list of hosts, with the statistics that actually explain a bad connection. |
| [`vercel-deployments`](../widgets/src/widgets/vercel-deployments/README.md) | Vercel deployments — how they are going over time, not just what shipped last. |
| [`tailnet`](../widgets/src/widgets/tailnet/README.md) | Tailscale peers, and — the part plain `tailscale status` buries — *how* you are reaching each one. |
| [`github`](../widgets/src/widgets/github/README.md) | Pull requests across every org you work in — not what shipped, but whether work is actually moving. |
| [`github-actions`](../widgets/src/widgets/github-actions/README.md) | GitHub Actions across those same accounts — what is running, what is failing, and how long it sat in the queue. |
| [`github-prs`](../widgets/src/widgets/github-prs/README.md) | The pull requests you have to follow up on, and a dashboard for whichever one you open. |
| [`google-search-console`](../widgets/src/widgets/google-search-console/README.md) | Which pages and queries moved in Google Search, what people search to arrive, and which pages that earn the traffic are broken in the index. |
| [`linear`](../widgets/src/widgets/linear/README.md) | Linear across every team at once — what is outstanding, which cycles are running, and whether issues are being closed faster than they arrive. |
| [`google-analytics`](../widgets/src/widgets/google-analytics/README.md) | Google Analytics 4: is traffic normal right now against a baseline this widget records itself, and did key events hold up against the same weekday. |
| [`agent-activity`](../widgets/src/widgets/agent-activity/README.md) | Local coding-agent processes and their child work, with source details and bounded latest finished items. |
| [`agent-usage`](../widgets/src/widgets/agent-usage/README.md) | How much the coding agents on this machine have actually been used — one tab per agent, from each agent's own local state, plus a live quota reading for the four that publish one and a subscription for the five that do. |
| [`ports`](../widgets/src/widgets/ports/README.md) | What is listening on this machine, what started it, and who can reach it. |
| [`mux-activity`](../widgets/src/widgets/mux-activity/README.md) | Everything running in the multiplexer it runs in — [Rex](https://www.superlogical.com/rex/docs/), [Herdr](https://herdr.dev) or [Luvus](https://luvus.dev) — agents by who needs a human, blocked first, and one key to get to any pane. Under Luvus, also the tasks and path leases agents coordinate with. |
| [`netwatch`](../widgets/src/widgets/netwatch/README.md) | Which processes are using the network, how much they have used, and how fast they are going right now. |
| [`disk`](../widgets/src/widgets/disk/README.md) | Which filesystems can still take a write, which disks are doing I/O, and, on Linux, whether I/O is stalling. |
| [`procs`](../widgets/src/widgets/procs/README.md) | What this machine is running: per-core CPU, load against the core count, memory and swap, ten minutes of load, then every process with its CPU over a named window, its memory and its state, and one process opened at a time. |
| [`link`](../widgets/src/widgets/link/README.md) | How good the connection is between this machine and whoever is connected to it — measured, not probed. |
| [`google-trends`](../widgets/src/widgets/google-trends/README.md) | What a region is searching for right now — Google's own Trending Now feed, shown as Google states it, with what changed since the last look. |
| [`clocks`](../widgets/src/widgets/clocks/README.md) | This server's clock, the clocks counting down, a pomodoro, everyone else's clock, the current weather for one configured place, and a condition glyph on each world-clock city that has coordinates. |
| [`matrix`](../widgets/src/widgets/matrix/README.md) | Digital rain. It computes nothing and reports no data on purpose. |
| [`months`](../widgets/src/widgets/months/README.md) | A month grid you can page through, with today in context — every square computed from the machine clock and the timezone database rather than fetched from anywhere. |

## About the repository

| | |
|---|---|
| [Configuring them](../README.md#configuration) | Both ways in: the settings screen every widget opens with `,`, and editing `config.json` yourself against the generated example. |
| [Making a widget](../wiki/making-a-widget.md) | One folder owns a widget's code, help, preview, settings, and AI configuration guide — and what else it takes to add one. In the [wiki](../wiki/README.md). |
| [Design](design.md) | The rules every widget holds to, and what each one cost to learn. |
| [Internals](internals.md) | `opscope-core`, the chart helpers, and what `cargo test` checks that a compiler cannot. |
| [Port decisions](port-decisions.md) | What the Rust port changed from the Python, and why. |
| [Port discovery](ports.md) | How `ports` acquires listeners and process details on Linux and macOS, including the traffic boundary. |
| [Link acquisition](link.md) | How `link` maps Linux `ss` and macOS `nettop` onto one session view, including the delivery-rate and loss boundaries. |
| [Google setup](google-setup.md) | The one Cloud project, three APIs and service-account key the Google widgets sign in with, and the least-privilege access each property needs. |
| [Building a widget on Google APIs](google-widgets.md) | For widget developers: core's `google` helper, the scopes, the sign-in settings to declare, the shared wording, and testing without key material. |
| [Building herdr panels](building-herdr-panels.md) | Driving these from Herdr: resize semantics, focus, and the layout mistakes worth skipping. |
| [Releasing](releasing.md) | How a version is decided, what merging the release PR sets off, and what to do when it goes wrong. |
| [As a Luvus module](../luvus/README.md) | Every widget as a pane inside a Luvus session: what the module declares, where its binaries come from, and why the manifest is generated. |
