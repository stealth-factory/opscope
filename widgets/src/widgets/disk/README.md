# `disk`

[← all widgets](../../../../docs/README.md)

Which filesystems can still take a write, which disks are doing I/O, and, on Linux, whether I/O is stalling.

Linux example:

```
╺━ DISK ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sampled 2s ago · every 2s
 2 of 10 shown · pseudo, bind, read-only hidden
 FILESYSTEMS  usable = f_bavail (root reserve excluded) · sorted: mount
MOUNT   TYPE         SIZE   USED USABLE           %     1h Δ     7d Δ       FULL IN INODES
▸ /     ext4          100G   80G    15G █████░  84%  +1.2G/h  +8.0G/d     ~1d 17h ↑    12%
  /home xfs           200G   40G   160G █░░░░░  20%        —        —   not filling     4%
 FULL IN at the rate since Mon 09:12 (5d of 7d) · ↑↓ = 1h rate sooner/later
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

 ↑↓ select  [↵] detail  [s] sort by fills soonest  tab section  [v] show idle disks
 [w] 14d  [p] pause  [,] settings  [q]uit
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
cycles the projection window through 7, 14 and 30 days for this session.
`v` shows idle disks, or hides them again.
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
older than thirty days: about 4.2k points per filesystem. Twenty
filesystems stay under about 90k points, around 2 MB of JSON. A hole is a
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
