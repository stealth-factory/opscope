# `disk`

[← all widgets](../../../../docs/README.md)

Which filesystems can still take a write, which disks are busy, and whether writes are stalling.

```
╺━ DISK ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sampled 2s ago · every 2s
 4 of 12 shown · pseudo, bind, read-only hidden
 FILESYSTEMS  usable = f_bavail (root reserve excluded) · sorted: mount
 MOUNT   TYPE   SIZE   USED  USABLE        %   1h Δ     7d Δ      FULL IN  INODES
 ▸ /     ext4   100G    80G     15G  ████  84%  +1.2G/h  +8.0G/d  ~1d 17h ↑     12%
   /home xfs    200G    40G    160G  ██    20%       —        —   not filling     4%
 DISKS  whole disks only · /proc/diskstats · 512-byte sectors
 DISK     TYPE    SIZE   READ  WRITE  IOPS  AWAIT  BUSY*  QUEUE
 ▸ sda    ssd     1.0T   12M   40M    1.2K   2ms    18%      1

 ↑↓ select  [↵] detail  [s] sort by fills soonest  [w] 14d  tab section  [v] show idle disks  [p] pause
```

`↑↓` move the selection, including from one section into the next. `tab`
moves between sections. `↵` opens the selected mount or the selected disk.
A mount's detail adds the disk and partition it sits on, `on nvme0n1
(nvme0n1p2)`. A disk's detail names the model, the type, the size, each
partition and the mounts on it, the totals since boot, and the current
rates. `esc` goes back. `s` cycles the sort of the section that has the
cursor: filesystems through mount order, fills soonest, percent used and
inodes; disks through write, read and name. `w` cycles the projection
window through 7, 14 and 30 days for this session. `v` shows idle disks,
or hides them again.
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

`s` orders by mount point as the kernel listed them, then by which
filesystem fills soonest, then by percent used with the fullest first, then
by inodes the same way. `not filling` and `measuring` sort after every
projection, in mount order. A row that has not answered yet sorts last.
The footer names the sort the next press moves to.

## What is hidden

Four rules, and a mount is counted under the first one that applies:
`hide_mounts`, then a pseudo filesystem, then read-only, then a second
mount of a device that is already shown (the shorter path stays). The count
line names only the rules that hid something. When nothing is hidden the
line is absent.

`tmpfs` is not pseudo. It can fill. The pseudo list is proc, sysfs, cgroup,
cgroup2, devpts, devtmpfs, debugfs, tracefs, securityfs, pstore, bpf,
configfs, fusectl, hugetlbfs, mqueue, autofs, binfmt_misc, efivarfs,
rpc_pipefs, nsfs, ramfs, devfs and fdesc.

`hide_mounts` is an exact path, or a prefix when the entry ends in `/`.
The entry `/` hides only the root mount.

## When a number is missing

The first frame says `sampling…` and draws no zeroes. A `statvfs` that does
not return within two seconds leaves `no answer in 2s` on that row and is
not asked again until it does return. If the mount table itself cannot be
read, the pane says `mounts unavailable:` and keeps the last sample. A
poller that stops says `stopped:` and why.

Columns leave as the pane narrows: the one-hour rate, then inodes, then
type, then size. The window rate and FULL IN stay. A row that still does
not fit wraps whole cells onto the next line. The mount point wraps onto
the next row rather than being cut. A pane that is too short to hold the
table scrolls; the title stays put.

## What this widget measured itself

`1h Δ`, the window `Δ` and `FULL IN` are numbers this widget measured
itself, kept in `disk/usage.json`. At most one sample a minute for the
last day, then one every fifteen minutes, and nothing older than thirty
days: at most 1,440 + 2,784 points, about 4.3k, per filesystem. Twenty
filesystems stay under about 90k points, around 2 MB of JSON. A hole is a
gap in the trace, not a slope invented across the time the pane was not
running. The footnote names the window and when the series started. `↵` on
a filesystem draws that trace.

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
count is on the filter line (`7 idle hidden`). `hide_devices` is a second
category on that line. The selected disk keeps a 60-sample read-up,
write-down chart. Read sits on a zero baseline and write on the top of the
cell, so an idle stretch draws nothing.

On macOS the same section is `iostat`: read and write are one number, and
the heading says await, busy and queue need IOKit. PRESSURE is not drawn
on macOS.

When `/proc/1/cgroup` is `0::/`, the disk heading also says `host devices,
container filesystems`.

## Pressure

Linux only. The row is stall time from `/proc/pressure/io`, some and full,
over 10, 60 and 300 seconds. The heading says it is stall time, not busy
time. A kernel built without PSI, or one booted with `psi=0`, shows `PSI
not available on this kernel (CONFIG_PSI or psi=0)` and never a zero.
