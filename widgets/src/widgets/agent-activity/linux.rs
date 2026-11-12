//! Linux acquisition only; the process table itself is shared in core.
use opscope_core as tc;
use std::collections::HashMap;

pub fn cwds(pids: &[u32]) -> HashMap<u32, Result<String, String>> {
    pids.iter()
        .map(|&pid| {
            let cwd = i32::try_from(pid)
                .map_err(|_| "invalid agent pid".to_string())
                .and_then(tc::procfs::read_cwd);
            (pid, cwd)
        })
        .collect()
}
