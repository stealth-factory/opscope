# Port discovery on Linux and macOS

`ports` keeps one shared model and presentation path. Only acquisition differs
by operating system; every parser is compiled and tested on every target.

| Reading | Linux | macOS |
|---|---|---|
| listening TCP sockets, bind address, owner and pid | `/proc/net/tcp`, `/proc/net/tcp6`, then `/proc/<pid>/fd` | `lsof -nP -iTCP -sTCP:LISTEN -Fpcunt` |
| command and uptime | `/proc/<pid>/cmdline` and process metadata | one batched `ps -www -o pid=,etime=,args=` query |
| working directory | `/proc/<pid>/cwd` | one batched `lsof -d cwd -Fpn` query |
| local addresses | `ip -j addr`, falling back to `ifconfig -a` | `ifconfig -a`, or `ip -j addr` when installed |
| per-port traffic | `ss -tine` | connection rows from `nettop -m tcp -L 1` |

The macOS listener, process, cwd and traffic sources were verified against the
stock tools on a live Mac. The batch forms are important: a machine with many
listeners still performs one `ps` and one cwd `lsof` per scan, rather than
starting three subprocesses for each pid.

`lsof` ships with macOS, but it is still checked at runtime. If it is absent,
the widget names the missing tool and does not draw an empty table. A failed or
timed-out listener scan likewise reaches the pane as an error while preserving
the last good result.

macOS `nettop` logging output includes cumulative bytes and the local endpoint
for each TCP connection. The parser maps those connection rows to their local
listening ports, then the shared traffic history subtracts consecutive samples
exactly as it does for Linux `ss`. Process-summary and listener-only rows are
not counters and are ignored. If `nettop` is unavailable, the traffic columns
and chart stay off and the pane names the missing source; blank dots would
incorrectly claim that traffic was measured and happened to be zero.

Windows has no acquisition implementation. It shows the shared unsupported
screen rather than an empty inventory.

See the [widget guide](../widgets/src/widgets/ports/README.md) for controls,
layout and exposure behaviour.
