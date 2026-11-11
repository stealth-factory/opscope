//! Live process samples with the shared reader's last recorded child states.
use opscope_core as tc;
use std::collections::HashMap;
use std::time::Instant;

pub const NAMES: &[&str] = &["claude", "codex", "opencode"];

#[derive(Clone, Debug)]
pub struct Agent {
    pub pid: u32,
    pub kind: String,
    pub command: String,
    pub cwd: Result<String, String>,
    pub work: tc::agent_work::Work,
}

impl Agent {
    /// A changed command does not silently reuse a selected pid.
    pub fn key(&self) -> (u32, String) {
        (self.pid, self.command.clone())
    }
}

#[derive(Clone, Default)]
pub struct Snapshot {
    pub agents: Vec<Agent>,
    pub sampled: Option<Instant>,
    pub err: Option<String>,
}

/// Reuse log lifecycles and shell filtering; a failed cwd/log does not erase shells.
pub fn collect(
    table: &tc::agent_work::Table,
    reader: &mut tc::agent_work::LogReader,
    cwds: &HashMap<u32, Result<String, String>>,
) -> Vec<Agent> {
    let mut agents = Vec::new();
    for (pid, kind) in table.agents(NAMES) {
        let cwd = cwds
            .get(&pid)
            .cloned()
            .unwrap_or_else(|| Err("agent cwd was not acquired".into()));
        let logs = cwd
            .as_ref()
            .map_err(|e| format!("agent cwd: {e}"))
            .and_then(|cwd| reader.read(&kind, cwd, None));
        let work = tc::agent_work::Work {
            tasks: table.background_shells(pid),
            ..Default::default()
        }
        .with_logs(logs);
        agents.push(Agent {
            pid,
            kind,
            command: table.get(pid).map(|p| p.args.clone()).unwrap_or_default(),
            cwd,
            work,
        });
    }
    agents.sort_by(|a, b| {
        (&a.kind, a.cwd.as_ref().ok(), a.pid).cmp(&(&b.kind, b.cwd.as_ref().ok(), b.pid))
    });
    agents
}

/// Most recent completed records in this selected session, not global history.
/// Known timestamps come first; id gives deterministic order for ties/unknowns.
pub fn finished(agent: &Agent, limit: usize) -> Vec<&tc::agent_work::LogTask> {
    let mut tasks: Vec<_> = agent
        .work
        .logs
        .tasks
        .iter()
        .filter(|t| t.state.finished())
        .collect();
    let time = |s: &str| {
        s.parse::<i64>().ok().or_else(|| {
            chrono::DateTime::parse_from_rfc3339(s)
                .ok()
                .map(|t| t.timestamp_millis())
        })
    };
    tasks.sort_by(|a, b| (time(&b.updated), &b.id).cmp(&(time(&a.updated), &a.id)));
    tasks.truncate(limit);
    tasks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failed_cwd_keeps_real_shell_work_and_an_explanation() {
        let table = tc::agent_work::parse_process_table(
            "20 1 claude claude\n21 20 bash bash -c cargo test\n22 21 cargo cargo test\n",
        )
        .unwrap();
        let mut reader = tc::agent_work::LogReader::default();
        let agents = collect(
            &table,
            &mut reader,
            &HashMap::from([(20, Err("permission denied".into()))]),
        );
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].work.tasks.len(), 1);
        assert!(agents[0]
            .work
            .unread
            .as_ref()
            .unwrap()
            .contains("permission denied"));
    }

    #[test]
    fn finished_rows_are_bounded_without_hiding_active_work() {
        use tc::agent_work::{Activity, LogTask};
        let mut a = Agent {
            pid: 20,
            kind: "claude".into(),
            command: "claude".into(),
            cwd: Ok("/srv/repo".into()),
            work: Default::default(),
        };
        for (id, state, updated) in [
            ("old", Activity::Done, "2026-11-10T01:00:00Z"),
            ("new", Activity::Error, "2026-11-11T01:00:00Z"),
            ("busy", Activity::Working, "2026-11-11T02:00:00Z"),
        ] {
            a.work.logs.tasks.push(LogTask {
                id: id.into(),
                label: id.into(),
                state,
                updated: updated.into(),
            });
        }
        assert_eq!(finished(&a, 1)[0].id, "new");
        assert!(finished(&a, 0).is_empty());
        assert!(a.work.rows(80).iter().any(|(_, r)| r.contains("busy")));
        assert!(!a.work.rows(80).iter().any(|(_, r)| r.contains("old")));
    }
}
