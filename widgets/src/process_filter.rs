//! Session-local visibility of dashboard processes in pane widgets.
#[derive(Default)]
pub struct Filter {
    pub show: bool,
}

pub fn matches(name: &str, command: &str) -> bool {
    name.to_ascii_lowercase().contains("opscope")
        || command.to_ascii_lowercase().contains("opscope")
}

impl Filter {
    pub fn visible(&self, is_opscope: bool) -> bool {
        self.show || !is_opscope
    }

    pub fn toggle(&mut self, selected: &mut usize) {
        self.show = !self.show;
        *selected = 0;
    }

    pub fn description(&self, hidden: usize) -> Vec<String> {
        if hidden == 0 {
            Vec::new()
        } else {
            vec![format!(
                "{} opscope {} hidden",
                hidden,
                if hidden == 1 { "process" } else { "processes" }
            )]
        }
    }
}

pub fn empty_message(hidden: usize, remaining: usize, otherwise: &str) -> String {
    if remaining > 0 {
        return otherwise.to_string();
    }
    opscope_core::filtered_to_nothing(hidden, &["opscope processes hidden".into()])
        .unwrap_or_else(|| otherwise.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toggling_restores_counts_and_resets_selection_repeatedly() {
        let mut filter = Filter::default();
        let entries = [true, false, true];
        let count = |filter: &Filter| entries.iter().filter(|&&n| filter.visible(n)).count();
        assert_eq!(count(&filter), 1);
        assert_eq!(filter.description(2), vec!["2 opscope processes hidden"]);
        let mut selected = 2;
        for _ in 0..3 {
            filter.toggle(&mut selected);
            assert_eq!(count(&filter), 3);
            assert_eq!(selected, 0);
            selected = 2;
            filter.toggle(&mut selected);
            assert_eq!(count(&filter), 1);
            assert_eq!(selected, 0);
        }
        assert!(!filter.visible(true));
        assert!(empty_message(2, 0, "idle").contains("opscope processes hidden"));
        assert_eq!(empty_message(0, 0, "idle"), "idle");
        assert!(filter.description(0).is_empty());
        assert_eq!(filter.description(1), vec!["1 opscope process hidden"]);
        assert_eq!(empty_message(1, 1, "idle"), "idle");
    }
}
