# Configure `disk`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux reads `/proc/self/mountinfo` and then `statvfs` on each mount that is
shown. macOS reads `getmntinfo` with `MNT_NOWAIT`, which returns the same
figures in one call. A mount that does not answer is named on its row. The
widget does not invent a zero for a figure it does not have.

Device rows on Linux come from `/proc/diskstats` each poll and from
`/sys/block` at most once a minute. Sectors in `diskstats` are 512 bytes.
macOS device rows come from `iostat` (`iostat -d -K -w`), which is
recommended rather than required: without it the filesystem table still
runs and the disk section says the tool is missing. `/proc/pressure/io` is
Linux-only. An absent file is named; it is not drawn as zero.

Samples this widget measured itself are kept in `usage.json` under the
shared state directory (`disk/usage.json`). A series is the mount, the
filesystem type and `f_blocks`. A resize or a reformat starts a new series.
A gap longer than the window on screen restarts the projection for that
window; the file still keeps the older points for a longer window. `w`
cycles 6, 12 and 24 hours for the session. `window_hours` is which of those
the pane starts on (24 by default). The disk detail screen does not offer
`w`: its I/O chart is in memory and has no window.

WHO IS WRITING needs no root. On Linux BY SERVICE reads cgroup v2
`io.stat` under `/sys/fs/cgroup` once a poll, and WAITING ON DISK reads
`/proc/<pid>/stat` for every process. What it can and cannot show is
decided by the host, not by this widget's settings:

- **`IOAccounting`.** A systemd unit gets its own BY SERVICE row only when
  the `io` controller is enabled for it; otherwise it rolls up into the
  nearest parent that has it, and the line under the section's heading names the depth.
  `systemctl set-property <unit> IOAccounting=yes` (as root) enables it
  for one unit, and the widget picks it up on the next poll. If no cgroup
  has it, the section says `io accounting is off everywhere`.
- **`task_delayacct`.** `/proc/sys/kernel/task_delayacct` at 0 (the usual
  default) leaves WAITING ON DISK without a WAITED column; the line under its heading says
  `wait times off (needs root: sysctl kernel.task_delayacct=1)`. Setting
  it to 1 (as root) adds per-process wait times; the widget reads it once
  a minute. It costs a little on every context switch, which is why
  kernels ship with it off; whether to turn it on is the user's call.
- **What root adds.** Another user's `/proc/<pid>/io`, `cwd`, open files
  and parts of `cmdline` are closed to an ordinary user. BY SERVICE is
  complete without them; the PROCS column, the line under BY PROCESS's heading (`412 of
  598 processes readable (yours) · the other 186 need root`) and the
  process detail say how many could not be read. Running as root makes
  every process readable; nothing in the widget's settings does.
- **`hidepid`.** With `/proc` mounted `hidepid=invisible` (or `2`), other
  users' processes are not listed at all, and BY PROCESS counts the
  `visible` ones and says so.

BY PROCESS reads `/proc/<pid>/io` in the same pass as WAITING ON DISK, and
`reconcile_seconds` (60, at least 10) is how long it sums the readable
processes' writes and the disks' before it compares them. Longer is
steadier; shorter follows a change sooner. A process's write counter moves
when a page is dirtied and the disk's when the write is submitted, so the
two only line up over a window, and the line says `≈`. A parent that
reaps a child inherits the child's counters; the widget takes the child's
last reading back out of the parent's next one, so those bytes count once
and a parent that only waited is not a writer.

Cgroup v1 or hybrid hosts get `by service needs cgroup v2`. On macOS the
section is BY PROCESS alone: `proc_pid_rusage` for every process this user
owns (another user's is counted as needing root), named by one
`ps -axo pid=,comm=` a poll, and compared with `iostat`'s combined MB/s.
Running the widget as root there reads every process as well.

## Settings owned here

The owned section is `disk`.

Declared fields: `refresh`, `hide_mounts`, `hide_devices`, `warn_percent`, `alert_percent`, `window_hours`, `top_writers`, `show_kernel_threads`, `reconcile_seconds`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

## Safe configuration process

1. Read the resolved path shown by the settings screen and the current values before proposing changes.
2. Ask which mount points to hide, how often to sample, and where the bar should start to heat. Paths are local data; do not copy them into tracked examples.
3. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
4. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the figures agree with the kernel, or that the pane gives a specific reason why it cannot.

## Boundaries

Do not fabricate hostnames, account names, mount paths from someone else's machine, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
