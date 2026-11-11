//! macOS acquisition: one lsof cwd read for all identified agent pids.
use opscope_core as tc;
use std::collections::HashMap;

pub fn cwds(pids: &[u32]) -> HashMap<u32, Result<String, String>> {
    if pids.is_empty() {
        return HashMap::new();
    }
    let list = pids
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    match tc::run_full(&["lsof", "-a", "-p", &list, "-d", "cwd", "-F0pn"], 5) {
        Err(why) => pids.iter().map(|&pid| (pid, Err(why.clone()))).collect(),
        Ok(out) => {
            // Partial successful output remains useful when another pid exits.
            let parsed = std::str::from_utf8(&out.stdout)
                .map(super::parse::parse_cwds)
                .map_err(|e| format!("invalid cwd UTF-8: {e}"));
            let stderr = String::from_utf8_lossy(&out.stderr);
            let why = stderr
                .lines()
                .find(|s| !s.trim().is_empty())
                .unwrap_or("agent cwd not returned: process exited or is unreadable");
            pids.iter()
                .map(|&pid| {
                    let cwd = parsed
                        .as_ref()
                        .map_err(Clone::clone)
                        .and_then(|map| map.get(&pid).cloned().ok_or_else(|| why.to_string()));
                    (pid, cwd)
                })
                .collect()
        }
    }
}
