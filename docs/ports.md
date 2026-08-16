# `ports.py`

What is listening on this machine, what started it, and who can reach it.

```
╺━ DEV SERVERS ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━╸
 19 listening · 8 yours · 1 reachable off-box   every 4s

  PORT  BIND    WHAT              PROJECT                  UP    EXPOSED
  3000  all     Next.js 16.3.1    fix-dependabot-alerts ✗  22h   -
  3001  all     Next.js 16.2.11   piaf-web                 8h    -
  3002  all     Next.js 16.3.0    my-project               1h    -
  4100  --      nothing listening                          --    tailnet
 25001  local   Next.js           my-project               1h    -
 42043  local   agy               wiiiimm-codes            9h    -
```

## What identifies a dev server

Not the pid, and not the port. The **project directory** it was started from,
which is what a person actually calls it — so that column is the one given
whatever width the pane has left, and the fixed ones make way for it.

The name comes from `package.json` in the process's `cwd` where there is one,
and from the directory's own name otherwise. `piaf-web` above is the former;
`my-project` is a `package.json` that was never renamed, which is exactly as
true and rather more revealing.

## What kind of thing it is

Read from the process itself, in two steps, because a dev server launched
through a package manager is several layers of wrapper deep:

- **the process title**, where there is one. Next.js rewrites its own to
  `next-server (v16.3.0)`, which hands over the framework *and* the version;
- **the argv path**, when the title is just `node`. A path containing
  `node_modules/next/dist` is a Next.js server whatever the binary is called.

Vite, Nuxt, Astro, Remix, webpack, Django, Rails, uvicorn, Postgres, Redis and
a dozen others are recognised the same way. Anything unmatched shows the
basename of its command rather than a guess.

## The two columns that answer "who can reach this"

**BIND** is the class of the bound address, not the address: `all` for
`0.0.0.0`, `local` for loopback, `tailnet` for an address in Tailscale's
`100.64.0.0/10` or `fd7a:115c:a1e0::/48`. The literal address is both too wide
for a column — a tailnet IPv6 address is 24 characters — and rarely the answer
to the question being asked.

**EXPOSED** is what Tailscale is serving, from `tailscale serve status`:
`tailnet` where it is reachable by your own devices, `public` where a funnel
puts it on the open internet. Funnel is the one worth colouring red; it is
also limited to three ports (443, 8443, 10000), so most machines will never
show it.

Cloudflare Tunnel is **not** read. It would answer the same question for
arbitrary ports without a tailnet, and is a line of parsing away if it is ever
adopted here — but a column that silently covers one mechanism and not another
is worse than one that says which it knows.

## The two rows that only this widget can show you

**A server on a directory that no longer exists.** `fix-dependabot-alerts ✗`
above is a dev server still running, still holding port 3000, on a worktree
that has been deleted. `/proc` marks the cwd `(deleted)`, and the name is kept
rather than blanked, because a stale server holding a port you want is
precisely the thing worth finding.

**A port that is served but not listening.** Port 4100 has a Tailscale URL
pointing at it and nothing behind it — the URL exists, answers 502, and
nothing in `lsof` explains why. It gets its own row rather than being left out
for lacking a socket.

## What it cannot see

Sockets owned by another user, which on a normal machine means everything root
runs — `sshd`, the resolver, Tailscale itself. `/proc/<pid>/fd` is unreadable
for those, so the socket cannot be tied to a process without root.

Those rows say `(not ours)`, or name the service **with a question mark** —
`SSH?`, `DNS?` — where the port number is a well-known convention. The question
mark is the point: it is a guess from a port number, not something read from
the process, and the widget says which it is. They are hidden behind `o` by
default, since they are never the answer to "which port is my dev server on".

## Keys

| Key | Action |
|---|---|
| `↑` `↓` | select a row |
| `o` | show or hide system ports |
| `r` | rescan now |
| `q` | quit |

## Cost

Nothing measurable. `/proc/net/tcp` and a walk of `/proc/*/fd` every four
seconds, plus one `tailscale serve status` — no network, no root, no
dependency beyond Tailscale for the exposure column, which is simply blank
without it.

## Configuration

```json
"ports": {
  "system_ports": [22, 53, 123, 323, 631, 5353],
  "refresh": 4
}
```

`system_ports` is what `o` hides. Add anything that is part of the machine
rather than something you started.
