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

## Session association and follow-up

Rex and Herdr currently provide cwd rather than an agent session ID, so
newest-session selection is explicitly a guess. Luvus session IDs are exact
only with a pane binding. Unbound `agent sessions` entries do not select
a parent; the log reader chooses the newest cwd match and labels the guess. This matters for concurrent sessions in one cwd.

The separate agent-activity widget's history depth remains a product
choice. This change provides shared lifecycle data without introducing a
second widget or selecting a history window. Upstream requests for child
reports and exact multiplexer session bindings remain to be filed separately.
