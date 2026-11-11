//! Native cwd fields; compiled and tested on both platforms.
use std::collections::HashMap;

/// NUL-terminated lsof records. Only structural leading newlines are removed;
/// a path's embedded newline or trailing whitespace belongs to the path.
pub fn parse_cwds(text: &str) -> HashMap<u32, String> {
    let mut found = HashMap::new();
    let mut pid = None;
    for field in text.split('\0') {
        let field = field.trim_start_matches('\n');
        if let Some(value) = field.strip_prefix('p') {
            pid = value.parse().ok();
        } else if let Some(path) = field.strip_prefix('n').filter(|p| p.starts_with('/')) {
            if let Some(pid) = pid {
                found.insert(pid, path.to_string());
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    #[test]
    fn native_fields_preserve_identity_and_literal_paths() {
        let got = super::parse_cwds(
            "p20\0\nfcwd\0n/srv/repo\0\np21\0\nn/other\npath \0\npbad\0n/wrong\0",
        );
        assert_eq!(got.get(&20).map(String::as_str), Some("/srv/repo"));
        assert_eq!(got.get(&21).map(String::as_str), Some("/other\npath "));
        assert_eq!(got.len(), 2);
    }
}
