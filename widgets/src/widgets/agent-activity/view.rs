//! A scrollable body; only the shared title and footer are pinned.
use super::model::{self, Agent, Snapshot};
use opscope_core as tc;

#[derive(Default)]
pub struct Body {
    pub rows: Vec<String>,
    pub placed: Vec<(usize, usize)>,
}

fn words(out: &mut Vec<String>, text: &str, width: usize, role: tc::Role, selected: bool) {
    let tint = if selected {
        tc::tint(tc::Role::Slot)
    } else {
        String::new()
    };
    // Shared lifted theme roles meet AA on both the pane and selected tint.
    let ink = format!("{tint}{}", tc::ink_on(role, selected));
    for row in tc::wrap_words(&tc::source_text(text), width.max(1)) {
        out.push(tc::seg(&[(&ink, row)], width));
    }
}

fn work_rows(out: &mut Vec<String>, a: &Agent, width: usize, selected: bool, recent: usize) {
    let mut work = a.work.rows(width);
    if work.is_empty() {
        let text = if a.work.logs.session_id.is_empty() {
            "   no matching session log or observed shell jobs"
        } else {
            "   no unfinished children or observed shell jobs"
        };
        words(out, text, width, tc::Role::Muted, selected);
    }
    for (warning, row) in work.drain(..) {
        words(
            out,
            &row,
            width,
            if warning {
                tc::Role::Orange
            } else {
                tc::Role::Fg
            },
            selected,
        );
    }
    let finished = model::finished(a, recent);
    if !finished.is_empty() {
        let total = a
            .work
            .logs
            .tasks
            .iter()
            .filter(|t| t.state.finished())
            .count();
        words(
            out,
            &format!(
                "   latest finished: {} of {total} in matched session",
                finished.len()
            ),
            width,
            tc::Role::Muted,
            selected,
        );
        for t in finished {
            words(
                out,
                &format!(
                    "   └ {}  {} · via log · {}",
                    t.label,
                    t.state.words(),
                    updated(&t.updated)
                ),
                width,
                if t.state == tc::agent_work::Activity::Error {
                    tc::Role::Orange
                } else {
                    tc::Role::Muted
                },
                selected,
            );
        }
    }
}

fn updated(stamp: &str) -> String {
    if stamp.is_empty() {
        return "time not recorded".into();
    }
    if let Ok(ms) = stamp.parse::<i64>() {
        return chrono::DateTime::from_timestamp_millis(ms)
            .map(|t| t.to_rfc3339())
            .unwrap_or_else(|| format!("source timestamp {stamp}"));
    }
    stamp.to_string()
}

fn warning(out: &mut Vec<String>, s: &Snapshot, width: usize) {
    if let Some(error) = &s.err {
        let err = format!("Source error: {error}");
        words(out, &err, width, tc::Role::Orange, false);
        if s.sampled.is_some() {
            words(
                out,
                "Showing the last successful process snapshot; counts and rows are stale.",
                width,
                tc::Role::Orange,
                false,
            );
        }
    }
}

pub fn board(s: &Snapshot, selected: usize, width: usize, recent: usize) -> Body {
    let mut body = Body::default();
    warning(&mut body.rows, s, width);
    if s.sampled.is_none() {
        if s.err.is_none() {
            words(
                &mut body.rows,
                "Reading local agent processes and session logs…",
                width,
                tc::Role::Muted,
                false,
            );
        }
        return body;
    }
    let shells: usize = s.agents.iter().map(|a| a.work.tasks.len()).sum();
    let children: usize = s
        .agents
        .iter()
        .map(|a| {
            a.work
                .logs
                .tasks
                .iter()
                .filter(|t| !t.state.finished())
                .count()
        })
        .sum();
    words(
        &mut body.rows,
        &format!(
            "{} named agent processes · {children} open log rows · {shells} observed shell jobs",
            s.agents.len()
        ),
        width,
        tc::Role::Accent,
        false,
    );
    words(&mut body.rows, "Claude Code · Codex · OpenCode — local processes; child state is last recorded, not a heartbeat.", width, tc::Role::Muted, false);
    words(&mut body.rows, "Log sessions are newest-cwd guesses. No multiplexer OSC reports are available in this standalone view.", width, tc::Role::Muted, false);
    if recent == 0 {
        let finished: usize = s
            .agents
            .iter()
            .map(|a| {
                a.work
                    .logs
                    .tasks
                    .iter()
                    .filter(|t| t.state.finished())
                    .count()
            })
            .sum();
        words(
            &mut body.rows,
            &format!("0 of {finished} matched finished log rows shown · hidden in settings"),
            width,
            tc::Role::Muted,
            false,
        );
    }
    if s.agents.is_empty() {
        words(&mut body.rows, "No supported local agent processes found. Start Claude Code, Codex or OpenCode on this machine.", width, tc::Role::Fg, false);
    }
    for (index, a) in s.agents.iter().enumerate() {
        body.rows.push(String::new());
        let top = body.rows.len();
        let here = index == selected;
        let head = if width >= 80 {
            format!(
                "{} {} · pid {} · process present · {} shell jobs",
                if here { "▸" } else { " " },
                a.kind,
                a.pid,
                a.work.tasks.len()
            )
        } else {
            format!(
                "{} {} · pid {} · process present",
                if here { "▸" } else { " " },
                a.kind,
                a.pid
            )
        };
        words(&mut body.rows, &head, width, tc::Role::Accent, here);
        match &a.cwd {
            Ok(cwd) => words(
                &mut body.rows,
                &format!("  cwd {cwd}"),
                width,
                tc::Role::Muted,
                here,
            ),
            Err(why) => words(
                &mut body.rows,
                &format!("  cwd unread: {why}"),
                width,
                tc::Role::Orange,
                here,
            ),
        }
        work_rows(&mut body.rows, a, width, here, recent);
        body.placed
            .extend((top..body.rows.len()).map(|row| (row, index)));
    }
    body
}

pub fn detail(s: &Snapshot, a: &Agent, present: bool, width: usize, recent: usize) -> Body {
    let mut body = Body::default();
    warning(&mut body.rows, s, width);
    words(
        &mut body.rows,
        &format!(
            "{} · pid {} · {}",
            a.kind,
            a.pid,
            if present {
                "process present in snapshot"
            } else {
                "process no longer seen; last captured details"
            }
        ),
        width,
        tc::Role::Accent,
        false,
    );
    words(
        &mut body.rows,
        &format!("Command: {}", a.command),
        width,
        tc::Role::Fg,
        false,
    );
    words(
        &mut body.rows,
        &format!(
            "Agent cwd: {}",
            a.cwd.as_ref().map(String::as_str).unwrap_or("unreadable")
        ),
        width,
        tc::Role::Fg,
        false,
    );
    if let Some(explain) = a.work.logs.explanation() {
        words(&mut body.rows, &explain, width, tc::Role::Orange, false);
    } else {
        words(
            &mut body.rows,
            "No matching session log found; process-backed shells can still be shown.",
            width,
            tc::Role::Muted,
            false,
        );
    }
    words(&mut body.rows,"Sources: local session stores (via log), then agent child shells (via process). No OSC report connection. A named process does not prove a model turn is running.",width,tc::Role::Muted,false);
    words(&mut body.rows,&format!("Finished view: up to {recent} latest finished log children from this matched session. Excludes older sessions and exited parent processes; this is not total history."),width,tc::Role::Muted,false);
    work_rows(&mut body.rows, a, width, false, recent);
    body
}

/// A frame-local follow flag is passed only after a selection key or click.
pub fn viewport(
    body: &Body,
    selected: usize,
    mut scroll: usize,
    room: usize,
    chase: bool,
) -> usize {
    if chase {
        let rows: Vec<_> = body
            .placed
            .iter()
            .filter(|(_, i)| *i == selected)
            .map(|(r, _)| *r)
            .collect();
        if let (Some(&first), Some(&last)) = (rows.first(), rows.last()) {
            scroll = tc::follow(scroll, last, room);
            scroll = tc::follow(scroll, first, room);
        }
    }
    scroll.min(body.rows.len().saturating_sub(room))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tc::agent_work::{Activity, LogTask, Work};

    fn sample() -> Snapshot {
        let mut work = Work::default();
        work.logs.guessed = true;
        work.logs.session_id = "parent-session".into();
        work.logs.tasks.push(LogTask {
            id: "child".into(),
            label: "Explore a lengthy repository path with Unicode 界".into(),
            state: Activity::Working,
            updated: String::new(),
        });
        Snapshot {
            agents: vec![Agent {
                pid: 20,
                kind: "claude".into(),
                command: "claude --resume sample".into(),
                cwd: Ok("/srv/a-directory-with-a-long-name/界".into()),
                work,
            }],
            sampled: Some(std::time::Instant::now()),
            err: None,
        }
    }

    #[test]
    fn every_row_fits_narrow_and_wide_panes_and_keeps_content() {
        let s = sample();
        for w in 12..=160 {
            for body in [board(&s, 0, w, 3), detail(&s, &s.agents[0], true, w, 3)] {
                for row in &body.rows {
                    assert!(
                        tc::display_width(&tc::source_text(row)) <= w,
                        "width {w}: {row:?}"
                    );
                }
                let plain = body
                    .rows
                    .iter()
                    .map(|r| tc::source_text(r))
                    .collect::<String>();
                assert!(plain.contains("Explore"), "width {w}");
            }
        }
    }

    #[test]
    fn wheel_view_does_not_follow_the_selected_block_back() {
        let body = Body {
            rows: vec![String::new(); 100],
            placed: vec![(2, 0), (3, 0), (80, 1)],
        };
        assert_eq!(viewport(&body, 0, 50, 5, false), 50);
        assert_eq!(viewport(&body, 0, 50, 5, true), 2);
        assert_eq!(viewport(&body, 1, 0, 5, true), 76);
    }

    #[test]
    fn hiding_finished_work_is_an_explicit_subset_in_the_body() {
        let mut s = sample();
        s.agents[0].work.logs.tasks[0].state = Activity::Done;
        let text = board(&s, 0, 100, 0)
            .rows
            .iter()
            .map(|r| tc::source_text(r))
            .collect::<String>();
        assert!(text.contains("0 of 1 matched finished log rows shown"));
        assert!(text.contains("hidden in settings"));
    }

    #[test]
    fn loading_empty_and_stale_error_have_different_words() {
        let loading = board(&Snapshot::default(), 0, 80, 3).rows.join("");
        assert!(loading.contains("Reading"));
        let empty = Snapshot {
            sampled: Some(std::time::Instant::now()),
            ..Default::default()
        };
        assert!(board(&empty, 0, 80, 3)
            .rows
            .join("")
            .contains("No supported"));
        let mut stale = sample();
        stale.err = Some("process read failed".into());
        let text = board(&stale, 0, 80, 3).rows.join("");
        assert!(
            text.contains("stale")
                && text.contains("process read failed")
                && text.contains("Explore")
        );
    }
}
