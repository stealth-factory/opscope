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
On macOS, `ifconfig -a` supplies local addresses for source visibility, but
`nettop` has no peer or ownership field in per-process mode. The widget
therefore cannot apply the Linux internet-only or unattributed-socket filters
on macOS and does not claim that it has.

The macOS process-detail screen can show command, working directory and open
regular files through `ps` and `lsof`. It explicitly marks endpoint,
connection and disk-I/O attribution unavailable: those facts cannot be
derived from the per-process `nettop` counters without pretending that one
measurement is another.

Every source failure is shown by name in the pane. An unavailable `ss`,
`nettop`, `ip`, `ifconfig`, `/proc/net/dev` or `netstat` source is not rendered
as an honestly empty table.

Both platforms establish a first-sample baseline. Totals and rates therefore
describe traffic observed since `netwatch` started (or since `r` rezeroed),
not the lifetime counter that the operating-system source happened to expose.

See the [widget guide](../widgets/src/widgets/netwatch/README.md) for controls,
charts and the Linux socket-accounting details.
