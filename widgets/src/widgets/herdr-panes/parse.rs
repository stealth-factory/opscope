use serde_json::{json, Value};
use std::path::PathBuf;

const MAX_FRAME: usize = 64 * 1024;
pub const SUBSCRIPTION_ID: &str = "opscope-herdr-panes-events";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerSocket {
    pub path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StreamEvent {
    Relevant,
    Ignore,
    Reconcile(String),
}

pub fn parse_result(text: &str) -> Result<Value, String> {
    let parsed: Value =
        serde_json::from_str(text).map_err(|e| format!("unreadable answer: {e}"))?;
    if let Some(said) = parsed.get("error") {
        let message = said["message"].as_str().unwrap_or("").trim();
        return Err(if message.is_empty() {
            format!("herdr said {said}")
        } else {
            message.to_string()
        });
    }
    match parsed.get("result") {
        Some(Value::Null) | None => Err("herdr answered with no result".into()),
        Some(value) => Ok(value.clone()),
    }
}

pub fn parse_status(text: &str) -> Result<ServerSocket, String> {
    let status: Value =
        serde_json::from_str(text).map_err(|e| format!("unreadable server status: {e}"))?;
    if !status.is_object() || status.get("result").is_some() || status.get("error").is_some() {
        return Err("server status is not a plain status object".into());
    }
    if status["status"].as_str() != Some("running") || status["running"].as_bool() != Some(true) {
        return Err("Herdr server is not running".into());
    }
    if status["compatible"].as_bool() != Some(true)
        || status["endpoint_compatible"].as_bool() == Some(false)
    {
        return Err("Herdr server protocol is incompatible".into());
    }
    let socket = status["socket"]
        .as_str()
        .filter(|path| !path.is_empty() && !path.chars().any(char::is_control))
        .ok_or_else(|| "Herdr status has no valid local socket".to_string())?;
    let path = PathBuf::from(socket);
    if !path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, std::path::Component::ParentDir))
    {
        return Err("Herdr status did not resolve to a local Unix socket".into());
    }
    Ok(ServerSocket { path })
}

pub fn parse_pane_ids(text: &str) -> Result<Vec<String>, String> {
    let result = parse_result(text)?;
    let panes = result["panes"]
        .as_array()
        .ok_or_else(|| "herdr answered with no pane inventory".to_string())?;
    let mut ids = Vec::with_capacity(panes.len());
    for pane in panes {
        let id = pane["pane_id"]
            .as_str()
            .filter(|id| !id.is_empty())
            .ok_or_else(|| "herdr pane inventory has an invalid pane id".to_string())?;
        ids.push(id.to_string());
    }
    ids.sort();
    ids.dedup();
    Ok(ids)
}

pub fn subscription_request(pane_ids: &[String]) -> Result<String, String> {
    let mut subscriptions: Vec<Value> = [
        "workspace.created",
        "workspace.closed",
        "workspace.renamed",
        "workspace.metadata_updated",
        "pane.created",
        "pane.closed",
        "pane.updated",
        "pane.moved",
        "pane.exited",
        "pane.agent_detected",
    ]
    .into_iter()
    .map(|event| json!({"type": event}))
    .collect();
    let mut panes = pane_ids.to_vec();
    panes.sort();
    panes.dedup();
    subscriptions.extend(
        panes
            .into_iter()
            .map(|pane_id| json!({"type": "pane.agent_status_changed", "pane_id": pane_id})),
    );
    let request = json!({
        "id": SUBSCRIPTION_ID,
        "method": "events.subscribe",
        "params": {"subscriptions": subscriptions}
    })
    .to_string()
        + "\n";
    if request.len() > MAX_FRAME {
        return Err("Herdr pane inventory is too large for one bounded subscription".into());
    }
    Ok(request)
}

pub fn parse_subscription_ack(text: &str) -> Result<(), String> {
    let ack: Value = serde_json::from_str(text)
        .map_err(|e| format!("unreadable subscription acknowledgement: {e}"))?;
    if let Some(error) = ack.get("error") {
        let code = error["code"].as_str().unwrap_or("");
        return Err(if code == "events_lost" {
            "Herdr event history was lost; reconciling from a fresh snapshot".into()
        } else {
            let message = error["message"].as_str().unwrap_or("").trim();
            if message.is_empty() {
                format!("Herdr subscription failed: {error}")
            } else {
                message.to_string()
            }
        });
    }
    if ack["id"].as_str() != Some(SUBSCRIPTION_ID)
        || ack["result"]["type"].as_str() != Some("subscription_started")
    {
        return Err("Herdr returned an invalid subscription acknowledgement".into());
    }
    Ok(())
}

pub fn parse_stream_event(text: &str) -> Result<StreamEvent, String> {
    let envelope: Value =
        serde_json::from_str(text).map_err(|e| format!("malformed Herdr event frame: {e}"))?;
    if let Some(error) = envelope.get("error") {
        let code = error["code"].as_str().unwrap_or("");
        if code == "events_lost" || code.contains("resync_required") {
            return Ok(StreamEvent::Reconcile(
                "Herdr event history was lost; reconciling from a fresh snapshot".into(),
            ));
        }
        let message = error["message"].as_str().unwrap_or("").trim();
        return Err(if message.is_empty() {
            format!("Herdr event stream error: {error}")
        } else {
            message.to_string()
        });
    }
    let event = envelope["event"]
        .as_str()
        .ok_or_else(|| "Herdr event frame has no event name".to_string())?;
    if event == "events_lost" || event == "events.resync_required" {
        return Ok(StreamEvent::Reconcile(
            "Herdr event history was lost; reconciling from a fresh snapshot".into(),
        ));
    }
    if !envelope["data"].is_object() {
        return Err("Herdr event frame has no data object".into());
    }
    if relevant_event(event) {
        Ok(StreamEvent::Relevant)
    } else {
        Ok(StreamEvent::Ignore)
    }
}

fn relevant_event(event: &str) -> bool {
    [
        "workspace.created",
        "workspace_created",
        "workspace.closed",
        "workspace_closed",
        "workspace.renamed",
        "workspace_renamed",
        "workspace.metadata_updated",
        "workspace_metadata_updated",
        "pane.created",
        "pane_created",
        "pane.closed",
        "pane_closed",
        "pane.updated",
        "pane_updated",
        "pane.moved",
        "pane_moved",
        "pane.exited",
        "pane_exited",
        "pane.agent_detected",
        "pane_agent_detected",
        "pane.agent_status_changed",
        "pane_agent_status_changed",
    ]
    .contains(&event)
}

#[derive(Default)]
pub struct NdjsonBuffer {
    pending: Vec<u8>,
}

impl NdjsonBuffer {
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<String>, String> {
        self.pending.extend_from_slice(chunk);
        if self.pending.len() > MAX_FRAME && !self.pending.contains(&b'\n') {
            return Err("Herdr event frame exceeded the size limit".into());
        }
        let mut frames = Vec::new();
        while let Some(end) = self.pending.iter().position(|byte| *byte == b'\n') {
            if end > MAX_FRAME {
                return Err("Herdr event frame exceeded the size limit".into());
            }
            let mut line: Vec<u8> = self.pending.drain(..=end).collect();
            line.pop();
            if line.last() == Some(&b'\r') {
                line.pop();
            }
            if line.is_empty() {
                continue;
            }
            frames.push(
                String::from_utf8(line)
                    .map_err(|e| format!("Herdr event frame is not UTF-8: {e}"))?,
            );
        }
        if self.pending.len() > MAX_FRAME {
            return Err("Herdr event frame exceeded the size limit".into());
        }
        Ok(frames)
    }

    pub fn parse_eof(&self) -> Result<(), String> {
        if self.pending.is_empty() {
            Err("Herdr event socket closed".into())
        } else {
            Err("Herdr event socket closed with an incomplete frame".into())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_plain_compatible_local_status_and_rejects_remote_or_invalid_targets() {
        assert!(parse_status(
            r#"{"status":"running","running":true,"compatible":true,"socket":"/local/endpoint"}"#
        )
        .is_ok());
        for status in [
            r#"{"result":{"status":"running","running":true,"compatible":true,"socket":"/local/endpoint"}}"#,
            r#"{"status":"not_running","running":false,"compatible":true,"socket":"/local/endpoint"}"#,
            r#"{"status":"running","running":true,"compatible":false,"socket":"/local/endpoint"}"#,
            r#"{"status":"running","running":true,"compatible":true,"socket":"remote-endpoint"}"#,
            r#"{"status":"running","running":true,"compatible":true,"socket":"/local/../remote"}"#,
            r#"{"status":"running","running":true,"compatible":true}"#,
        ] {
            assert!(parse_status(status).is_err(), "{status}");
        }
    }

    #[test]
    fn acknowledgement_is_readiness_not_an_event() {
        assert_eq!(
            parse_subscription_ack(
                r#"{"id":"opscope-herdr-panes-events","result":{"type":"subscription_started"}}"#
            ),
            Ok(())
        );
        assert!(parse_subscription_ack(
            r#"{"id":"opscope-herdr-panes-events","error":{"code":"events_lost","message":"lost"}}"#
        )
        .unwrap_err()
        .contains("fresh snapshot"));
    }

    #[test]
    fn parses_events_lost_errors_and_relevant_event_name_variants() {
        assert_eq!(
            parse_stream_event(r#"{"id":"event","error":{"code":"events_lost","message":"lost"}}"#)
                .unwrap(),
            StreamEvent::Reconcile(
                "Herdr event history was lost; reconciling from a fresh snapshot".into()
            )
        );
        assert_eq!(
            parse_stream_event(r#"{"event":"pane_agent_status_changed","data":{"pane_id":"p1"}}"#)
                .unwrap(),
            StreamEvent::Relevant
        );
        assert_eq!(
            parse_stream_event(r#"{"event":"workspace.renamed","data":{"workspace_id":"w1"}}"#)
                .unwrap(),
            StreamEvent::Relevant
        );
        assert!(parse_stream_event(r#"{"error":{"code":"bad","message":"broken"}}"#).is_err());
    }

    #[test]
    fn pane_filters_cover_inventory_without_resetting_on_unchanged_ids() {
        let ids = vec!["p2".to_string(), "p1".to_string(), "p1".to_string()];
        let request: Value =
            serde_json::from_str(subscription_request(&ids).unwrap().trim()).unwrap();
        let subscriptions = request["params"]["subscriptions"].as_array().unwrap();
        let topic_names: Vec<&str> = subscriptions
            .iter()
            .filter_map(|subscription| subscription["type"].as_str())
            .collect();
        for topic in [
            "workspace.created",
            "workspace.closed",
            "workspace.renamed",
            "workspace.metadata_updated",
            "pane.created",
            "pane.closed",
            "pane.updated",
            "pane.moved",
            "pane.exited",
            "pane.agent_detected",
        ] {
            assert!(topic_names.contains(&topic), "missing {topic}");
        }
        let status: Vec<&Value> = subscriptions
            .iter()
            .filter(|subscription| subscription["type"] == "pane.agent_status_changed")
            .collect();
        assert_eq!(status.len(), 2);
        assert_eq!(status[0]["pane_id"], "p1");
        assert_eq!(status[1]["pane_id"], "p2");
        assert!(status
            .iter()
            .all(|subscription| subscription.get("agent_status").is_none()));
        assert_eq!(request["method"], "events.subscribe");
        let pane_ids = parse_pane_ids(
            r#"{"result":{"panes":[{"pane_id":"p2","agent":null},{"pane_id":"p1","agent":"agent"}]}}"#,
        )
        .unwrap();
        assert_eq!(pane_ids, vec!["p1".to_string(), "p2".to_string()]);
        assert!(parse_pane_ids(r#"{"result":{"panes":[{"agent":"agent"}]}}"#).is_err());
    }

    #[test]
    fn subscription_size_is_bounded() {
        let ids = vec!["pane-id-".repeat(MAX_FRAME / 8)];
        assert!(subscription_request(&ids).is_err());
    }

    #[test]
    fn ndjson_parser_keeps_fragmented_frames_and_reports_eof_and_limits() {
        let mut frames = NdjsonBuffer::default();
        assert!(frames
            .push(br#"{"event":"pane.created","data":{"pane_id":"p1"}}"#)
            .unwrap()
            .is_empty());
        let lines = frames.push(b"\n").unwrap();
        assert_eq!(
            lines,
            vec![r#"{"event":"pane.created","data":{"pane_id":"p1"}}"#]
        );
        assert!(frames.parse_eof().is_err());
        let mut incomplete = NdjsonBuffer::default();
        incomplete.push(b"{").unwrap();
        assert!(incomplete.parse_eof().unwrap_err().contains("incomplete"));
        assert!(frames.push(&[b'x'; MAX_FRAME + 1]).is_err());
    }
}
