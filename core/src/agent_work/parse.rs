// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU Affero General Public License for more details.
//
// You should have received a copy of the GNU Affero General Public License
// along with this program.  If not, see <https://www.gnu.org/licenses/>.

//! Pure session-log parsers. No target gates: Linux and macOS test the same bytes.
use super::logs::{Activity, LogTask};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap, HashSet};

#[derive(Clone, Debug, Default)]
pub(super) struct Transcript {
    pub id: String,
    pub cwd: String,
    pub parent: Option<String>,
    pub label: String,
    pub state: Activity,
    pub updated: String,
    pub agents: BTreeMap<String, LogTask>,
    calls: HashMap<String, String>,
    shells: HashSet<String>,
}

fn text<'a>(v: &'a Value, key: &str) -> &'a str {
    v[key].as_str().unwrap_or("")
}
fn tag<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    s.split_once(&format!("<{name}>"))?
        .1
        .split_once(&format!("</{name}>"))
        .map(|p| p.0)
}

pub(super) fn parse_claude_record(t: &mut Transcript, line: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid Claude record: {e}"))?;
    let stamp = text(&v, "timestamp");
    let kind = text(&v, "type");
    // API request blobs also carry a message; those are copies, not turns.
    if !matches!(kind, "assistant" | "user") {
        return Ok(());
    }
    t.updated = stamp.into();
    if !text(&v, "sessionId").is_empty() {
        t.id = text(&v, "sessionId").into();
    }
    if !text(&v, "cwd").is_empty() {
        t.cwd = text(&v, "cwd").into();
    }
    if kind == "user" {
        t.state = Activity::Working;
    }
    if let Some(content) = v["message"]["content"].as_array() {
        for part in content {
            if text(part, "type") == "tool_use" {
                if matches!(text(part, "name"), "Agent" | "Task") {
                    let label = text(&part["input"], "subagent_type");
                    t.calls.insert(
                        text(part, "id").into(),
                        if label.is_empty() {
                            "subagent".into()
                        } else {
                            label.into()
                        },
                    );
                }
                t.state = Activity::Working;
            }
        }
    }
    let r = &v["toolUseResult"];
    if text(r, "status") == "async_launched" {
        let id = text(r, "agentId");
        if id.is_empty() {
            return Err("Claude launch has no agentId".into());
        }
        let call = v["message"]["content"]
            .as_array()
            .into_iter()
            .flatten()
            .find_map(|p| p["tool_use_id"].as_str())
            .unwrap_or("");
        t.agents.insert(
            id.into(),
            LogTask {
                id: id.into(),
                label: t
                    .calls
                    .get(call)
                    .cloned()
                    .unwrap_or_else(|| "subagent".into()),
                state: Activity::Working,
                updated: stamp.into(),
            },
        );
    }
    if let Some(id) = r["backgroundTaskId"].as_str() {
        t.shells.insert(id.into());
    }
    if let Some(s) = v["message"]["content"].as_str().filter(|s| {
        s.contains("<task-notification>")
            && (text(&v["origin"], "kind") == "task-notification"
                || v["isMeta"].as_bool() == Some(true))
    }) {
        if let Some(id) = tag(s, "task-id") {
            let state = match tag(s, "status") {
                Some("completed")
                    if tag(s, "note").is_some_and(|n| {
                        n.starts_with(
                            "This agent stopped with background work of its own still running",
                        )
                    }) =>
                {
                    Activity::Waiting
                }
                Some("completed") => Activity::Done,
                Some("failed" | "killed" | "stopped") => Activity::Error,
                _ => Activity::Unknown,
            };
            if let Some(task) = t.agents.get_mut(id) {
                task.state = state;
                task.updated = stamp.into();
            }
            if state.finished() {
                t.shells.remove(id);
            }
        }
    }
    if kind == "assistant" && text(&v["message"], "stop_reason") == "end_turn" {
        t.state = if !t.shells.is_empty() || t.agents.values().any(|a| !a.state.finished()) {
            Activity::Waiting
        } else {
            Activity::Done
        };
    }
    Ok(())
}

pub(super) fn parse_codex_record(t: &mut Transcript, line: &str) -> Result<(), String> {
    let v: Value = serde_json::from_str(line).map_err(|e| format!("invalid Codex record: {e}"))?;
    let p = &v["payload"];
    match text(&v, "type") {
        "session_meta" => {
            t.id = p["id"]
                .as_str()
                .or_else(|| p["session_id"].as_str())
                .unwrap_or("")
                .into();
            t.cwd = text(p, "cwd").into();
            let spawn = &p["source"]["subagent"]["thread_spawn"];
            t.parent = spawn["parent_thread_id"].as_str().map(str::to_owned);
            t.label = spawn["agent_role"]
                .as_str()
                .or_else(|| spawn["agent_type"].as_str())
                .or_else(|| spawn["agent_nickname"].as_str())
                .unwrap_or("subagent")
                .into();
            // Other internal sessions (guardian, compact, review) are not root panes.
            if p["source"].is_object() && t.parent.is_none() {
                t.parent = Some(String::new());
            }
        }
        "event_msg" => {
            let state = match text(p, "type") {
                "task_started" | "turn_started" => Some(Activity::Working),
                "task_complete" | "turn_complete" => Some(Activity::Done),
                "turn_aborted" | "shutdown_complete" => Some(Activity::Error),
                _ => None,
            };
            if let Some(state) = state {
                t.state = state;
                t.updated = text(&v, "timestamp").into();
            }
        }
        _ => (),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn claude_launch_interim_stop_resume_and_final_stop() {
        let mut t = Transcript::default();
        for l in [
            r#"{"type":"assistant","message":{"content":[{"type":"tool_use","id":"call","name":"Agent","input":{"subagent_type":"Explore"}}]}}"#,
            r#"{"type":"user","message":{"content":[{"type":"tool_result","tool_use_id":"call"}]},"toolUseResult":{"status":"async_launched","agentId":"child"}}"#,
        ] {
            parse_claude_record(&mut t, l).unwrap();
        }
        assert_eq!(t.agents["child"].label, "Explore");
        let notification = |note: &str| {
            serde_json::json!({"type":"user","isMeta":true,"message":{"content":format!("<task-notification><task-id>child</task-id><status>completed</status><note>{note}</note></task-notification>")}}).to_string()
        };
        parse_claude_record(
            &mut t,
            &notification(
                "This agent stopped with background work of its own still running. It may resume.",
            ),
        )
        .unwrap();
        assert_eq!(t.agents["child"].state, Activity::Waiting);
        parse_claude_record(&mut t,&notification("A task-notification fires each time this agent stops with no live background children of its own.")).unwrap();
        assert_eq!(t.agents["child"].state, Activity::Done);
    }
    #[test]
    fn claude_end_turn_is_not_finished_with_a_live_background_shell() {
        let mut t = Transcript::default();
        parse_claude_record(
            &mut t,
            r#"{"type":"user","toolUseResult":{"backgroundTaskId":"shell"}}"#,
        )
        .unwrap();
        let end = r#"{"type":"assistant","message":{"stop_reason":"end_turn"}}"#;
        parse_claude_record(&mut t, end).unwrap();
        assert_eq!(t.state, Activity::Waiting);
        parse_claude_record(&mut t,r#"{"type":"user","isMeta":true,"message":{"content":"prefix\n<task-notification><task-id>shell</task-id><status>completed</status></task-notification>"}}"#).unwrap();
        parse_claude_record(&mut t, end).unwrap();
        assert_eq!(t.state, Activity::Done);
    }
    #[test]
    fn a_typed_notification_example_does_not_finish_real_work() {
        let mut t = Transcript::default();
        parse_claude_record(
            &mut t,
            r#"{"type":"user","toolUseResult":{"status":"async_launched","agentId":"child"}}"#,
        )
        .unwrap();
        parse_claude_record(&mut t, r#"{"type":"user","origin":{"kind":"human"},"message":{"content":"<task-notification><task-id>child</task-id><status>completed</status></task-notification>"}}"#).unwrap();
        assert_eq!(t.agents["child"].state, Activity::Working);
    }

    #[test]
    fn codex_uses_serialized_source_and_last_turn_lifecycle() {
        let mut t = Transcript::default();
        parse_codex_record(&mut t,r#"{"type":"session_meta","payload":{"id":"child","source":{"subagent":{"thread_spawn":{"parent_thread_id":"parent","depth":1,"agent_role":"explorer"}}}}}"#).unwrap();
        assert_eq!(t.parent.as_deref(), Some("parent"));
        assert_eq!(t.label, "explorer");
        for (kind, state) in [
            ("task_started", Activity::Working),
            ("task_complete", Activity::Done),
            ("task_started", Activity::Working),
            ("turn_aborted", Activity::Error),
        ] {
            parse_codex_record(
                &mut t,
                &serde_json::json!({"type":"event_msg","payload":{"type":kind}}).to_string(),
            )
            .unwrap();
            assert_eq!(t.state, state);
        }
    }
}
