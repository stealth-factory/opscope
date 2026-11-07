# `procs`

[← all widgets](../../../../docs/README.md)

What this machine is running: every process, its CPU, memory and state.

Linux example, 100 columns:

```text
╺━ PROCS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 sampled 1s ago · every 2s · %CPU of all 8 cores over the last 2s · 598 processes · sorted by %CPU
 3 zombies · 112 kernel threads hidden

     PID COMMAND        USER        %CPU   MEM   RSS STATE RUNNING CGROUP
▸  48213 example-worker user-a      12.4  4.1%  1.3G R       2d    session-2.scope
   51020 example-agent  user-a       2.8  2.0%  640M S       3h    session-2.scope
   48990 example-sync   user-a       1.1  0.9%  280M D       41m   session-2.scope
    1204 example-db     service-b    0.6 12.3%  3.9G S       9d    example-db
     733 example-vpn    root         0.4  0.1%   45M S       3d    example-vpn
   51077 (a) b (c)      user-a         —  0.0%     0 Z       12s   session-2.scope

 D = waiting on disk, 1 shown now
 ↑↓ select  [↵] detail  [s] sort by memory  [/] filter  [u] only yours  [r]efresh  [,] settings
 [ctrl-k]cmds  [ctrl-t]heme  [q]uit
```

## What it shows

One row per process, every user's by default:

| column | what it is |
|---|---|
| PID | the process id |
| COMMAND | the short name: `comm` on Linux, the executable's name on macOS. A name too long for the column wraps onto the next line; it is never cut |
| USER | the owner, from `/etc/passwd` on Linux (then the system's user lookup) and from `ps` on macOS |
| %CPU | a share of **all** cores: a busy single thread on 8 cores reads 12.5. The header says what it is measured over, every frame |
| MEM | resident memory as a share of the machine's |
| RSS | resident memory |
| STATE | `R` running, `S` sleeping, `D` waiting on disk (tinted, and footnoted under the table), `Z` zombie, `T` stopped |
| RUNNING | how long since it started |
| CGROUP | Linux only: the unit its cgroup names, or for a login session or the root the last part of the path |

**%CPU is not the same figure on both platforms**, and the header says
which one it is:

- **Linux** measures it: the change in `utime + stime` between two polls,
  divided by the clock tick rate (from `sysconf`, never assumed to be 100),
  the window and the number of CPUs. The window is the kernel's own, from
  the `/proc/stat` totals, so the header reads `%CPU of all 8 cores over
  the last 2s`. A process seen for the first time shows `—` until it has
  been seen twice - unless it started since the last poll, in which case
  all of its time is inside the window and it is measured from zero.
- **macOS** reads `ps`'s `%cpu`, which is the kernel's decaying average,
  not a delta over any window. The header says `%CPU = macOS decaying
  average (ps), not the last 2s`. It is divided by the core count so the
  two platforms' columns are both shares of the whole machine.

The header also counts zombies (they stay in the table) and the kernel
threads that are hidden (pid 2 and its children; `show_kernel_threads`
lists them). A process that exits between the listing and the read is
dropped, never drawn with a delta against nothing, and a pid reused
between two polls is a new process, not a continuation of the old one.

**hidepid.** With `/proc` mounted `hidepid=invisible` (or `2`), other
users' processes are not listed to this user at all. The counts are then
of the processes this user can see, and say so: `412 processes visible
(hidepid hides other users')`, and `184 of 412 visible shown` under a
filter.

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
| `↵` | open the selected process (a click on the selected row does the same) |
| `esc` | back from the detail, or clear the filter |
| `s` | sort by %CPU, then memory, then running (newest first); the hint names the next one and the header the one in force |
| `/` | filter by command or user. Letters are text while the box is open, so `s`, `u` and `q` type themselves; `↵` keeps the filter and `esc` clears it |
| `u` | every user's processes, or only yours; the hint names the next state, and `yours` shows in the filter line |
| `r` | poll now |
| `,` | settings |
| `!` | notices |
| `q` | quit |
| wheel, `ctrl-y` / `ctrl-e`, `pgup` / `pgdn`, `home` / `end` | scroll the view; the selection stays where it is |

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

There is no `rows` setting - the pane scrolls, and a cap would hide rows -
and no `cpu_window`: the %CPU window is the poll interval, and the header
names it.

## Sources

Linux: `/proc` through `tc::procfs` - per pid, `stat`, `cgroup` and the
owner of `/proc/<pid>` each poll (all three readable for every process),
`cmdline` only for an empty `comm`; `/proc/stat`, `/proc/uptime` and
`/proc/meminfo` once a poll. The open process's `cmdline`, `cwd` and `io`
are read on the same poller. The previous poll's `stat` is the only
history kept.

macOS: one `ps -axo
pid=,ppid=,uid=,user=,%cpu=,%mem=,rss=,state=,etime=,comm=,args=` a poll,
and `hw.memsize` once. Apple's `U` (uninterruptible) state is drawn as `D`
and `I` (idle) as `S`. `ps` is required on macOS; without it the widget
shows the shared missing-tool screen.
