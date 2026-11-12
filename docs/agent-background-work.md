# Background work from agent sessions (OPS-271)

`tc::agent_work::LogReader` is shared by mux-activity and future consumers.
It returns session identity, whether the pane association was guessed, and
child IDs, labels, last recorded states and update times. It keeps finished
states; mux draws only unfinished children. No prompts, model reasoning or
tool output are retained in the cache or drawn as labels.

The source order is reported OSC 7501 children, session logs for subagents,
and processes for shells. A report ID is not a process PID; process rows
stay unless a reliable process association is available. Match complete log IDs (including the final component
of a hierarchical report ID), not labels: two Explore agents can have the
same label. An unidentifiable report cannot safely suppress another source.
A record from a log is the last recorded lifecycle, not a liveness heartbeat.

## Claude Code probe

Checked locally on 2026-11-11 with Claude Code 2.1.322 in a dedicated Rex
session. User hooks were disabled using `--settings '{"disableAllHooks":true}'`.
No repository files or existing panes were changed by the probe.

1. Launch an Explore Agent with `run_in_background: true` and a short Bash
   sleep inside it; launch a background Bash sleep from the parent too.
2. While they run, read Rex's
   `com.superlogical.terminal.program_status` and the parent transcript.
3. Inspect lifecycle fields from the child transcript under
   `<session>/subagents/agent-<id>.jsonl`, then the final parent notice.

The parent stores `toolUseResult.status = async_launched`, `agentId`,
`outputFile` and `description`. The Agent tool call supplies `subagent_type`.
The tool result links back through `tool_use_id`. Background Bash instead
returns `backgroundTaskId`; it is not a subagent launch.

The child transcript is present while work runs. An assistant `end_turn`
can still leave its own background shell running. The parent can then receive
`<status>completed</status>` with a note beginning “This agent stopped with
background work of its own still running”. That is an interim stop. After
the shell finishes, the child resumes, writes a final turn, and the parent
receives another notification for the same task ID. Completion handling
must distinguish these two stops. System-origin notification markers also
keep a user quoting an example notification from clearing real work.

Rex returned only the root `claude-code` status record during both probes,
including the hook-disabled one. No OSC 7501 child records were observed
for the subagent or background shell. This is an observation of this
installed version, not a claim about every Claude release. The earlier
[Atelier report](https://github.com/alvag/atelier/issues/185) remains
unconfirmed here. Anthropic's [hooks reference](https://code.claude.com/docs/en/hooks)
documents the structured launch result, but the reader needs no hooks.

## Codex

The installed rollout headers serialize ThreadSpawn as
`source.subagent.thread_spawn`, with `parent_thread_id` and `depth`.
The reader follows descendants across dated session folders, excludes
internal guardian/compaction sessions, and follows the latest turn's
`task_started`, `task_complete` and `turn_aborted` events. These shapes
are also defined in the upstream
[protocol source](https://github.com/openai/codex/blob/main/codex-rs/protocol/src/protocol.rs).

The completed rollout-header index is shared across all panes for a short
interval. Only selected child bodies are parsed, incrementally. Finished files are cached; partial final
JSONL records, including split UTF-8 characters, wait for the writer's
newline. Unchanged malformed records keep a cached failure instead of
replaying the file each poll. An unreadable relevant record is an error on
the pane, not an empty result. Malformed unrelated headers produce an
incomplete-inventory warning while known child tasks remain visible.

## OpenCode

The installed V2 store has `session_v2.parent_id`, `time_busy`, `time_idle`
and `idle_outcome`. The reader opens SQLite read-only, walks descendants,
and preserves failed/interrupted outcomes. SQLite is bundled into core.
The historical `time_suspended` column is an execution claim, not proof
that the agent is paused; it must not hide busy work. See upstream
[table definitions](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/session/sql.ts),
[execution claims](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/session/store.ts)
and [lifecycle projection](https://github.com/anomalyco/opencode/blob/dev/packages/core/src/session/projector.ts).

V1 stores do not persist the same liveness information. This reader reports
that limitation and continues to show process-backed shells. It does not
infer that a child session is running merely because it exists.

## Claude background agents and pid files (OPS-276)

`claude --bg` agents run on Claude's daemon, on a private terminal outside
every multiplexer pane. Rex and Luvus never hear from them, and Herdr only
through its hook, credited to the pane that started the daemon.
`tc::agent_work::read_jobs` reads what the `claude agents` dashboard reads
instead, with no hooks, and mux-activity draws each job as an agent row in
a `background agents · via jobs` group on every board.

Observed on 2026-11-12 on Linux with Claude Code 2.1.322 installed. The
`state.json` files present carried `cliVersion` 2.1.312 to 2.1.319 (one had
none); the pid files carried `version` 2.1.316 to 2.1.322. Both formats are
undocumented and internal, so the parsers are pinned by fixtures under
`core/src/agent_work/fixtures/jobs/`, every field is optional except the
ones named below, and an unrecognised `state` is an error on a row rather
than a guess.

`<config dir>/jobs/<short id>/state.json`:

* `state` is `working`, `blocked`, `done` or `stopped`, as the agent
  reports it. Required.
* `name`, `detail` (one line), `needs` (when blocked), `cwd`, `cliVersion`,
  `updatedAt` (RFC 3339).
* `sessionId` and `resumeSessionId`. A resumed job runs under the second, and
  its pid file names that one; all of them are kept for matching.
* `inFlight`: `{tasks, queued, kinds}`. Absent on some stopped jobs; absent
  is not zero.
* `fan`: `{id, kind, label, startedAt, doneAt}`, times in epoch ms.
  Subagents (`kind: agent`) get a `doneAt`. Shells never did in any file
  read here, including shells whose processes were long gone, so a shell
  with no `doneAt` is not evidence that it runs. Rows draw only unfinished
  subagents; live shells come from the process table under the job's pid.
* `children` lists things it produced, such as pull requests, not subagents,
  and is not read. `jobs/pins.json` lists pinned jobs and is not read.
* A job folder holding only `tmp/` has never run and is not a job.

`timeline.jsonl` beside it holds `{at, state, detail}` records. Only its
last 64 KiB is read: the start of the run of the current state it ends with
is when that state began. A run that reaches back past the cut is a lower
bound, drawn with `≥`; with no timeline, `updatedAt` is the lower bound.

`<config dir>/sessions/<pid>.json` is kept by every running Claude process,
interactive or background: `pid`, `sessionId`, `cwd`, `status`
(`busy`/`idle`/`shell`), `kind` (`interactive`/`bg`), `jobId` for a
background one, and `procStart`. Files outlive their processes, so a file
counts only while its pid is in the process table, and on Linux only while
`procStart` equals the pid's `starttime` in `/proc/<pid>/stat`. A job is
running when such a file names its `jobId`.

The same file binds a pane's agent to its session: `LogReader::read_agent`
looks up the agent pid's file before falling back to the cwd guess, and the
row loses `session guessed`. The config dir is the agent's own:
`CLAUDE_CONFIG_DIR` (else `$HOME/.claude`) from `/proc/<pid>/environ`, then
any config dir already seen. A Mac has no readable environ, so there the
pid file is found by looking in each known config dir. Every config dir
found this way is also searched for background agents.

Under Herdr, a job whose session id equals the `agent_session` id Herdr's
hook reported is not drawn a second time: its subagents join that agent's
row. Herdr did not carry a session id for Claude agents on the version
checked, so this applies only once it does.

## Session association and follow-up

Without a pid file (Codex, OpenCode, or a Claude that writes none), Rex and
Herdr provide cwd rather than an agent session ID, so newest-session
selection is explicitly a guess. Luvus session IDs are exact
only with a pane binding. Unbound `agent sessions` entries do not select
a parent; the log reader chooses the newest cwd match and labels the guess. This matters for concurrent sessions in one cwd.

The separate agent-activity widget's history depth remains a product
choice. This change provides shared lifecycle data without introducing a
second widget or selecting a history window. Upstream requests for child
reports and exact multiplexer session bindings remain to be filed separately.
