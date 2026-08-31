# Latency acquisition on Linux and macOS

`latency` deliberately keeps one shared watcher and one shared presentation
path. The only variation is how the installed `ping` exposes a packet that did
not answer.

| Installed ping | Invocation | Loss evidence |
|---|---|---|
| iputils | `ping -n -O -i <seconds> <host>` | `no answer yet` and unreachable lines |
| BSD ping, including macOS | `ping -n -i <seconds> <host>` | native `Request timeout` lines, gaps in reply `icmp_seq` values, and a silence clock when no output arrives at all |

This is detected at runtime by asking the installed binary whether it accepts
`-O`. It is not selected with `cfg(target_os)`: iputils is not synonymous with
Linux, and another ping implementation can be installed on a Linux host.

Sequence gaps cover intermittent loss. Total silence has no next sequence
number to reveal the gap, so after `max(3 × interval, 2 seconds)` the watcher
records one missing sample per interval until a reply resets the clock. A
wall-clock jump larger than that grace — a sleeping Mac, a paused VM — is
not treated as loss: ping was frozen and transmitted nothing, so the clock
re-arms rather than filling the window with samples that were never sent. A
reply after either form of loss uses the ordinary recovery path. A gap that
is already closed by the reply that reveals it is recorded as empty samples
without a LOSS/UP pair — that pair would otherwise read as a 0s outage.
Statistics, events, retained history and drawing are otherwise unchanged.

The stock macOS ping accepts the widget's 0.2-second minimum as an unprivileged
user. The configured interval is therefore passed through unchanged on both
platforms.

Unit tests contain captured BSD reply lines with a sequence gap and the
iputils `no answer yet` line. Parser and dialect tests are always compiled, so
both paths remain visible to Linux and macOS CI.
