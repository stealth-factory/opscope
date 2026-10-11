use serde_json::{json, Value};

const WAIT_ID: &str = "opscope-luvus-events-wait";

#[derive(Debug, PartialEq, Eq)]
pub enum WaitResult {
    Matched { event_sequence: u64 },
    TimedOut { sequence: Option<u64> },
}

pub fn reconcile_result(
    result: &Result<WaitResult, String>,
    waited_epoch: u64,
    cursor: u64,
    current_baseline: (u64, u64),
) -> Option<((u64, u64), bool)> {
    if current_baseline.0 != waited_epoch {
        return match result {
            Ok(WaitResult::Matched { .. }) => Some((current_baseline, true)),
            Ok(WaitResult::TimedOut { .. }) => Some((current_baseline, false)),
            Err(_) => None,
        };
    }
    match result {
        Ok(WaitResult::Matched { event_sequence }) => Some(((waited_epoch, *event_sequence), true)),
        Ok(WaitResult::TimedOut { sequence }) => {
            Some(((waited_epoch, sequence.unwrap_or(cursor)), false))
        }
        Err(_) => None,
    }
}

pub fn wait_request(after_sequence: u64, timeout_s: u64) -> String {
    let timeout_s = timeout_s.clamp(1, 30);
    json!({
        "id": WAIT_ID,
        "method": "events.wait",
        "params": {
            "event": "pane.agent_status_changed",
            "after_sequence": after_sequence,
            "timeout_s": timeout_s
        }
    })
    .to_string()
        + "\n"
}

pub fn parse_wait_response(text: &str, after_sequence: u64) -> Result<WaitResult, String> {
    let response: Value =
        serde_json::from_str(text).map_err(|e| format!("unreadable event wait answer: {e}"))?;
    if response["id"].as_str() != Some(WAIT_ID) {
        return Err("event wait answer has an unexpected request id".into());
    }
    if response["error"].is_object() {
        let code = response["error"]["code"].as_str().unwrap_or("");
        let message = response["error"]["message"].as_str().unwrap_or("").trim();
        if code == "timeout" {
            return Ok(WaitResult::TimedOut { sequence: None });
        }
        if code.contains("resync_required")
            || code == "events_lost"
            || message.contains("resync_required")
        {
            return Err("event history requires a new snapshot".into());
        }
        return Err(if message.is_empty() {
            format!("event wait failed: {}", response["error"])
        } else {
            message.to_string()
        });
    }
    let result = response
        .get("result")
        .filter(|result| result.is_object())
        .ok_or_else(|| "event wait answer has no result".to_string())?;
    if result["terminal"]["resync_required"] == true
        || result["events"]["resync_required"] == true
        || result["resync_required"] == true
        || result["gap"] == true
        || result["lost"] == true
        || result["events_lost"] == true
        || matches!(
            result["type"].as_str(),
            Some("resync_required" | "events_lost")
        )
    {
        return Err("event history requires a new snapshot".into());
    }
    if result["matched"].as_bool() == Some(false) {
        let sequence = result["sequence"]
            .as_u64()
            .ok_or_else(|| "event wait timeout has no sequence".to_string())?;
        if sequence < after_sequence {
            return Err("event sequence moved backwards; a new snapshot is required".into());
        }
        return Ok(WaitResult::TimedOut {
            sequence: Some(sequence),
        });
    }
    if result["matched"].as_bool() != Some(true) {
        return Err("event wait answer has no matched flag".into());
    }
    let event_name = result["event"]["event"].as_str();
    if matches!(
        event_name,
        Some("events.resync_required" | "events_lost" | "event.gap")
    ) {
        return Err("event history requires a new snapshot".into());
    }
    if event_name != Some("pane.agent_status_changed") {
        return Err("event wait returned an unexpected event".into());
    }
    let event_sequence = result["event"]["sequence"]
        .as_u64()
        .ok_or_else(|| "matched event has no sequence".to_string())?;
    if event_sequence <= after_sequence {
        return Err("matched event sequence did not advance; a new snapshot is required".into());
    }
    Ok(WaitResult::Matched { event_sequence })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matched_events_use_the_event_sequence_not_the_registration_fence() {
        let result = parse_wait_response(
            r#"{"id":"opscope-luvus-events-wait","result":{"type":"event_wait","matched":true,"sequence":18,
              "event":{"event":"pane.agent_status_changed","sequence":23,"data":{"pane":"7","status":"done"}}}}"#,
            18,
        )
        .unwrap();
        assert_eq!(result, WaitResult::Matched { event_sequence: 23 });
    }

    #[test]
    fn unmatched_wait_advances_to_its_result_sequence() {
        assert_eq!(
            parse_wait_response(
                r#"{"id":"opscope-luvus-events-wait","result":{"type":"event_wait","matched":false,"sequence":29}}"#,
                23,
            ),
            Ok(WaitResult::TimedOut { sequence: Some(29) })
        );
    }

    #[test]
    fn filtered_sequence_jumps_are_not_mistaken_for_lost_events() {
        assert_eq!(
            parse_wait_response(
                r#"{"id":"opscope-luvus-events-wait","result":{"matched":true,"sequence":21,
                  "event":{"event":"pane.agent_status_changed","sequence":47,"data":{}}}}"#,
                21,
            ),
            Ok(WaitResult::Matched { event_sequence: 47 })
        );
    }

    #[test]
    fn malformed_resync_and_backwards_answers_require_a_snapshot() {
        for response in [
            "not json",
            r#"{"id":"unrelated","result":{"matched":false,"sequence":11}}"#,
            r#"{"id":"opscope-luvus-events-wait","error":{"code":"events.resync_required","message":"resync"}}"#,
            r#"{"id":"opscope-luvus-events-wait","result":{"resync_required":true}}"#,
            r#"{"id":"opscope-luvus-events-wait","result":{"gap":true,"matched":false,"sequence":10}}"#,
            r#"{"id":"opscope-luvus-events-wait","result":{"matched":true,"event":{"event":"events.resync_required","sequence":11}}}"#,
            r#"{"id":"opscope-luvus-events-wait","result":{"matched":false,"sequence":9}}"#,
            r#"{"id":"opscope-luvus-events-wait","result":{"matched":true,"event":{"event":"pane.agent_status_changed","sequence":10}}}"#,
        ] {
            assert!(parse_wait_response(response, 10).is_err(), "{response}");
        }
    }

    #[test]
    fn request_is_one_filtered_wait_with_a_cursor_and_bounded_timeout() {
        let request: Value = serde_json::from_str(wait_request(42, 30).trim()).unwrap();
        assert_eq!(request["method"], "events.wait");
        assert_eq!(request["params"]["event"], "pane.agent_status_changed");
        assert_eq!(request["params"]["after_sequence"], 42);
        assert_eq!(request["params"]["timeout_s"], 30);
        let short: Value = serde_json::from_str(wait_request(0, 0).trim()).unwrap();
        assert_eq!(short["params"]["timeout_s"], 1);
        let long: Value = serde_json::from_str(wait_request(0, 300).trim()).unwrap();
        assert_eq!(long["params"]["timeout_s"], 30);
    }

    #[test]
    fn stale_wait_matches_wake_without_overwriting_a_new_snapshot_cursor() {
        assert_eq!(
            reconcile_result(
                &Ok(WaitResult::Matched { event_sequence: 90 }),
                3,
                50,
                (4, 61),
            ),
            Some(((4, 61), true))
        );
        assert_eq!(
            reconcile_result(
                &Ok(WaitResult::TimedOut { sequence: Some(54) }),
                3,
                50,
                (4, 61),
            ),
            Some(((4, 61), false))
        );
        assert_eq!(
            reconcile_result(&Err("malformed".into()), 3, 50, (4, 61)),
            None
        );
    }
}
