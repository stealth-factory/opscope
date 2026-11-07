# Configure `procs`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux reads `/proc` through the shared `tc::procfs` readers. Each poll it
lists `/proc`, and for every pid reads `stat`, `cgroup` and the owner of
the `/proc/<pid>` directory - all three readable for every process,
whoever owns it - plus `cmdline` only for a process whose `comm` is
empty. Three machine-wide files are read once a poll and feed the panel
above the table: `/proc/stat` (the `cpu` line and one `cpuN` line per
online CPU: the total busy share and a bar per core, busy being
everything but `idle` and `iowait`), `/proc/loadavg` (the three load
averages, and the 1-minute one kept a slot a poll for the 10-minute
trace) and `/proc/meminfo` (`MemTotal`, `MemFree`, `MemAvailable`,
`SwapTotal`, `SwapFree`: used is `MemTotal - MemAvailable`, reclaimable
`MemAvailable - MemFree`, free `MemFree`, so the three sum to the total).
`/proc/uptime` is read once a poll too. While `t` lists threads, each
multi-threaded process's `/proc/<pid>/task/<tid>/stat` is read as well,
and not otherwise.
%CPU is the change in `utime + stime` between two polls, divided by the
clock tick rate, the window and the number of CPUs; the window is the
kernel's own, from the `/proc/stat` totals. The tick rate and the page
size come from `sysconf`. User names come from `/etc/passwd`, then
`getpwuid_r` for a uid it does not list.

macOS runs one `ps -axo pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=,args=`
a poll, which lists every user's processes. Its `%cpu` is macOS's
decaying average, not a delta over the poll, and the header says so.
`hw.memsize` gives the total that MEM is a share of. The panel reads
`vm_stat` (its page size from the first line, then free, active,
inactive, speculative, wired and compressor pages: used is active +
wired + compressor, reclaimable inactive + speculative), `sysctl -n
vm.swapusage` and `getloadavg(3)` each poll; the CPU total is the sum of
`ps`'s `%cpu` over the core count, labelled as a decaying average, and
there is no per-core figure (that would need `host_processor_info`). A
`vm_stat` or `sysctl` that fails is named in the panel and the table
still draws. `ps` is a required tool on macOS; without it the widget
shows the shared missing-tool screen. macOS lists no threads, so `t` is
not offered there.

The services view (`g`, Linux only) walks `/sys/fs/cgroup` once a poll
while it is on screen, through the shared `tc::cgroup` reader: for each
cgroup, `cgroup.subtree_control`, `cgroup.procs`, and `cpu.stat` and
`memory.current` where those controllers are enabled; for the open
service, `memory.max`. All of these are readable by any user. Which
cgroups get rows is decided by which have the `cpu` or `memory`
controller enabled, and that is systemd's to set, not this widget's:
`systemctl set-property <unit> CPUAccounting=yes` (or
`MemoryAccounting=yes`) enables the controller for that unit's cgroup and
the slices above it, so it gets a row of its own instead of rolling up
into its slice; `DefaultCPUAccounting=yes` and
`DefaultMemoryAccounting=yes` in `system.conf` do it for every unit.
Whether a user's login sessions get rows of their own depends on what
`user.slice` and the user's slice enable below them; where they do not,
the sessions roll up into the slice and the header says so. On cgroup v1 there is no services view; the
view says so. Nothing in this widget's settings changes any of it.

The detail screen reads, for the one process that is open, its
`cmdline`, `cwd` and `/proc/<pid>/io`, every poll. Another user's `cwd`
and `io` are closed to an ordinary user; the rows say `needs root`
rather than drawing a zero. With `/proc` mounted `hidepid=invisible` (or
`2`), other users' processes are not listed at all and the counts say
`visible`. Nothing in the settings changes what can be read; running as
root does.

## Settings owned here

The owned section is `procs`.

Declared fields: `refresh`, `show_all_users`, `show_kernel_threads`, `show_threads`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

There is no `rows` setting (the pane scrolls; a cap would hide rows) and
no `cpu_window` setting (the %CPU window is the poll interval, and the
header names it).

## Safe configuration process

1. Read the resolved path shown by the settings screen and the current values before proposing changes.
2. Ask how often to poll (which also sets how many points the 10-minute load trace holds), whether the pane should start on every user's processes or only the user's own, whether kernel threads should be listed, and whether each process's threads should start listed under it (Linux). User names and process names are local data; do not copy them into tracked examples.
3. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
4. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the top rows agree with `top` (which shows % of one core by default: divide by the core count) and that RSS agrees with `ps -o rss`; the MEMORY row's used figure agrees with `free`'s total less available, and the load with `uptime`.

## Boundaries

Do not fabricate hostnames, user names, process names from someone else's machine, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
