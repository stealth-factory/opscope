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

//! Every widget here, what it does, and a preview before it runs.
//!
//! This is packaging and navigation, not a widget. Each real widget owns
//! the summary, explanation, and preview in its folder; the launcher
//! compiles those same files rather than restating them.

use std::time::Duration;

use opscope_core as tc;

const SETTINGS: tc::SettingsSpec = tc::SettingsSpec {
    widget: "opscope",
    section: "terminal",
    legacy_section: None,
    schema: include_str!("settings.json"),
    catalogues: &[],
};

/// Each widget's own words, taken from that widget's folder.
struct Widget {
    stem: &'static str,
    help: &'static str,
    readme: &'static str,
    dependencies: &'static str,
}

macro_rules! widget {
    ($stem:literal) => {
        Widget {
            stem: $stem,
            help: include_str!(concat!("../widgets/", $stem, "/help.txt")),
            readme: include_str!(concat!("../widgets/", $stem, "/README.md")),
            dependencies: include_str!(concat!("../widgets/", $stem, "/dependencies.json")),
        }
    };
}

const WIDGETS: &[Widget] = &[
    widget!("agent-usage"),
    widget!("clocks"),
    widget!("github"),
    widget!("github-actions"),
    widget!("github-prs"),
    widget!("herdr-panes"),
    widget!("latency"),
    widget!("linear"),
    widget!("link"),
    widget!("matrix"),
    widget!("months"),
    widget!("netwatch"),
    widget!("ports"),
    widget!("tailnet"),
    widget!("vercel-deployments"),
];

impl Widget {
    /// The row: this widget's own first line.
    fn summary(&self) -> &'static str {
        self.help.lines().next().unwrap_or("")
    }

    /// The aside: the paragraph under the summary, which is where each
    /// widget explains why it exists.
    ///
    /// Only that paragraph. What follows is the usage synopsis and the key
    /// list, which are for somebody reading --help rather than somebody
    /// deciding whether this is the thing they want.
    fn about(&self) -> String {
        let mut para: Vec<&str> = Vec::new();
        for line in self.help.lines().skip(2) {
            if line.starts_with("    ") {
                break; // an indented usage block
            }
            if line.trim().is_empty() {
                if !para.is_empty() {
                    break;
                }
                continue;
            }
            para.push(line.trim());
        }
        para.join(" ").chars().take(400).collect()
    }

    /// The picture from this widget's doc page, if it has one.
    ///
    /// Every doc opens with a rendering of the widget it describes, kept by
    /// whoever wrote it and read by whoever is deciding whether to run the
    /// thing. Using that means no second copy of anything - and, more to
    /// the point, no widget has to be started to be looked at.
    fn sample(&self) -> Vec<&'static str> {
        let mut block = Vec::new();
        let mut inside = false;
        for line in self.readme.lines() {
            if line.starts_with("```") {
                if inside {
                    break;
                }
                inside = true;
                continue;
            }
            if inside {
                block.push(line);
            }
        }
        // Only a block that is actually a picture of the widget: the docs
        // also hold shell snippets and JSON, and a config listing is not a
        // preview.
        match block.first() {
            Some(first) if first.starts_with("╺━") => block,
            _ => Vec::new(),
        }
    }
}

/// Break a paragraph at spaces, for the note under the list.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest: Vec<char> = text.trim().chars().collect();
    while !rest.is_empty() && lines.len() < 3 {
        if rest.len() <= width {
            lines.push(rest.iter().collect());
            break;
        }
        let cut = rest[..(width + 1).min(rest.len())]
            .iter()
            .rposition(|c| *c == ' ')
            .filter(|c| *c > width / 2)
            .unwrap_or(width);
        lines.push(rest[..cut].iter().collect());
        rest = rest[cut..].iter().skip_while(|c| **c == ' ').copied().collect();
    }
    lines
}

/// Where the widgets live: beside this binary, whatever it was called from.
fn beside(stem: &str) -> Option<std::path::PathBuf> {
    let here = std::env::current_exe().ok()?;
    Some(here.parent()?.join(stem))
}

struct Palette {
    dim: String,
    grid: String,
    txt: String,
    lbl: String,
    accent: String,
}

fn palette() -> Palette {
    Palette {
        dim: tc::rgb(127, 147, 172),
        grid: tc::rgb(60, 78, 98),
        txt: tc::rgb(225, 235, 245),
        lbl: tc::rgb(130, 165, 200),
        accent: tc::rgb(150, 210, 255),
    }
}

/// The rows to draw, and where the window sits.
///
/// Returns the first index shown, so the caller can say what it is
/// showing rather than presenting a slice as the whole list.
///
/// `from` is where the window sat last frame. `chase` is true only on a
/// frame a key moved the cursor: then the window moves by as little as it
/// takes to hold it, because a list that jumps to centre the selection
/// loses the reader's place. On a frame the wheel moved the view, `from`
/// stands and the cursor is allowed to scroll out of sight - it is still
/// what enter launches, and the next arrow press brings the window to it.
fn window_for(
    count: usize,
    selected: usize,
    room: usize,
    from: usize,
    chase: bool,
) -> (usize, usize) {
    if count <= room || room == 0 {
        return (0, count);
    }
    let last = count - room;
    let first = if chase {
        tc::follow(from.min(last), selected, room)
    } else {
        from.min(last)
    };
    (first, room)
}

fn rows_for(w: usize, selected: usize, first: usize, room: usize, p: &Palette) -> Vec<String> {
    let name_w = WIDGETS
        .iter()
        .map(|item| item.stem.chars().count())
        .max()
        .unwrap_or(12);
    // Every column keeps a space of its own, so a summary that fills its
    // width stops short of whatever is beside it rather than running in.
    let text_w = ((w - 1).saturating_sub(name_w + 6)).max(8);
    WIDGETS
        .iter()
        .enumerate()
        .skip(first)
        .take(room)
        .map(|(i, item)| {
            let here = i == selected;
            let tint = if here { tc::bg(28, 44, 62) } else { String::new() };
            let c = |colour: &str| format!("{}{}", tint, colour);
            let mut line = vec![
                (
                    c(if here { &p.accent } else { &p.dim }),
                    if here { " ▸ ".to_string() } else { "   ".to_string() },
                ),
                (
                    c(if here { &p.txt } else { &p.lbl }),
                    tc::pad(item.stem, name_w),
                ),
                (
                    c(&p.dim),
                    tc::pad(
                        &item.summary().chars().take(text_w - 1).collect::<String>(),
                        text_w,
                    ),
                ),
            ];
            if here {
                line.push((tint.clone(), " ".repeat(w)));
            }
            let refs: Vec<(&str, String)> =
                line.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
            tc::seg(&refs, w - 1)
        })
        .collect()
}

/// Hand the terminal over, and take it back when the widget exits.
fn run_widget(keyboard: &mut tc::Keyboard, stem: &str) {
    keyboard.restore();
    tc::restore_screen();
    match beside(stem) {
        Some(path) => {
            match std::process::Command::new(&path).status() {
                Ok(_) => {}
                Err(e) => {
                    tc::out(&format!("{}: {}\r\n", path.display(), e));
                    tc::flush();
                    std::thread::sleep(Duration::from_secs(2));
                }
            }
        }
        None => {
            tc::out("cannot find where this binary lives\r\n");
            tc::flush();
            std::thread::sleep(Duration::from_secs(2));
        }
    }
    // The widget left the terminal however it left it, so take it back
    // rather than assuming: cbreak again, cursor away again, screen
    // clear, and mouse reporting on if the setting still wants it.
    // restore_screen turned it off on the way out of the child, and
    // without putting it back the menu's wheel does nothing after the
    // first launch even though the config never changed.
    keyboard.reclaim();
    tc::claim_screen();
}

/// The status a supervisor should see for a launched widget.
///
/// `ExitStatus::code()` is `None` when the child died from a signal, and
/// treating that as 0 made a crash look like a successful run. Unix
/// convention is 128 plus the signal; anywhere else, a plain failure.
fn child_exit(status: std::process::ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            return 128 + sig;
        }
    }
    1
}

/// The widget stem a command-line name refers to.
///
/// `.py` is the old Python invocation. `deployments` is the name that
/// binary answered to before it was renamed; the file is gone, the habit
/// is not.
fn resolve_stem(name: &str) -> &str {
    match name.strip_suffix(".py").unwrap_or(name) {
        "deployments" => "vercel-deployments",
        other => other,
    }
}

fn doctor() -> i32 {
    let widgets: Vec<(&str, &str)> = WIDGETS
        .iter()
        .map(|widget| (widget.stem, widget.dependencies))
        .collect();
    match tc::doctor_report(&tc::Host::detect(), &widgets) {
        Ok(report) => {
            println!("{report}");
            0
        }
        Err(error) => {
            eprintln!("cannot inspect dependencies: {error}");
            2
        }
    }
}

fn main() -> std::process::ExitCode {
    // A widget name is resolved before --help is looked at, so that
    // `start netwatch --help` is netwatch's help, not this one's. Every
    // argument after the name belongs to the widget, including that one.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first() {
        if first == "doctor" {
            if args.len() > 1 && !args[1..].iter().all(|arg| arg == "-h" || arg == "--help") {
                eprintln!("opscope doctor takes no arguments");
                return std::process::ExitCode::from(2);
            }
            if args.iter().any(|arg| arg == "-h" || arg == "--help") {
                println!(
                    "Inspect every widget's required and recommended external tools.\n\n    opscope doctor\n\nPrints host-specific installation advice; never installs anything."
                );
                return std::process::ExitCode::SUCCESS;
            }
            return std::process::ExitCode::from(doctor() as u8);
        }
        if !first.starts_with('-') {
            // `.py` is still accepted, and only for that: every widget here
            // answered to that name for years and the muscle memory outlives
            // the files. It resolves to the binary of the same stem.
            // `deployments` is the name that binary answered to before it
            // was renamed; the file is gone, the habit is not.
            let wanted = resolve_stem(first);
            let Some(found) = WIDGETS.iter().find(|w| w.stem == wanted) else {
                eprintln!(
                    "no widget called {:?} - try: {}",
                    first,
                    WIDGETS
                        .iter()
                        .map(|w| w.stem)
                        .collect::<Vec<_>>()
                        .join(", ")
                );
                std::process::exit(2);
            };
            let Some(path) = beside(found.stem) else {
                eprintln!("cannot find where this binary lives");
                std::process::exit(2);
            };
            // Replaced rather than wrapped: the menu is for browsing, not
            // something to sit between you and a widget you already named.
            let status = std::process::Command::new(&path).args(&args[1..]).status();
            std::process::exit(match status {
                Ok(s) => child_exit(s),
                Err(e) => {
                    eprintln!("{}: {}", path.display(), e);
                    2
                }
            });
        }
    }

    tc::maybe_widget_help(include_str!("help.txt"), include_str!("CONFIGURE.md"), true);
    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut selected = 0usize;
    // Where the list window sits, and whether a key has just moved the
    // cursor. The wheel writes the first and never the second.
    let (mut scroll, mut moved) = (0usize, false);

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "," => {
                    tc::run_settings(&mut keyboard, SETTINGS);
                    continue;
                }
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return std::process::ExitCode::SUCCESS;
                }
                "up" | "k" | "K" => {
                    selected = selected.saturating_sub(1);
                    moved = true;
                }
                "down" | "j" | "J" => {
                    selected += 1;
                    moved = true;
                }
                // The wheel moves the list under the cursor and leaves the
                // selection where it is - the example panel below goes on
                // showing whatever is picked.
                "ctrl-y" | "wheel-up" => scroll = scroll.saturating_sub(1),
                "ctrl-e" | "wheel-down" => scroll = scroll.saturating_add(1),
                "enter" | "right" => {
                    run_widget(&mut keyboard, WIDGETS[selected.min(WIDGETS.len() - 1)].stem)
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        if selected >= WIDGETS.len() {
            selected = WIDGETS.len() - 1;
        }

        let mut body = vec![tc::title("opscope", w, &p.accent)];
        // What is left for the list once the title, the count line, the two
        // blanks, the description heading and the footer have had theirs.
        // Drawing all of them and letting the frame cut the tail is what put
        // the cursor off the bottom of a short pane. Worked out once and used
        // by both the count line and the list: the two had drifted two rows
        // apart, so the header named a range that was not what was drawn.
        let room = h.saturating_sub(8).max(1);
        let (first, shown) = window_for(WIDGETS.len(), selected, room, scroll, moved);
        scroll = first;
        moved = false;
        body.push(tc::seg(
            &[(
                p.dim.as_str(),
                if shown < WIDGETS.len() {
                    // A partial list says so, rather than reading as the
                    // whole set with some widgets missing.
                    format!(
                        " {} widgets · showing {}-{}   ↵ or → starts one, q leaves",
                        WIDGETS.len(),
                        first + 1,
                        first + shown
                    )
                } else {
                    format!(" {} widgets   ↵ or → starts one, q leaves", WIDGETS.len())
                },
            )],
            w - 1,
        ));
        body.push(String::new());
        body.extend(rows_for(w, selected, first, shown, &p));
        body.push(String::new());

        // What the highlighted one is for, in its own words - the rest of
        // its opening paragraph, which the row has no room for. Not the
        // command to run it: that is this screen's job, not the reader's.
        let pick = &WIDGETS[selected];
        if h.saturating_sub(body.len()) >= 3 {
            body.push(tc::seg(
                &[(
                    p.lbl.as_str(),
                    format!(" ── {} ── ", pick.stem.to_uppercase()),
                )],
                w - 1,
            ));
            let tall = h.saturating_sub(body.len()) >= 12;
            let about = wrap(&pick.about(), w.saturating_sub(4));
            for line in about.iter().take(if tall { 1 } else { 3 }) {
                body.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
            }
        }

        // And what it looks like. A picture from its README rather than the
        // widget itself: starting one to look at it would ping hosts, spend
        // API quota and read the whole agent transcript tree, and browsing
        // a menu should cost nothing at all. Measured against the footer
        // that will actually be drawn, rather than a guess at its height.
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " launch".into())],
            vec![(p.dim.as_str(), "[,] settings".into())],
            vec![(p.dim.as_str(), "[q]uit".into())],
        ];
        let foot: Vec<String> = tc::pack_hints(&hints, w - 2, "  ")
            .into_iter()
            .map(|l| format!(" {}", l))
            .collect();
        let room = h.saturating_sub(body.len() + foot.len());
        let shown = pick.sample();
        if !shown.is_empty() && room >= 6 && w >= 44 {
            let rule = "─".repeat(w.saturating_sub(15).max(1));
            body.push(tc::seg(
                &[
                    (p.grid.as_str(), " ┌── ".into()),
                    (p.dim.as_str(), "example".into()),
                    (p.grid.as_str(), format!(" {}┐", rule)),
                ],
                w - 1,
            ));
            for line in shown.iter().take(room - 1) {
                body.push(tc::seg(
                    &[
                        (p.grid.as_str(), " │".into()),
                        (
                            p.dim.as_str(),
                            line.chars().take(w.saturating_sub(4)).collect::<String>(),
                        ),
                    ],
                    w - 1,
                ));
            }
        }

        while body.len() < h.saturating_sub(foot.len()) {
            body.push(String::new());
        }
        body.extend(foot);
        body.truncate(h);
        tc::draw(&body, w, h);
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_window_always_contains_the_cursor() {
        // The bug this replaces: all thirteen rows were drawn and the
        // frame cut the tail, so on a short pane the cursor moved onto a
        // row that was not there - nothing highlighted, and Enter still
        // starting whatever it was invisibly on.
        for room in 1usize..14 {
            for selected in 0..13 {
                let (first, shown) = window_for(13, selected, room, 0, true);
                assert!(
                    selected >= first && selected < first + shown,
                    "room {} cursor {} fell outside {}..{}",
                    room,
                    selected,
                    first,
                    first + shown
                );
                assert!(first + shown <= 13, "window ran past the list");
            }
        }
    }

    #[test]
    fn a_list_that_fits_is_not_windowed() {
        // No note, no scrolling, nothing changed for the pane sizes these
        // actually run at.
        assert_eq!(window_for(13, 0, 13, 0, true), (0, 13));
        assert_eq!(window_for(13, 12, 20, 0, true), (0, 13));
        assert_eq!(window_for(0, 0, 5, 0, true), (0, 0));
    }

    #[test]
    fn the_window_moves_only_as_far_as_it_must() {
        // Scrolling by one when the cursor steps off the edge, rather than
        // recentring: a list that jumps loses the reader's place.
        assert_eq!(window_for(13, 5, 6, 0, true), (0, 6));
        assert_eq!(window_for(13, 6, 6, 0, true), (1, 6));
        assert_eq!(window_for(13, 12, 6, 0, true), (7, 6));
        // And it holds still while the cursor moves about inside it.
        assert_eq!(window_for(13, 8, 6, 7, true), (7, 6));
        assert_eq!(window_for(13, 12, 6, 7, true), (7, 6));
    }

    #[test]
    fn the_wheel_moves_the_window_off_the_cursor_and_stops_at_the_end() {
        // Not chasing: the window sits where the wheel put it, and the
        // cursor is left behind rather than dragged along.
        assert_eq!(window_for(13, 0, 6, 4, false), (4, 6));
        assert_eq!(window_for(13, 12, 6, 0, false), (0, 6));
        // It stops with the last row on screen rather than scrolling into
        // blank space below it.
        assert_eq!(window_for(13, 0, 6, 99, false), (7, 6));
        // A list that fits is not windowed however far the wheel is turned.
        assert_eq!(window_for(13, 0, 13, 99, false), (0, 13));
    }


    #[test]
    fn the_old_deployments_name_still_starts_the_widget() {
        assert_eq!(resolve_stem("deployments"), "vercel-deployments");
        assert_eq!(resolve_stem("deployments.py"), "vercel-deployments");
        assert_eq!(resolve_stem("vercel-deployments"), "vercel-deployments");
        assert_eq!(resolve_stem("latency.py"), "latency");
    }

    #[test]
    fn the_menu_shows_the_whole_command() {
        // name_w used to cap at 18 and then take one cell for padding, so
        // `vercel-deployments` drew as the command that is not built.
        let p = palette();
        let rows = rows_for(86, 0, 0, WIDGETS.len(), &p);
        for widget in WIDGETS {
            assert!(
                rows.iter().any(|row| row.contains(widget.stem)),
                "{} was clipped on the menu",
                widget.stem
            );
        }
    }

    #[test]
    fn every_binary_is_on_the_menu() {
        // start.py globs the directory, so a new widget appears by existing.
        // Here the list is compiled in, and the failure mode is a widget
        // that ships without a way to find it - which linear did, for one
        // commit. The manifest is the thing that knows what was built.
        let manifest = include_str!("../../Cargo.toml");
        let mut built: Vec<&str> = Vec::new();
        let mut in_bin = false;
        for line in manifest.lines() {
            let line = line.trim();
            if line.starts_with('[') {
                in_bin = line == "[[bin]]";
                continue;
            }
            if in_bin {
                if let Some(rest) = line.strip_prefix("name = \"") {
                    if let Some(name) = rest.strip_suffix('"') {
                        built.push(name);
                    }
                }
            }
        }
        assert!(built.len() > 1, "no binaries found in the manifest");
        for name in built {
            // The menu does not list itself. Taken from the crate's own bin
            // name rather than written out, because the launcher has been
            // renamed once already and a hardcoded name here fails as
            // "opscope is built but is not on the menu" - which reads like a
            // missing widget rather than a stale string in this test.
            if name == env!("CARGO_BIN_NAME") {
                continue;
            }
            assert!(
                WIDGETS.iter().any(|w| w.stem == name),
                "{} is built but is not on the menu",
                name
            );
        }
    }

    #[test]
    fn every_widget_describes_itself() {
        // The row and the aside both come from the widget's own help text,
        // so an empty one here means a help file that lost its opening -
        // which is the thing this screen is entirely made of.
        for widget in WIDGETS {
            assert!(
                !widget.summary().trim().is_empty(),
                "{} has no summary line",
                widget.stem
            );
            assert!(
                !widget.about().trim().is_empty(),
                "{} has no paragraph under its summary",
                widget.stem
            );
        }
    }

    #[test]
    fn the_aside_stops_before_the_usage_block() {
        // start.py takes the paragraph under the summary and nothing more:
        // what follows is the synopsis and the key list, which belong to
        // --help rather than to somebody choosing a widget.
        for widget in WIDGETS {
            let about = widget.about();
            assert!(
                !about.contains("Keys:"),
                "{} carried its key list into the aside",
                widget.stem
            );
            assert!(about.chars().count() <= 400, "{} ran long", widget.stem);
        }
    }

    #[test]
    fn a_sample_is_a_picture_of_the_widget() {
        // Every doc page opens with a rendering, and the rendering opens
        // with the same rule every widget draws across its top. A fenced
        // block that does not is a shell snippet or a config listing.
        let mut with_pictures = 0;
        for widget in WIDGETS {
            let sample = widget.sample();
            if sample.is_empty() {
                continue;
            }
            with_pictures += 1;
            assert!(sample[0].starts_with("╺━"), "{} is not a preview", widget.stem);
        }
        assert!(
            with_pictures == WIDGETS.len(),
            "only {} of {} widgets have a preview",
            with_pictures,
            WIDGETS.len()
        );
    }

    #[test]
    fn a_paragraph_breaks_at_spaces() {
        assert_eq!(wrap("one two three", 7), vec!["one two", "three"]);
        // A word longer than the line is cut rather than dropped.
        assert_eq!(wrap("abcdefghij", 4), vec!["abcd", "efgh", "ij"]);
        // Three lines at most: this is a note, not the doc page.
        assert_eq!(wrap(&"word ".repeat(60), 10).len(), 3);
        assert!(wrap("", 8).is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn a_child_killed_by_signal_is_not_success() {
        // `kill -s TERM $$` exits by signal, so `code()` is None. The
        // previous fallback turned that into 0, which is how a crashed
        // widget became a successful launch.
        let status = std::process::Command::new("sh")
            .args(["-c", "kill -s TERM $$"])
            .status()
            .expect("spawn sh");
        assert!(
            status.code().is_none(),
            "expected a signal death, got {:?}",
            status.code()
        );
        assert_eq!(child_exit(status), 128 + 15);
    }

    #[test]
    fn the_list_is_in_a_settled_order() {
        // Alphabetical, as start.py's sorted glob produces - so the row a
        // key lands on does not move between builds.
        let names: Vec<&str> = WIDGETS.iter().map(|w| w.stem).collect();
        let mut sorted = names.clone();
        sorted.sort();
        assert_eq!(names, sorted);
    }
}
