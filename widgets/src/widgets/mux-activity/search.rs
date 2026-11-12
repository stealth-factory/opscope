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

//! The `/` filter every backend shares: a query line, and whether a row's
//! words fuzzy-match it.
//!
//! The match only includes or excludes a row. Each board keeps its own
//! order - urgency first, then grouped by session - so a blocked agent
//! never drops below an idle one that happened to match better.

use opscope_core as tc;

/// Whether `query` fuzzy-matches any one of `fields`, the words a row
/// shows. Each field is matched on its own, the way the palette matches a
/// label: a query spread across a name and a directory is not a match
/// anybody meant. An empty query matches every row.
pub fn matches<S: AsRef<str>>(query: &str, fields: &[S]) -> bool {
    query.trim().is_empty() || fields.iter().any(|f| tc::fuzzy(query, f.as_ref()).is_some())
}

/// A section heading's count: `14`, or `3 of 14` while a filter holds some
/// of them back.
pub fn count(shown: usize, of: usize, filtering: bool) -> String {
    if filtering {
        format!("{} of {}", shown, of)
    } else {
        of.to_string()
    }
}

#[derive(Default)]
pub struct Search {
    pub query: String,
    /// The query line is open and keys are text.
    pub typing: bool,
    /// The query changed since the board last read it, so a cursor whose
    /// pane it hid goes to the first match rather than staying put.
    changed: bool,
}

impl Search {
    /// A query is in force: open or kept, and not blank.
    pub fn active(&self) -> bool {
        !self.query.trim().is_empty()
    }

    pub fn matches<S: AsRef<str>>(&self, fields: &[S]) -> bool {
        matches(&self.query, fields)
    }

    /// The filter as `tc::filter_row()` names it, `/api`.
    pub fn filter(&self) -> Option<String> {
        self.active().then(|| format!("/{}", self.query))
    }

    pub fn open(&mut self) {
        self.typing = true;
    }

    pub fn clear(&mut self) {
        if !self.query.is_empty() {
            self.changed = true;
        }
        self.query.clear();
        self.typing = false;
    }

    /// A key while the query line is open. Only `esc` and `enter` are
    /// navigation, or the query could never hold a `q`.
    pub fn key(&mut self, key: &str) {
        match key {
            "esc" => self.clear(),
            "enter" => self.typing = false,
            "backspace" => {
                if self.query.pop().is_some() {
                    self.changed = true;
                }
            }
            other if other.chars().count() == 1 => {
                self.query.push_str(other);
                self.changed = true;
            }
            _ => {}
        }
    }

    /// Whether the query changed since this was last asked, and forget it.
    pub fn take_changed(&mut self) -> bool {
        std::mem::take(&mut self.changed)
    }

    /// The footer while the query line is open: what is typed, then the two
    /// keys that leave it. `width` is what the footer is packed to; a query
    /// longer than that shows its tail behind `…`, so the cursor stays on
    /// screen and the row never wraps.
    pub fn typing_hints<'a>(
        &self,
        accent: &'a str,
        dim: &'a str,
        width: usize,
    ) -> Vec<Vec<(&'a str, String)>> {
        vec![
            vec![(accent, query_line(&self.query, width))],
            vec![(dim, "[↵] keep".into())],
            vec![(dim, "[esc] clear".into())],
        ]
    }

    /// The board footer's own hints: `/` to filter, or to edit the filter in
    /// force, and `esc` to clear it while there is one.
    pub fn hints<'a>(&self, dim: &'a str) -> Vec<Vec<(&'a str, String)>> {
        if self.active() {
            vec![vec![(dim, "[/] edit filter".into())], vec![(dim, "[esc] clear".into())]]
        } else {
            vec![vec![(dim, "[/]filter".into())]]
        }
    }
}

/// `/query▌` in at most `width` columns: whole when it fits, otherwise
/// `/…` and as much of the end as fits, since the end is where the typing
/// is.
fn query_line(query: &str, width: usize) -> String {
    let whole = format!("/{}▌", query);
    if tc::display_width(&whole) <= width {
        return whole;
    }
    // `/`, `…` and `▌` are a column each.
    let room = width.saturating_sub(3);
    let mut used = 0;
    let mut tail: Vec<char> = Vec::new();
    for c in query.chars().rev() {
        let cw = tc::display_width(c.encode_utf8(&mut [0; 4]));
        if used + cw > room {
            break;
        }
        used += cw;
        tail.push(c);
    }
    let tail: String = tail.into_iter().rev().collect();
    format!("/…{}▌", tail)
}

/// Where the cursor goes once the list is rebuilt: on the same row if it is
/// still listed; on the first row when the filter just hid it; otherwise
/// where it was, for the caller to clamp. `same` says whether two rows are
/// the same pane.
pub fn reselect<T>(before: &[T], selected: usize, after: &[T], filter_changed: bool, same: impl Fn(&T, &T) -> bool) -> usize {
    match before.get(selected) {
        Some(held) => match after.iter().position(|n| same(held, n)) {
            Some(at) => at,
            None if filter_changed => 0,
            None => selected,
        },
        None if filter_changed => 0,
        None => selected,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_empty_query_matches_every_row() {
        assert!(matches("", &["claude", "work"]));
        assert!(matches::<&str>("   ", &[]));
    }

    #[test]
    fn case_does_not_matter() {
        assert!(matches("API", &["~/src/api-server"]));
        assert!(matches("api", &["Fix the API client"]));
    }

    #[test]
    fn characters_in_order_across_word_starts_match() {
        // c-o at word starts: "codex opscope".
        assert!(matches("co", &["codex opscope"]));
        assert!(matches("fb", &["feature/branch"]));
    }

    #[test]
    fn a_word_start_scores_better_than_the_middle_of_a_word() {
        let start = tc::fuzzy("api", "api-server").unwrap();
        let middle = tc::fuzzy("api", "rapid").unwrap();
        assert!(start < middle, "{start} {middle}");
    }

    #[test]
    fn no_field_matching_is_no_match() {
        assert!(!matches("zzz", &["claude", "work", "~/src/api"]));
        // Letters scattered through the middle of words are not a match.
        assert!(!matches("drac", &["editor classics"]));
    }

    #[test]
    fn each_field_is_matched_on_its_own() {
        // "cw" would match "claude work" joined, but neither word alone.
        assert!(!matches("cw", &["claude", "xwork"]));
        assert!(matches("cl", &["xwork", "claude"]));
    }

    #[test]
    fn heading_counts_say_a_subset_only_while_filtering() {
        assert_eq!(count(3, 14, true), "3 of 14");
        assert_eq!(count(14, 14, false), "14");
    }

    #[test]
    fn the_query_line_takes_text_and_leaves_on_esc_or_enter() {
        let mut s = Search::default();
        s.open();
        for k in ["a", "q", "i", "backspace", "p"] {
            s.key(k);
        }
        assert_eq!(s.query, "aqp");
        assert!(s.take_changed());
        assert!(!s.take_changed());
        s.key("enter");
        assert!(!s.typing && s.active());
        assert_eq!(s.filter().as_deref(), Some("/aqp"));
        s.open();
        s.key("esc");
        assert!(!s.typing && !s.active() && s.filter().is_none());
        assert!(s.take_changed());
    }

    #[test]
    fn a_long_query_shows_its_end_within_the_width() {
        // Short enough: shown whole.
        assert_eq!(query_line("api", 20), "/api▌");
        // Too long for 20 columns: the tail, still ending at the cursor.
        let line = query_line("abcdefghijklmnopqrstuvwx", 20);
        assert_eq!(tc::display_width(&line), 20);
        assert_eq!(line, "/…hijklmnopqrstuvwx▌");
        // Wide characters are measured as two columns.
        let wide = query_line("日本語のテキスト検索", 12);
        assert!(tc::display_width(&wide) <= 12, "{wide}");
        assert!(wide.ends_with("検索▌"));
    }

    #[test]
    fn the_cursor_stays_on_its_pane_or_goes_to_the_first_match() {
        let same = |a: &&str, b: &&str| a == b;
        // Still listed: it follows the pane, not the index.
        assert_eq!(reselect(&["a", "b", "c"], 1, &["b", "c"], true, same), 0);
        assert_eq!(reselect(&["a", "b", "c"], 2, &["a", "c"], true, same), 1);
        // Hidden by the filter: the first match.
        assert_eq!(reselect(&["a", "b", "c"], 2, &["a", "b"], true, same), 0);
        // Gone on a refresh with no filter change: left for the caller.
        assert_eq!(reselect(&["a", "b", "c"], 2, &["a", "b"], false, same), 2);
    }
}
