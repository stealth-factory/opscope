# `disk`

[← all widgets](../../../../docs/README.md)

Which filesystems can still take a write, and which are running out of inodes.

```
╺━ DISK ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sampled 2s ago · every 2s
 4 of 12 shown · pseudo, bind, read-only hidden
 FILESYSTEMS  usable = f_bavail (root reserve excluded) · sorted: mount
 MOUNT   TYPE   SIZE   USED  USABLE        %  INODES
 ▸ /     ext4   100G    80G     15G  ████  84%     12%
   /home xfs    200G    40G    160G  ██    20%      4%

 ↑↓ select  [↵] detail  [s] sort by % used  [p] pause  [,] settings  [q]uit
```

`↑↓` move the selection. `↵` opens the selected mount: its source, type,
options, size, used, usable, the root reserve, the inodes, and every other
mount on the same device. `esc` goes back. `s` cycles the sort through
mount order, percent used and inodes. `p` pauses the sample and the header
says so. `,` opens settings. `!` opens notices. `q` quits.

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

`s` orders by mount point as the kernel listed them, then by percent used
with the fullest first, then by inodes the same way. A row that has not
answered yet sorts last. The footer names the sort the next press moves to.

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

Columns leave as the pane narrows, inodes first, then type, then size. The
mount point wraps onto the next row rather than being cut. A pane that is
too short to hold the table scrolls; the title stays put.
