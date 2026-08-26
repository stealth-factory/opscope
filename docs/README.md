# Documentation

[← terminal-toys](../README.md)

One page per widget: what it shows, where every number on it comes from,
each key it answers to, and the settings it reads.

## Widgets

| | |
|---|---|
| [`start`](start.md) | The front door: every widget, what it does, and whether it will work on this machine. |
| [`latency`](latency.md) | Continuous latency to a list of hosts, with the statistics that actually explain a bad connection. |
| [`deployments`](deployments.md) | Vercel deployments — how they are going over time, not just what shipped last. |
| [`tailnet`](tailnet.md) | Tailscale peers, and — the part plain `tailscale status` buries — *how* you are reaching each one. |
| [`herdr-panes`](herdr-panes.md) | Everything running under [Herdr](https://herdr.dev), across every workspace — and one keypress to get to any of it. |
| [`github`](github.md) | Pull requests across every org you work in — not what shipped, but whether work is actually moving. |
| [`pr`](pr.md) | The pull requests you have to follow up on, and a dashboard for whichever one you open. |
| [`linear`](linear.md) | Linear across every team at once — what is outstanding, which cycles are running, and whether issues are being closed faster than they arrive. |
| [`usage`](usage.md) | How much the coding agents on this machine have actually been used — one tab per agent, from each agent's own local state, plus a live quota reading for the four that publish one and a subscription for the five that do. |
| [`ports`](ports.md) | What is listening on this machine, what started it, and who can reach it. |
| [`netwatch`](netwatch.md) | Which processes are using the network, how much they have used, and how fast they are going right now. |
| [`link`](link.md) | How good the connection is between this machine and whoever is connected to it — measured, not probed. |
| [`clocks`](clocks.md) | This server's clock, the clocks counting down, a pomodoro, and everyone else's clock — the four things you need to know about time while working across timezones. |

`matrix` has no page. It computes nothing on purpose, and a document
saying so at length would be the joke explained.

## About the repository

| | |
|---|---|
| [Design](design.md) | The rules every widget holds to, and what each one cost to learn. |
| [Internals](internals.md) | `toys-core`, the chart helpers, and what `cargo test` checks that a compiler cannot. |
| [Port decisions](port-decisions.md) | What the Rust port changed from the Python, and why. |
| [Building herdr panels](building-herdr-panels.md) | Driving these from Herdr: resize semantics, focus, and the layout mistakes worth skipping. |
| [Releasing](releasing.md) | How a version is decided, what merging the release PR sets off, and what to do when it goes wrong. |
