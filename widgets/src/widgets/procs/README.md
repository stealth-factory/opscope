# `procs`

[← all widgets](../../../../docs/README.md)

What this machine is running: every process, its CPU, memory and state.

Linux example, 100 columns:

```text
╺━ PROCS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸

 ── MACHINE ── sampled 1s ago · every 2s ──────────────────────────────────────────────────────────
 CPU    8 cores · 41% busy · load 3.90 2.80 2.10 (of 8)
 ▆▆▆▆
 ████ ▃▃▃▃ ▁▁▁▁                ▆▆▆▆ ▁▁▁▁
 ████ ████ ████ ▂▂▂▂ ▄▄▄▄ ▆▆▆▆ ████ ████
   92   48   38   11   20   29   61   35
 per core, % over the last poll · busiest cpu0 92%

 MEMORY   31G  ███████████████████▌██████░░░░░░  used 18G · reclaimable 6G · free 7G
               used = total − MemAvailable
 SWAP      4G  ████████░░░░░░░░░░░░░░░░░░░░░░░░  1.2G used

 LOAD   1-minute average, last 10 min
 8 cores┤⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁⠁
        │⢀⣠⣤⣤⣶⣶⣶⣶⣶⣶⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣤⣄⣀⡀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⠀⢀⣀⣤⣤⣶⣶⣾⣿⣿⣿⣷⣶⣶⣤⣤⣄⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⣀⡀
        │⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣶⣶⣤⣤⣤⣤⣤⣤⣤⣤⣶⣶⣶⣾⣿⣿⣿⣿⣶⣶⣶⣶⣶⣶⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿ 3.90
       0┤⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿⣿

 ── PROCESSES ── 598 · sorted by %CPU · 3 zombies · 112 kernel threads hidden ─────────────────────
 %CPU of all 8 cores over the last 2s
     PID COMMAND        USER        %CPU   MEM   RSS STATE RUNNING CGROUP
▸  48213 example-worker user-a      12.4  4.1%  1.3G R       2d    session-2.scope
   51020 example-agent  user-a       2.8  2.0%  640M S       3h    session-2.scope
   48990 example-sync   user-a       1.1  0.9%  280M D       41m   session-2.scope
    1204 example-db     service-b    0.6 12.3%  3.9G S       9d    example-db
     733 example-vpn    root         0.4  0.1%   45M S       3d    example-vpn
   51077 (a) b (c)      user-a         —  0.0%     0 Z       12s   session-2.scope

 D = waiting on disk, 1 shown now
 ↑↓ select  [↵] detail  [s] sort by memory  [/] filter  [u] only yours  [t] show threads
 [g] by service  [r]efresh  [,] settings  [ctrl-k]cmds  [ctrl-t]heme  [q]uit
```

`g` on Linux switches to one row per service (see [By service](#by-service-linux)):

```text
╺━ PROCS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸

 ── SERVICES ── sampled 1s ago · every 2s · cgroup v2 · complete ──────────────────────────────────
 %CPU of all 8 cores over the last 2s · 7 cgroups with cpu or memory · sorted by %CPU
 cpu accounting down to: services, scopes; user sessions roll up to user.slice
 6 of 7 shown · 1 empty hidden
 SERVICE                           %CPU MEMORY PROCS
▸user.slice                        88.4   9.5G   203
 system.slice/example-db.service   31.2   3.8G    12
 system.slice/example-log.service   0.4   120M     1
 /                                  0.3      —     1
 system.slice                       0.0     4G     0
 init.scope                         0.0     0B     1

 the root has no memory.current of its own
 ↑↓ select  [↵] its processes  [s] sort by memory  [/] filter  [u] only yours  [g] by process
 [r]efresh  [,] settings  [ctrl-k]cmds  [ctrl-t]heme  [q]uit
```

The screen is in two parts, each under a rule: **MACHINE**, the panel,
with when it was sampled and how often, and **PROCESSES**, the table,
with how many there are, the sort, what is hidden and what %CPU is a
share of - what does not fit on the rule goes, whole, on the lines under
it. In the services view one **SERVICES** rule stands where both are.
Below, *the heading* is the list's rule and the lines under it.

On a pane tall enough, everything down to the column header stays put
and only the table scrolls: the wheel, `ctrl-y` / `ctrl-e` and the
cursor all move the table's own window, and the panel and its charts do
not move. That needs the table to keep at least 6 rows and a third of
the pane - 100×40 with eight cores does, 100×24 does not. A pane shorter
than that scrolls the whole body under the title instead, so nothing is
out of reach: pinned there, the head would leave the list a slot, and a
list too small to read is as good as a list that is not there. A head
that grows by a row or three - the filter box opening, a note on the
panel - does not switch the pane back and forth; a resize decides again.

In Kitty the load trace is pixels: a smooth line with the area under it,
coloured by height along the heat ramp - calm up to the core count, red
at twice it - the core count as a dashed line across it, and a dot on the
latest reading. In text the same trace is Braille with every dot under
the line set, each row in its height's colour and the core-count row
dotted. The per-core bars are eighth-cell text in every terminal, since
they change every poll and as an image were a whole upload each time;
the memory and swap bars are text everywhere too, as every meter is. On a pane wide enough
for both to stay on one line each - about 130 columns with eight cores -
MEMORY and SWAP move up beside CPU.

## The panel

Above the table, the machine as a whole, from three files read once a
poll on Linux:

- **CPU**: the core count, the share of every core that was busy over the
  last poll (`/proc/stat`: everything but `idle` and `iowait`, so `steal`
  counts as busy), and the three load averages from `/proc/loadavg` with
  the core count beside them, so a load over the core count reads as one.
  Under it, a bar per core, 0 to 100% over three rows, standing in a
  track that shows its full height and coloured by that core's own load,
  when four or more cores fit the width - wider bars, and each core's
  figure under its bar in the same colour, as the pane grows - and
  otherwise one meter of the total. The first poll has nothing
  to subtract from and says when the figures will come.
- **MEMORY**: three parts that sum to `MemTotal` exactly - used is
  `MemTotal - MemAvailable`, reclaimable `MemAvailable - MemFree`, free
  `MemFree` - so `used` is what `free` prints as total less available.
  Not `Cached`, which overlaps `MemAvailable`. On an idle machine whose
  `MemAvailable` is under `MemFree`, nothing is reclaimable and free is
  `MemAvailable`, so the parts still sum to the total.
- **SWAP**: its size and how much is used, `none used`, or `no swap`,
  the bar and its figure coloured by how full it is.
- **LOAD**: the 1-minute load average at every poll for the last ten
  minutes (300 points at `refresh` 2; a poll whose load could not be read
  is a gap, and so is every interval a stalled or failing poller missed,
  so an outage is never joined across as a trend), the axis topped at the core count until the load passes it,
  the core count marked on the axis and as a line across the chart, and
  the latest figure at the line's end. The colour is by height, not by
  sample: the same load is the same colour wherever it appears. Until ten minutes have passed it says how long it has so far.

A file that cannot be read is named in the panel (`/proc/stat
unavailable: …`), and the table still draws. The panel stays put above
the table when the pane is tall enough (see above) and scrolls with it
when not; the pane opens at the top.

**macOS** has no per-core figure in this version - it would need
`host_processor_info` - and the panel says so in one line rather than
drawing a row of empty bars. Its total is the sum of `ps`'s `%cpu` over
the core count and is labelled `decaying average (ps)`; memory is
`vm_stat`'s (used is active + wired + compressed pages, reclaimable
inactive + speculative, at the page size `vm_stat` prints), labelled
`from vm_stat`; swap is `sysctl -n vm.swapusage`; the load is
`getloadavg`.

## Threads

`t` lists each multi-threaded process's threads under it, Linux only: the
tid in PID, `└ name` in COMMAND, the thread's own %CPU over the same
window, its state and how long it has been running. Memory, user and
cgroup are the process's and are not repeated. Threads are never counted
as processes - the heading's count is unchanged and names how many threads
are listed - and a thread row belongs to its process: a click on it
selects the process, and `↑` `↓` step from process to process.
`/proc/<pid>/task/` is read only while threads are shown. macOS lists no
threads, so `t` is not offered there.

## By service (Linux)

`g` switches the table to one row per cgroup with the `cpu` or `memory`
controller enabled, and back; each view keeps its own selection and
scroll. Linux only, and only on cgroup v2: macOS has no cgroups, so
there is no services view there and `g` is not offered.

| column | what it is |
|---|---|
| SERVICE | the cgroup's path under `/sys/fs/cgroup`, `/` for the root. A path too long for the column wraps; it is never cut |
| %CPU | the cgroup's **own** share of `cpu.stat` `usage_usec` over the window between the last two walks of the tree, of all cores, like the process table |
| MEMORY | its own share of `memory.current`. The root has no `memory.current`, so its row reads `—` and a line under the table says why |
| PROCS | the processes in it, from `cgroup.procs` |

**Own, never twice.** `cpu.stat` and `memory.current` count everything
under a cgroup. Each row is what is left once its nearest descendants
that carry the controller are taken out - core's `tc::cgroup::attribute`,
the same attribution `disk` uses for `io` - so the rows add up to the
machine's total and a slice's row is only what its services do not
account for. For CPU the two walks are subtracted first and the
difference attributed. A cgroup without the controllers (a login
session under a `user.slice` that does not delegate them) rolls up into
the nearest ancestor that has them, with its processes; how deep that
goes on this host is read from the tree each poll and said in the
heading, as core words it: `cpu accounting down to: services, scopes;
user sessions roll up to user.slice`.

The two controllers need not reach the same depth. A slice that
delegates `memory` but not `cpu` to its services gives each service a row
for its memory and processes, with `—` in %CPU and a line under the
table saying their CPU is counted in the slice's row - so the slice can
read 14% with 0 processes of its own.

The walk is not atomic: a busy cgroup's children are read a moment after
it, and now and then they have moved more than it did over the window.
Its own share then cannot be taken; the row reads `—`, the heading counts
`1 row with a gap this poll`, and the next poll measures it again.

A cgroup under a row whose `cpu.stat` or `memory.current` cannot be read
leaves its usage inside that row's, and so does one removed between two
walks (a transient service or scope that ran and finished: what it used
stays in its parent's `cpu.stat`). The row's own share is then not known:
it reads `—`, a line under the table says why, and the heading says
`partial` and counts `1 row holding an unread cgroup's usage`.

The first walk has nothing to subtract from, so %CPU waits for the next
and the heading says when. The tree is walked only while the services
view (or one service) is on screen, once a poll after the `/proc` pass,
and the previous tree is the only history kept.

Rows with no CPU over the window, no memory and no processes are hidden
and counted: `6 of 7 shown · 1 empty hidden`. `/` filters on the path,
`u` narrows to the cgroups under your own `user.slice/user-<uid>.slice`
(nothing, said so, when the accounting stops at `user.slice`), and `s`
cycles %CPU, memory and processes. Everything is readable without root,
so the heading says `complete` - or `partial` on a poll where a cgroup's
files could not be read, as above.

Instead of an empty table, the view names what stands in the way: `by
service needs cgroup v2` on a v1 or hybrid host; `cpu accounting is off
everywhere · systemctl set-property <unit> CPUAccounting=yes` when the
controller is enabled on nothing below the root (with the other
controller's figures still drawn when only one is off); `cgroup tree
unreadable: …`. In a container the heading says `rows are this
container's cgroups`.

`↵` on a service opens it:

```text
╺━ PROCS · SERVICE ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 cgroup      /system.slice/example-db.service
 unit        example-db
 enabled     cpu memory pids
 %CPU        31.2% own · %CPU of all 8 cores over the last 2s
 cpu.stat    user 23.4% · system 7.8% over the last 2s, this cgroup and everything under it
 throttled   0 times for 0s over the last 2s
 memory      3.8G own · memory.current 3.8G with everything under it · memory.max 4G
 processes   12 from cgroup.procs, its own and those of cgroups under it without the controller

     PID COMMAND     USER        %CPU   MEM   RSS STATE RUNNING
     100 example-db  service-b    3.0  5.5%  900M S          9d
     101 example-db  service-b    1.0  2.4%  400M S          9d
 esc back  [r]efresh  [,] settings  [ctrl-k]cmds  [ctrl-t]heme  [q]uit
```

Its processes are rows of the process table, sorted by %CPU. A pid in
`cgroup.procs` that the last `/proc` pass did not list - started or gone
between the two reads - is named by its pid, `(not in this poll's process
list)`, or `(not readable)` under `hidepid`.

## What it shows

One row per process, every user's by default:

| column | what it is |
|---|---|
| PID | the process id |
| COMMAND | the short name: `comm` on Linux, the executable's name on macOS. A name too long for the column wraps onto the next line; it is never cut |
| USER | the owner, from `/etc/passwd` on Linux (then the system's user lookup) and from `ps` on macOS |
| %CPU | a share of **all** cores: a busy single thread on 8 cores reads 12.5. The heading says what it is measured over, every frame |
| MEM | resident memory as a share of the machine's |
| RSS | resident memory |
| STATE | `R` running, `S` sleeping, `D` waiting on disk (tinted, and footnoted under the table), `Z` zombie, `T` stopped |
| RUNNING | how long since it started |
| CGROUP | Linux only: the unit its cgroup names, or for a login session or the root the last part of the path |

**%CPU is not the same figure on both platforms**, and the heading says
which one it is:

- **Linux** measures it: the change in `utime + stime` between two polls,
  divided by the clock tick rate (from `sysconf`, never assumed to be 100),
  the window and the number of CPUs. The window is the kernel's own, from
  the `/proc/stat` totals, so the heading reads `%CPU of all 8 cores over
  the last 2s`. A process seen for the first time shows `—` until it has
  been seen twice - unless it started since the last poll, in which case
  all of its time is inside the window and it is measured from zero.
- **macOS** reads `ps`'s `%cpu`, which is the kernel's decaying average,
  not a delta over any window. The heading says `%CPU = macOS decaying
  average (ps), not the last 2s`. It is divided by the core count so the
  two platforms' columns are both shares of the whole machine.

The heading also counts zombies (they stay in the table) and the kernel
threads that are hidden (pid 2 and its children; `show_kernel_threads`
lists them). A process that exits between the listing and the read is
dropped, never drawn with a delta against nothing, and a pid reused
between two polls is a new process, not a continuation of the old one.

**hidepid.** With `/proc` mounted `hidepid=invisible` (or `2`), other
users' processes are not listed to this user at all. The counts are then
of the processes this user can see, and say so: `412 processes visible
(hidepid hides other users')`, and `184 of 412 visible shown` under a
filter. With `hidepid=noaccess` (or `1`) they are listed but their files
are closed, so they cannot be in the table; the rule counts them instead
of dropping them: `184 · 228 more running, closed to this user
(hidepid)`.

Columns leave as the pane narrows, in this order: CGROUP, RUNNING, STATE,
USER, MEM. PID, COMMAND, %CPU and RSS stay at any width. Extra width goes
to COMMAND first and CGROUP after it.

## The detail

`↵` opens the selected process:

```text
╺━ PROCS · PID 48990 ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 command     example-sync
 state       D waiting on disk, in an uninterruptible sleep
 %CPU        1.1% · %CPU of all 8 cores over the last 2s
 memory      280M resident · 0.9% of memory
 user        user-a · uid 1000 · gid 1000
 threads     4
 nice        0
 started     2026-11-07 12:31:02 · running 41m
 cpu time    27.40s
 parents     48990 example-sync ← 51020 example-agent ← 1 init
 cgroup      /user.slice/user-1000.slice/session-2.scope
 cwd         /srv/example
 disk I/O    read 1.9K/s · write 51M/s, over the last 2s

 command line
   example-sync --source /srv/example --to /mnt/example

 D: in an uninterruptible wait, almost always on disk or a network
 filesystem; the disk widget shows who is waiting
 esc back  [r]efresh  [,] settings  [ctrl-k]cmds  [ctrl-t]heme  [q]uit
```

The full command line is wrapped, never cut. The parent chain names each
parent from the same sample, and stops at one this user cannot see.

**Disk I/O** is the difference between the last two reads of
`/proc/<pid>/io` (`read_bytes`, and `write_bytes` less
`cancelled_write_bytes`), over one poll. The first read says
`measuring`. Another user's `/proc/<pid>/io` is closed to an ordinary
user, so that row reads `disk I/O needs root for another user's process`
rather than a zero; the working directory says the same. macOS does not
read cwd, threads, nice, cumulative CPU time or disk I/O in this version,
and each of those rows says so. For which processes are writing across
the whole machine, see [`disk`](../disk/README.md).

If the process exits while its detail is open, the screen says so and
keeps the last reading it had.

## Keys

| key | does |
|---|---|
| `↑` `↓` / `j` `k` | move the selection |
| `↵` | open the selected process, or by service the service's processes (a click on the selected row does the same) |
| `esc` | back from the detail, or clear the filter |
| `s` | sort by %CPU, then memory, then running (newest first) - by service %CPU, memory, then processes; the hint names the next one and the heading the one in force |
| `/` | filter by command or user (by service: by cgroup path). Letters are text while the box is open, so `s`, `u` and `q` type themselves; `↵` keeps the filter and `esc` clears it |
| `u` | every user's processes, or only yours (by service: the cgroups under your own user slice); the hint names the next state, and `yours` shows in the filter line |
| `t` | show or hide each process's threads under it (Linux only); the hint reads `[t] show threads` or `[t] hide threads` |
| `g` | switch between by process and by service (Linux only, cgroup v2); the hint reads `[g] by service` or `[g] by process`, and each view keeps its own selection |
| `r` | poll now |
| `,` | settings |
| `!` | notices |
| `q` | quit |
| wheel, `ctrl-y` / `ctrl-e`, `pgup` / `pgdn`, `home` / `end` | scroll the view - the table alone when the head is pinned, the whole body when the pane is too short for that; the selection stays where it is |

A filter in force is named in the body: `184 of 598 shown · /example ·
yours`. A filter that leaves nothing says `nothing matches · all 598
hidden by the filter`, never an empty table.

## Configuration

Section `procs` in `config.json`; `,` opens the settings screen.

| key | default | what it does |
|---|---|---|
| `refresh` | `2.0` | seconds between polls, at least 0.5. On Linux this is also the %CPU window. `-n SECONDS` overrides it |
| `show_all_users` | `true` | start on every user's processes; `u` switches for the session |
| `show_kernel_threads` | `false` | list kernel threads rather than hiding and counting them |
| `show_threads` | `false` | start with threads listed under their processes (Linux); `t` switches for the session |

There is no `rows` setting - the pane scrolls, and a cap would hide rows -
and no `cpu_window`: the %CPU window is the poll interval, and the heading
names it.

## Sources

Linux: `/proc` through `tc::procfs` - per pid, `stat`, `cgroup` and the
owner of `/proc/<pid>` each poll (all three readable for every process),
`cmdline` only for an empty `comm`; `/proc/stat`, `/proc/loadavg`,
`/proc/uptime` and `/proc/meminfo` once a poll; each thread's
`/proc/<pid>/task/<tid>/stat` only while `t` shows them. The open
process's `cmdline`, `cwd` and `io` are read on the same poller. The
previous poll's `stat` lines are the only history kept, beside the
10-minute ring of load averages. While the services view is on screen,
one walk of `/sys/fs/cgroup` a poll (`tc::cgroup`) reads each cgroup's
`cgroup.subtree_control`, `cpu.stat`, `memory.current` and
`cgroup.procs`, and the open service's `memory.max`; the previous tree is
kept for the CPU delta and dropped when the view closes.

macOS: one `ps -axww -o
pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=` a poll (the
name is the last column because Apple's `ps` cuts every other column to a
fixed width, and `ww` because without it a line is cut at 79 columns), one
`ps -ww -o args= -p <pid>` for the open process's command line,
`vm_stat`, `sysctl -n vm.swapusage` and `getloadavg` a poll, and
`hw.memsize` once. Apple's `U` (uninterruptible) state is drawn as `D`
and `I` (idle) as `S`. `ps` is required on macOS; without it the widget
shows the shared missing-tool screen.
