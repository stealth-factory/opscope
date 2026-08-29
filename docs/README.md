# Documentation

[← opscope](../README.md)

Each widget owns one folder containing its code, README, help, settings
declaration, and AI configuration guide.

## Launcher

[`opscope`](../widgets/src/launcher/README.md) is the front door: every
widget, what it does, and its static preview. It is packaging and
navigation, not itself a widget.

## Widgets

| | |
|---|---|
| [`latency`](../widgets/src/widgets/latency/README.md) | Continuous latency to a list of hosts, with the statistics that actually explain a bad connection. |
| [`deployments`](../widgets/src/widgets/deployments/README.md) | Vercel deployments — how they are going over time, not just what shipped last. |
| [`tailnet`](../widgets/src/widgets/tailnet/README.md) | Tailscale peers, and — the part plain `tailscale status` buries — *how* you are reaching each one. |
| [`herdr-panes`](../widgets/src/widgets/herdr-panes/README.md) | Everything running under [Herdr](https://herdr.dev), across every workspace — and one keypress to get to any of it. |
| [`github`](../widgets/src/widgets/github/README.md) | Pull requests across every org you work in — not what shipped, but whether work is actually moving. |
| [`github-actions`](../widgets/src/widgets/github-actions/README.md) | GitHub Actions across those same accounts — what is running, what is failing, and how long it sat in the queue. |
| [`github-prs`](../widgets/src/widgets/github-prs/README.md) | The pull requests you have to follow up on, and a dashboard for whichever one you open. |
| [`linear`](../widgets/src/widgets/linear/README.md) | Linear across every team at once — what is outstanding, which cycles are running, and whether issues are being closed faster than they arrive. |
| [`agent-usage`](../widgets/src/widgets/agent-usage/README.md) | How much the coding agents on this machine have actually been used — one tab per agent, from each agent's own local state, plus a live quota reading for the four that publish one and a subscription for the five that do. |
| [`ports`](../widgets/src/widgets/ports/README.md) | What is listening on this machine, what started it, and who can reach it. |
| [`netwatch`](../widgets/src/widgets/netwatch/README.md) | Which processes are using the network, how much they have used, and how fast they are going right now. |
| [`link`](../widgets/src/widgets/link/README.md) | How good the connection is between this machine and whoever is connected to it — measured, not probed. |
| [`clocks`](../widgets/src/widgets/clocks/README.md) | This server's clock, the clocks counting down, a pomodoro, and everyone else's clock — the four things you need to know about time while working across timezones. |
| [`matrix`](../widgets/src/widgets/matrix/README.md) | Digital rain. It computes nothing and reports no data on purpose. |

## About the repository

| | |
|---|---|
| [Making a widget](../wiki/making-a-widget.md) | One folder owns a widget's code, help, preview, settings, and AI configuration guide — and what else it takes to add one. In the [wiki](../wiki/README.md). |
| [Design](design.md) | The rules every widget holds to, and what each one cost to learn. |
| [Internals](internals.md) | `opscope-core`, the chart helpers, and what `cargo test` checks that a compiler cannot. |
| [Port decisions](port-decisions.md) | What the Rust port changed from the Python, and why. |
| [Building herdr panels](building-herdr-panels.md) | Driving these from Herdr: resize semantics, focus, and the layout mistakes worth skipping. |
| [Releasing](releasing.md) | How a version is decided, what merging the release PR sets off, and what to do when it goes wrong. |
