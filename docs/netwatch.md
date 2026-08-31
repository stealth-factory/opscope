# How `netwatch` measures traffic

`netwatch` uses different measurements on Linux and macOS because macOS has
no unprivileged per-socket byte counter equivalent to Linux `ss -tine`. The
pane names the active measurement; the two platforms do not put different
numbers under the same label.

| Platform | Process rows | Interface totals | Row meaning |
| --- | --- | --- | --- |
| Linux | `ss -tine`, joined to `/proc/<pid>/fd` by socket inode | `/proc/net/dev` | TCP payload bytes per socket, aggregated by owning process |
| macOS | persistent `nettop -P -x -L 0 -s 1 -J bytes_in,bytes_out` feed | `netstat -ib` link-layer rows | All-protocol cumulative network bytes per process, differenced from the first sample |

On Linux, `ip -o addr` supplies local addresses so loopback-to-self traffic
can be removed. The `external` setting can then narrow rows to internet peers.
On macOS, `nettop` has no peer or ownership field in per-process mode, so the
widget does not collect local addresses and cannot apply the Linux
internet-only or unattributed-socket filters. It does not claim that it has.

The macOS process-detail screen can show command, working directory and open
regular files through `ps` and `lsof`. A missing or failed `lsof` is named
rather than drawn as an empty file list. Endpoint, connection and disk-I/O
attribution are marked unavailable: those facts cannot be derived from the
per-process `nettop` counters without pretending that one measurement is
another.

`nettop` is a long-lived child. If it exits, the widget says so, waits, and
starts it again rather than freezing the process table on that one error.
Idle processes that drop out of a `nettop -P` sample keep their counter
baseline so their return is not counted as new traffic.

Every source failure is shown by name in the pane. An unavailable `ss`,
`nettop`, `ip`, `/proc/net/dev` or `netstat` source is not rendered as an
honestly empty table. Interface rates on macOS are shown without an
attribution percentage: process totals include loopback and virtual paths
that `netstat` excludes, so the ratio would not measure the same traffic.

Both platforms establish a first-sample baseline. Totals and rates therefore
describe traffic observed since `netwatch` started (or since `r` rezeroed),
not the lifetime counter that the operating-system source happened to expose.

See the [widget guide](../widgets/src/widgets/netwatch/README.md) for controls,
charts and the Linux socket-accounting details.
