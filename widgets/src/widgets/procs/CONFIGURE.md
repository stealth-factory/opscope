# Configure `procs`

This is configuration guidance for people and AI assistants. It is documentation, not an executable skill and not permission to change files or external services.

## Real source

Linux reads `/proc` through the shared `tc::procfs` readers. Each poll it
lists `/proc`, and for every pid reads `stat`, `cgroup` and the owner of
the `/proc/<pid>` directory - all three readable for every process,
whoever owns it - plus `cmdline` only for a process whose `comm` is
empty. `/proc/stat` (the `cpu` line and one `cpuN` line per online CPU),
`/proc/uptime` and `/proc/meminfo` (`MemTotal`) are read once a poll.
%CPU is the change in `utime + stime` between two polls, divided by the
clock tick rate, the window and the number of CPUs; the window is the
kernel's own, from the `/proc/stat` totals. The tick rate and the page
size come from `sysconf`. User names come from `/etc/passwd`, then
`getpwuid_r` for a uid it does not list.

macOS runs one `ps -axo pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=,args=`
a poll, which lists every user's processes. Its `%cpu` is macOS's
decaying average, not a delta over the poll, and the header says so.
`hw.memsize` gives the total that MEM is a share of. `ps` is a required
tool on macOS; without it the widget shows the shared missing-tool screen.

The detail screen reads, for the one process that is open, its
`cmdline`, `cwd` and `/proc/<pid>/io`, every poll. Another user's `cwd`
and `io` are closed to an ordinary user; the rows say `needs root`
rather than drawing a zero. With `/proc` mounted `hidepid=invisible` (or
`2`), other users' processes are not listed at all and the counts say
`visible`. Nothing in the settings changes what can be read; running as
root does.

## Settings owned here

The owned section is `procs`.

Declared fields: `refresh`, `show_all_users`, `show_kernel_threads`

The field types, defaults, order, and inline help come from `settings.json` in this folder. Use the widget's settings screen (press `,`) instead of constructing JSON by hand.

There is no `rows` setting (the pane scrolls; a cap would hide rows) and
no `cpu_window` setting (the %CPU window is the poll interval, and the
header names it).

## Safe configuration process

1. Read the resolved path shown by the settings screen and the current values before proposing changes.
2. Ask how often to poll, whether the pane should start on every user's processes or only the user's own, and whether kernel threads should be listed. User names and process names are local data; do not copy them into tracked examples.
3. Change only this widget's declared section. Keep secrets out of chat, logs, shell history, source files, and screenshots.
4. Save through the settings screen and leave it — the widget reloads itself on the way out, so no restart is needed. Then verify that the top rows agree with `top` (which shows % of one core by default: divide by the core count) and that RSS agrees with `ps -o rss`.

## Boundaries

Do not fabricate hostnames, user names, process names from someone else's machine, credentials, or successful readings. If a value cannot be established from the local environment or the user's explicit instruction, ask.
