use super::parse;
use serde_json::json;

pub fn reconcile_result(
    result: &Result<parse::WaitResult, String>,
    waited_epoch: u64,
    cursor: u64,
    current_baseline: (u64, u64),
) -> Option<((u64, u64), bool)> {
    if current_baseline.0 != waited_epoch {
        return match result {
            Ok(parse::WaitResult::Matched { .. }) => Some((current_baseline, true)),
            Ok(parse::WaitResult::TimedOut { .. }) => Some((current_baseline, false)),
            Err(_) => None,
        };
    }
    match result {
        Ok(parse::WaitResult::Matched { event_sequence }) => {
            Some(((waited_epoch, *event_sequence), true))
        }
        Ok(parse::WaitResult::TimedOut { sequence }) => {
            Some(((waited_epoch, sequence.unwrap_or(cursor)), false))
        }
        Err(_) => None,
    }
}

pub fn wait_request(after_sequence: u64, timeout_s: u64) -> String {
    let timeout_s = timeout_s.clamp(1, 30);
    json!({
        "id": parse::EVENT_WAIT_ID,
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;

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
                &Ok(parse::WaitResult::Matched { event_sequence: 90 }),
                3,
                50,
                (4, 61),
            ),
            Some(((4, 61), true))
        );
        assert_eq!(
            reconcile_result(
                &Ok(parse::WaitResult::TimedOut { sequence: Some(54) }),
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
