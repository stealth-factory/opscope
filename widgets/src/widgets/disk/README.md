# `disk`

[← all widgets](../../../../docs/README.md)

Which filesystems can still take a write, which disks are doing I/O, and, on Linux, whether I/O is stalling.

Linux example:

```
╺━ DISK ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sampled 2s ago · every 2s
 2 of 10 shown · pseudo, bind, read-only hidden
 FILESYSTEMS  usable = f_bavail (root reserve excluded) · sorted: mount
MOUNT   TYPE         SIZE   USED USABLE           %     1h Δ    24h Δ       FULL IN INODES
▸ /     ext4          100G   80G    15G █████░  84%  +1.2G/h  +8.0G/d     ~1d 17h ↑    12%
  /home xfs           200G   40G   160G █░░░░░  20%        —        —   not filling     4%
 FULL IN at the rate since Mon 09:12 (5h of 24h) · ↑↓ = 1h rate sooner/later
 DISKS  whole disks only · /proc/diskstats · 512-byte sectors · sorted: write · QUEUE is
 instantaneous
DISK                                     TYPE    SIZE   READ WRITE  IOPS AWAIT BUSY* QUEUE
  sda                                    ssd        1T   1.5M   18M  1.2K   2ms   18%     1
  sdb                                    ssd        1T   400K  1.0M  48.0   1ms    2%     0
 * BUSY = share of time with at least one request in flight (field 10); an NVMe reaches
 100% with headroom
 total of sda, sdb
┌read  peak 3.3M/s  now 1.9M/s───────────────────────────────────────────────────────────┐
│⣀⠤⠤⠤⠒⠒⠒⠉⠉⠉⠉⠉⠉⠉⠉⠉⠒⠒⠒⠒⠢⢄⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⡠⠤⠤⠔⠒⠒⠊⠉⠉⠉⠉⠉⠉⠉⠉⠑⠒⠒⠤⠤⠤⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀│
│                                                                                        │
├0───────────────────────────────────────────────────────────────────────────────────────┤
│                       ⣀⣀⣀⠤⠤⠤⠤⠔⠒⠒⠒⠒⠒⠊⠉⠉⠉⠉⠒⠒⠒⠒⠒⠒⠤⠤⠤⠤⢄⣀⣀⡀                                 │
│⠤⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⡠⠤⠤⠤⠤⠒⠒⠒⠒⠊⠉⠉                               ⠈⠉⠉⠑⠒⠒⠒⠒⠤⠤⠤⠤⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⡠⠤⠤⠤⠤⠒⠒⠒⠒⠊⠉⠉│
└write  peak 36M/s  now 19M/s────────────────────────────────────────────────────────────┘
 /proc/pressure/io · stall time, not busy time
 some 23.1% avg10 · 18.4% avg60 · 9.7% avg300   full 7.2% · 5.1% · 2.0%

 WHO IS WRITING  sda · last 2s · 18M/s written · 1.5M/s read
 BY SERVICE  cgroup v2 io.stat · complete · sorted: write · io accounting down to:
 services; user sessions roll up to user.slice
CGROUP                              WRITE    READ SHARE PROCS
▸ system.slice/example-db.service   16M/s  1.2M/s   89% 12 · none readable as you
  user.slice                       1.6M/s  300K/s    9% 203 · 198 readable
  /                                400K/s    0B/s    2% 1 · not readable
 accounted: 18M/s of 18M/s

 WAITING ON DISK  3 in D state now · wait times off (needs root: sysctl
 kernel.task_delayacct=1) · 1 kernel thread hidden
  PID     COMMAND      STATE CGROUP
  1204    example-db   D     system.slice/example-db.service
  48190   example-sync D     user.slice/user-1000.slice/session-2.scope

 ↑↓ select  [↵] detail  [s] sort by read  tab section  [v] show idle disks
 [w] 6h  [p] pause  [,] settings  [q]uit
```

`↑↓` move the selection, including from one section into the next. `tab`
moves between sections. `↵` opens the selected mount or the selected disk.
A mount's detail adds the disk and partition it sits on, `on nvme0n1
(nvme0n1p2)`. On Linux a disk's detail names the model, the type, the
size, each partition and the mounts on it — including a filesystem mounted
on the whole disk — the totals since boot, and the current rates. macOS
does not have those: the detail says `iostat` does not provide the model,
the type, the size, the partitions or the totals. `esc` goes back. `s`
cycles the sort of the section that has the cursor: filesystems through
mount order, fills soonest, percent used and inodes; disks through write,
read and name. On macOS the write key is combined MB/s and the read key
is transfers per second, and the heading names the one in force. `w`
cycles the projection window through 6, 12 and 24 hours for this session;
the pane starts on the `window_hours` setting, 24 by default (an old
`window_days` setting is ignored). The disk detail screen does not offer
`w`: its I/O chart is kept in memory and has no window, and neither do
the WHO IS WRITING screens.
`v` shows idle disks, or hides them again.
On Linux, `tab` also reaches BY SERVICE and WAITING ON DISK. There `↵`
opens a cgroup's detail, a process's detail, or, on `+N more`, everyone
in D state. `s` cycles BY SERVICE through write, read and name, and
WAITING through time waited and pid when wait times are on. `a` shows
every cgroup in BY SERVICE, or only the top `top_writers` again; the hint
appears once there are more cgroups than that.
`p` pauses the sample and the header says so. `,` opens settings. `!`
opens notices. `q` quits.

The wheel, `ctrl-y` and `ctrl-e` scroll the view and leave the selection
where it is. `pgup`, `pgdn`, `home` and `end` move that same view.

## What a row is

One filesystem that can still take a write. Linux reads
`/proc/self/mountinfo` and then `statvfs`. macOS reads `getmntinfo`.
Percent used is used / (used + usable). Usable is `f_bavail`, which already
excludes the root reserve, so the percentage is not used / size. The header
says so.

Inodes are used / total. A filesystem that does not count them (`f_files`
is zero) shows `n/a` rather than zero. A remote type — nfs, nfs4, cifs,
smb3, smbfs, afpfs, sshfs, and anything under `fuse.` — is dim in the type
column only.

`s` cycles four orders: mount point as the kernel listed them; which
filesystem fills soonest; percent used, fullest first; inodes the same
way. Mount order breaks the ties. `not filling` and `measuring` sort
after every projection, in mount order. A row that has not answered yet
sorts last. The footer names the sort the next press moves to.

## What is hidden

Five rules, and a mount is counted under the first one that applies:
covered by a later mount at the same path, then `hide_mounts`, then a
pseudo filesystem, then read-only, then a second mount of a device that is
already shown (the shorter path stays). The count line names only the
rules that hid something, and it calls a mount hidden by a later one at
the same path `covered`. When nothing is hidden the line is absent.

`tmpfs` is not pseudo. It can fill. The pseudo list is proc, sysfs, cgroup,
cgroup2, devpts, devtmpfs, debugfs, tracefs, securityfs, pstore, bpf,
configfs, fusectl, hugetlbfs, mqueue, autofs, binfmt_misc, efivarfs,
rpc_pipefs, nsfs, ramfs, devfs and fdesc.

`hide_mounts` is an exact path, or a prefix when the entry ends in `/`.
The entry `/` hides only the root mount.

## Settings

`,` opens the settings screen. The section is `disk`.

`refresh` is the seconds between samples. It defaults to 2. Anything below
0.5 is raised to 0.5, anything above a day is cut to a day, and a value
that is not a positive number falls back to 2. `disk -n SECONDS` overrides
it for that run, with the same limits.

`warn_percent` and `alert_percent` default to 80 and 90. At and below the
warning the bar stays quiet; at the alert it is fully hot, and between
them it climbs in a straight line.

`top_writers` (8, at least 1) is how many cgroups BY SERVICE shows before
`a`. `show_kernel_threads` (false) lists kernel threads in WAITING ON DISK
instead of only counting them. Both are Linux-only.

## When a number is missing

The first frame says `sampling…` and draws no zeroes. A `statvfs` that does
not return within two seconds leaves `no answer in 2s` on that row and is
not asked again until it does return. If the mount table itself cannot be
read, the pane says `mounts unavailable:` and keeps the last sample. A
poller that stops says `stopped:` and why.

Columns leave as the pane narrows: the one-hour rate, then inodes, then
type, then size. The window rate and FULL IN stay. A row that still does
not fit wraps whole cells onto the next line. When the longest mount
point cannot sit whole beside its figures, every mount point gets a line
of its own and its figures go under it, indented, with the full width to
choose columns from. A mount point longer than the pane wraps rather than
being cut, and so does an error. A pane that is too short to hold the
table scrolls; the title stays put.

## What this widget kept

The one-hour rate and the window rate come from samples this widget took
itself. `FULL IN` projects that window rate onto the usable space now.
`disk/usage.json` keeps the samples, not those rates. At most one sample
a minute for the last day, then one every fifteen minutes, and nothing
older than seven days: about 2k points per filesystem. Twenty
filesystems stay under about 40k points, around 1 MB of JSON. A hole is a
gap in the trace, not a slope invented across the time the pane was not
running. The footnote says the rate is since the first fitted sample, and
a full window also says when the series has been kept. `↵` on a filesystem
draws that trace through the last closed minute, so a sample still being
taken does not move it.

## Disks

Whole disks only. A partition is not a row and is not added into its disk.
The heading says `whole disks only`, that the counters are
`/proc/diskstats`, and that those sectors are `512-byte sectors`. Linux
columns are disk, type, size, read, write, iops, await, busy and queue.
They leave in that order from the right: queue, then await, then type,
then size, then iops. Busy is the share of the interval with at least one
request in flight. Queue is the instantaneous in-flight count. The first
sample says `measuring`. A counter that went backwards is a gap, drawn as
`—`, not a spike.

A disk with no reads and no writes since boot is hidden until `v`. The
count is on the filter line (`7 idle hidden`). `hide_devices` matches a
whole-disk name exactly, and that disk stays hidden when `v` shows idle
disks. Its count is the next category on the same line. Below the table
is one 60-sample read-up, write-down chart in the same frame as netwatch.
On Linux it is the sum of every disk until the disk section has focus;
then it is the disk under the cursor, named on the line above it. The top edge names the read peak and the current read;
the bottom edge names the write peak and the current write; the zero line
sits between them. Text mode
draws the line in Braille; a Kitty terminal draws the same line in pixels
under the frame. Each half has its own scale, and an idle stretch stays
blank inside the frame. On macOS the chart is one trace of combined I/O.

The sum leaves out a disk named as another's slave, and a disk whose
partition is that slave, and its line says so, because adding both would
count the same bytes twice. A disk hidden by name stays in the sum. A
sample where any included disk has no rate is a gap, not a partial sum.

On macOS the same section is `iostat`: read and write are one number, and
the heading says await, busy and queue need IOKit. The write sort is that
combined MB/s and the read sort is tps. PRESSURE is not drawn on macOS.

Inside a container (`/.dockerenv`, `/run/.containerenv`, or a non-empty
`container=` on pid 1) the disk heading also says `host devices, container
filesystems`. `/proc/diskstats` there is still the host's devices. A
`0::/` cgroup line is not that test: it is also what a private cgroup
namespace shows, and what a host shows when pid 1 stays in the root cgroup.

## Pressure

Linux only. The row is stall time from `/proc/pressure/io`, some and full,
over 10, 60 and 300 seconds. The heading says it is stall time, not busy
time. A kernel built without PSI, or one booted with `psi=0`, shows `PSI
not available on this kernel (CONFIG_PSI or psi=0)` and never a zero.

## Who is writing

Linux only, and only on the list screen. It describes one disk: the one
the DISKS cursor was put on, or until then the disk the table shows first
(sorted by write, the busiest). The heading names it, the interval, and
that disk's own write and read rates from `/proc/diskstats`.

**BY SERVICE** is complete: every cgroup that moved bytes on that disk in
the interval, from cgroup v2 `io.stat`, which the kernel keeps for every
process whether this user can read the process or not. Nothing here
needs root. `io.stat` is hierarchical, so a parent's figure already holds
its children's; each row is the cgroup's *own* share, with every child
that has `io` enabled taken out, so the rows add up without counting a
byte twice. The shares are taken over the interval's change rather than
from two readings, because a cgroup that went away leaves its bytes in
its parent's counter, and that parent would otherwise show them all at
once. Where `io` is not enabled below a cgroup, its children roll up into
it, and the heading says how deep accounting goes on this host (`io
accounting down to: services; user sessions roll up to user.slice`). `/`
is the root's own share: I/O no child cgroup was charged for. A
partition's bytes count toward its disk. A dm or md device above the disk
is a disk row of its own, and the kernel already charges what it passes
down to this disk, so it is not added again.

WRITE and READ are bytes per second over the interval. SHARE is the
row's part of the cgroups' summed write. PROCS is how many processes land
in the row and how many of them this user can read `/proc/<pid>/io` for;
the rest of their per-process figures need root. The table shows the top
`top_writers` in the sort's order; the line above it says how many it
left out, and `a` shows them. Columns leave as the pane narrows: who is
readable, then PROCS, then SHARE, then READ. A path wider than its column
wraps.

The line under the table ties the cgroups to the device over the same
interval. `accounted: 187M/s of 188M/s · the rest, 1M/s, is writeback on
filesystems without cgroup writeback, or root's own` names what is left
over whenever the two figures read differently: writeback on a filesystem
without cgroup writeback (anything but ext2, ext4, btrfs, f2fs and xfs) is
charged to the root, and so is the root's own I/O. When
the cgroups add up to more than the device by over 2%, it does not call
that accounted: `cgroups report 203M/s against 188M/s at the device ·
dirtying runs ahead of submission`. Under 2% it says the two were read a
moment apart. An idle disk still draws the table's header and `accounted:
0B/s of 0B/s`.

Instead of the table: `by service needs cgroup v2` on a cgroup v1 or
hybrid host; `io accounting is off everywhere · systemctl set-property
<unit> IOAccounting=yes enables it per unit` when no cgroup has `io`; the
reason when the tree cannot be read. Inside a container the heading adds
`rows are this container's cgroups; the device total is the host's`. The
first poll says `measuring`, because a rate needs two.

`↵` on a row shows the full cgroup path, the controllers enabled there,
its `io.pressure`, how many cgroups below it roll up into it and how to
give one its own row, its rates on the disk, and every pid in it with its
name and whether this user can read it.

**WAITING ON DISK** is every process in `D` state now - a sample of this
instant, not a count over time - with its cgroup, which is readable for
every process. With `/proc/sys/kernel/task_delayacct` at 1, WAITED is the
time each spent waiting on block I/O since the last poll (`1.8s of 2s`,
from field 42 of `/proc/<pid>/stat`), and the rows are ranked by it. At 0,
the default, there is no WAITED column at all rather than a column of
zeros, the header says `wait times off (needs root: sysctl
kernel.task_delayacct=1)`, and the rows are in pid order, a sample. A
kernel that does not print field 42 is the same as 0. The widget reads
the setting once a minute. Kernel threads (children of `kthreadd`) wait
on someone else's behalf, so they are hidden unless
`show_kernel_threads` is set, and the header counts them. Five rows are
shown, then `+N more`; `↵` there lists everyone in D. `↵` on a process
shows its command line, its working directory if this user can read it,
and `the rest needs root` when its `io` cannot be read. Columns leave as
the pane narrows: CGROUP first, then STATE.

On macOS this section is not drawn. By service is cgroup v2, which a Mac
does not have, and `ps` has no state that means what `D` means on Linux
(`U` is the nearest, and it is a different thing). It is left out rather
than drawn empty, because an empty table would read as nothing writing.

Each poll is one walk of `/sys/fs/cgroup` and one pass over `/proc`
(`stat` and `io` per process). The previous tree and each process's
previous wait ticks are the only history kept.
