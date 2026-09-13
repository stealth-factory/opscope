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
    widget!("luvus-panes"),
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
///
/// No row cap: the body is a window onto this note, so a paragraph
/// that needs more than three lines has to keep wrapping or the rest
/// can never be scrolled to.
fn wrap(text: &str, width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut rest: Vec<char> = text.trim().chars().collect();
    while !rest.is_empty() {
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

/// Where the window onto the body sits after a frame's worth of input.
///
/// The same shape as `github-prs`, and for the same reason: the wheel
/// writes `at` and nothing else, and this hands it straight back, because
/// scrolling to look at something must never change what `↵` opens. Only
/// on the frame a key moved the selection - `chase` - does the window
/// follow the cursor; a view that re-centred every frame dragged itself
/// back from wherever the wheel had just put it.
///
/// Its own function so the composition can be tested. `tc::follow` is
/// already tested; what has been wrong is when it is called.
fn scrolled(at: usize, cursor: Option<usize>, chase: bool, body: usize, room: usize) -> usize {
    let at = match cursor.filter(|_| chase) {
        Some(row) => tc::follow(at, row, room),
        None => at,
    };
    // Clamped last, and written back by the caller: without that a wheel
    // spun past the end leaves a scroll nobody can see, and the same
    // number of wheel-ups to undo.
    at.min(body.saturating_sub(room))
}

/// The pinned top row: the name, and the version that is actually running.
///
/// The version sits after the rule as its own segment rather than inside
/// the title text, because `tc::title` upper-cases what it is given and
/// `V0.17.0` is not a version anybody writes. As a segment it takes the
/// dim colour while the name keeps the accent, and the title is built to
/// the width the version leaves so the row still measures exactly `w`.
///
/// Below that, the version goes rather than being cut: half of `v0.17.0`
/// is worse than no version at all, and a build number that might be
/// missing a digit is a build number nobody can act on.
fn title_row(w: usize, p: &Palette) -> String {
    let tag = format!(" v{}", tc::version_number());
    let cells = tc::display_width(&tag);
    // `╺━ OPSCOPE ╸` is twelve cells, and two of rule either side of the
    // name is the least that still reads as a title bar.
    if w >= 14 + cells {
        let mut row = tc::title("opscope", w - cells, &p.accent);
        row.push_str(&tc::seg(&[(p.dim.as_str(), tag)], cells));
        row
    } else {
        tc::title("opscope", w, &p.accent)
    }
}

/// The list row the cursor is on, counting from the top of the body.
///
/// The count line and the blank under it come first.
const LIST_TOP: usize = 2;

/// Everything under the title, built at whatever height it needs.
///
/// Not given `h` on purpose. Every part of this used to be sized to what
/// the pane had left - the list to `h - 8`, the description to one line or
/// three, the example to whatever remained - so a short pane hid the
/// description and most of the list, and the wheel could not move the
/// chrome out of the way. A window onto this is what scrolls now.
fn body_rows(w: usize, selected: usize, p: &Palette) -> Vec<String> {
    let mut body = vec![tc::seg(
        &[(
            p.dim.as_str(),
            format!(" {} widgets   ↵ or → starts one, q leaves", WIDGETS.len()),
        )],
        w - 1,
    )];
    body.push(String::new());
    body.extend(rows_for(w, selected, p));
    body.push(String::new());

    // What the highlighted one is for, in its own words - the rest of
    // its opening paragraph, which the row has no room for. Not the
    // command to run it: that is this screen's job, not the reader's.
    let pick = &WIDGETS[selected.min(WIDGETS.len() - 1)];
    body.push(tc::seg(
        &[(
            p.lbl.as_str(),
            format!(" ── {} ── ", pick.stem.to_uppercase()),
        )],
        w - 1,
    ));
    for line in wrap(&pick.about(), w.saturating_sub(4)) {
        body.push(tc::seg(&[(p.dim.as_str(), format!("  {}", line))], w - 1));
    }

    // And what it looks like. A picture from its README rather than the
    // widget itself: starting one to look at it would ping hosts, spend
    // API quota and read the whole agent transcript tree, and browsing
    // a menu should cost nothing at all.
    let shown = pick.sample();
    if !shown.is_empty() && w >= 44 {
        let rule = "─".repeat(w.saturating_sub(15).max(1));
        body.push(tc::seg(
            &[
                (p.grid.as_str(), " ┌── ".into()),
                (p.dim.as_str(), "example".into()),
                (p.grid.as_str(), format!(" {}┐", rule)),
            ],
            w - 1,
        ));
        for line in shown {
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
    body
}

fn rows_for(w: usize, selected: usize, p: &Palette) -> Vec<String> {
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

        // The footer is pinned and measured first: it is what the body has
        // to fit above, and a guess at its height put the last row of the
        // list under it.
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

        // A window onto the body rather than a cut of it, with the title
        // pinned above it: scrolled away, the screen stops saying what it
        // is. Everything else - the count line, the list, the description
        // and the example - moves together, so a ten-row pane scrolls its
        // chrome out of the way instead of leaving one row of list under
        // eight rows of everything else.
        let body = body_rows(w, selected, &p);
        let room = h.saturating_sub(foot.len());
        let room_below = room.saturating_sub(1).max(1);
        scroll = scrolled(scroll, Some(LIST_TOP + selected), moved, body.len(), room_below);
        moved = false;

        let mut frame = vec![title_row(w, &p)];
        frame.extend(body.iter().skip(scroll).take(room_below).cloned());
        while frame.len() < room {
            frame.push(String::new());
        }
        frame.extend(foot);
        frame.truncate(h);
        tc::draw(&frame, w, h);
        std::thread::sleep(Duration::from_millis(150));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A row as it reaches the terminal, without the colour escapes.
    fn plain(row: &str) -> String {
        let mut out = String::new();
        let mut chars = row.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

    #[test]
    fn the_title_row_carries_the_running_version_and_still_measures_the_pane() {
        // The version is what a stale npx cache cannot lie about, so it has
        // to be the stamp rather than a number typed here - and `tc::title`
        // fills to width, so hanging a segment off it without taking those
        // cells out of the title is a row wider than the pane, which wraps
        // and scrolls the pinned title off the top.
        let p = palette();
        let tag = format!("v{}", tc::version_number());
        assert!(
            tc::version().contains(&tc::version_number().to_string()),
            "the title would disagree with --version"
        );
        let mut stood_down = 0;
        for w in 20usize..=160 {
            let row = plain(&title_row(w, &p));
            assert_eq!(tc::display_width(&row), w, "the title row is not {w} wide");
            assert!(row.contains("OPSCOPE"), "the title lost its name at {w}");
            if row.contains(&tag[..2]) {
                // Present in full or not at all: `v0.1` is a version that
                // was never released.
                assert!(row.contains(&tag), "the version was cut at {w}: {row:?}");
            } else {
                stood_down += 1;
            }
        }
        assert!(stood_down > 0, "the version never stood down on a narrow pane");
        // Where it stands down, rather than merely that it does: the title
        // itself wants twelve cells and two of rule, and the version takes
        // the rest.
        let edge = 14 + tc::display_width(&format!(" {tag}"));
        assert!(plain(&title_row(edge, &p)).contains(&tag), "no version at {edge}");
        assert!(
            !plain(&title_row(edge - 1, &p)).contains(&tag[..2]),
            "a version at {} , which cannot hold it",
            edge - 1
        );
        assert!(plain(&title_row(80, &p)).contains(&tag), "no version on a wide pane");
    }

    #[test]
    fn the_wheel_moves_the_view_and_only_a_key_brings_it_back() {
        // The rule the launcher was breaking: the wheel slides the viewport
        // and `selected` stays exactly where it is, even off screen, so
        // scrolling to look at something never changes what enter opens.
        let (body, room) = (60usize, 10usize);
        // A wheel-moved view is handed straight back, cursor or no cursor.
        assert_eq!(scrolled(7, Some(0), false, body, room), 7);
        // And is not dragged back on the next frame either.
        assert_eq!(scrolled(7, Some(0), false, body, room), 7);
        // On the frame a key moved the selection, the window follows it.
        assert_eq!(scrolled(0, Some(40), true, body, room), 31);
        assert_eq!(scrolled(31, Some(40), false, body, room), 31);
        // Spun past the end it stops with the last row on screen.
        assert_eq!(scrolled(9_999, None, false, body, room), body - room);
        // A body that fits has nowhere to go.
        assert_eq!(scrolled(5, None, false, 8, room), 0);
    }

    #[test]
    fn the_body_is_built_whole_whatever_the_pane_can_show() {
        // Every part of this used to be sized to the rows left over, so a
        // short pane showed one row of list under eight of chrome and the
        // wheel could not reach any of it. The body knows nothing about
        // height now: the list is whole, the description is under it, and
        // the example is under that.
        let p = palette();
        let body = body_rows(90, 0, &p);
        let rows: Vec<String> = body.iter().map(|r| plain(r)).collect();
        for widget in WIDGETS {
            assert!(
                rows.iter().any(|row| row.contains(widget.stem)),
                "{} is not in the body",
                widget.stem
            );
        }
        assert!(
            rows.iter().any(|row| row.contains("── AGENT-USAGE ──")),
            "the description of the selected widget is not in the body"
        );
        // The paragraph used to stop at three lines, which hid the rest
        // of agent-usage at eighty columns. Scrolling cannot reach what
        // was never built.
        assert!(
            rows.iter().any(|row| row.contains("plausible zero")),
            "the end of the selected paragraph is not in the body"
        );
        assert!(
            rows.iter().any(|row| row.contains("example")),
            "the preview is not in the body"
        );
        // Taller than any short pane, which is the point: there is
        // something for the wheel to move.
        assert!(
            body.len() > WIDGETS.len() + 4,
            "the body is only {} rows",
            body.len()
        );
        // The cursor's row in the body, which is what the follow is given.
        assert!(
            plain(&body[LIST_TOP]).contains(WIDGETS[0].stem),
            "the first list row is not where the follow thinks it is"
        );
        let later = body_rows(90, 3, &p);
        assert!(
            plain(&later[LIST_TOP + 3]).contains(WIDGETS[3].stem),
            "row {} of the body is not the selected widget",
            LIST_TOP + 3
        );
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
        let rows = rows_for(86, 0, &p);
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
        // The body is a window onto the whole note, so a long paragraph
        // keeps wrapping rather than stopping at three.
        assert!(wrap(&"word ".repeat(60), 10).len() > 3);
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
