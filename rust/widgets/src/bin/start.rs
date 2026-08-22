// terminal-toys - small dependency-free terminal widgets
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

//! Every widget here, what it does, and whether it will work on this machine.
//!
//! A port of start.py, which reads the directory it sits in and parses each
//! script's docstring. A compiled binary has no directory to read, so the
//! same three things - the summary, the paragraph under it, and the picture
//! from the doc page - are compiled in from the same files the Python
//! parses at run time. Nothing is described twice either way.

use std::time::Duration;

use toys_core as tc;

/// Each widget's own words, taken from the files that already hold them:
/// the help text every binary answers `--help` with, and the doc page that
/// opens with a picture of it.
struct Widget {
    stem: &'static str,
    help: &'static str,
    doc: &'static str,
}

const WIDGETS: &[Widget] = &[
    Widget {
        stem: "clocks",
        help: include_str!("clocks_help.txt"),
        doc: include_str!("../../../../docs/clocks.md"),
    },
    Widget {
        stem: "deployments",
        help: include_str!("deployments_help.txt"),
        doc: include_str!("../../../../docs/deployments.md"),
    },
    Widget {
        stem: "herdr-panes",
        help: include_str!("herdr-panes_help.txt"),
        doc: include_str!("../../../../docs/herdr-panes.md"),
    },
    Widget {
        stem: "latency",
        help: include_str!("latency_help.txt"),
        doc: include_str!("../../../../docs/latency.md"),
    },
    Widget {
        stem: "linear",
        help: include_str!("linear_help.txt"),
        doc: include_str!("../../../../docs/linear.md"),
    },
    Widget {
        stem: "link",
        help: include_str!("link_help.txt"),
        doc: include_str!("../../../../docs/link.md"),
    },
    Widget {
        stem: "matrix",
        help: include_str!("matrix_help.txt"),
        // The one widget with no doc page, and the only one that computes
        // nothing: there is no picture of it that a still frame would carry.
        doc: "",
    },
    Widget {
        stem: "netwatch",
        help: include_str!("netwatch_help.txt"),
        doc: include_str!("../../../../docs/netwatch.md"),
    },
    Widget {
        stem: "ports",
        help: include_str!("ports_help.txt"),
        doc: include_str!("../../../../docs/ports.md"),
    },
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
        for line in self.doc.lines() {
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

fn rows_for(w: usize, selected: usize, p: &Palette) -> Vec<String> {
    let name_w = (w.saturating_sub(58)).clamp(12, 18);
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
                    tc::pad(
                        &item.stem.chars().take(name_w - 1).collect::<String>(),
                        name_w,
                    ),
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
    // rather than assuming: cbreak again, cursor away again, screen clear.
    keyboard.reclaim();
    tc::out(&format!("{}{}{}", tc::HIDE, tc::CLEAR, tc::HOME));
    tc::flush();
}

fn main() {
    // A widget name is resolved before --help is looked at, so that
    // `start netwatch --help` is netwatch's help, not this one's. Every
    // argument after the name belongs to the widget, including that one.
    let args: Vec<String> = std::env::args().skip(1).collect();
    if let Some(first) = args.first() {
        if !first.starts_with('-') {
            let wanted = first.strip_suffix(".py").unwrap_or(first);
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
                Ok(s) => s.code().unwrap_or(0),
                Err(e) => {
                    eprintln!("{}: {}", path.display(), e);
                    2
                }
            });
        }
    }

    tc::maybe_help(include_str!("start_help.txt"));
    let p = palette();
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut selected = 0usize;

    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" | "Q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    return;
                }
                "up" | "k" | "K" => selected = selected.saturating_sub(1),
                "down" | "j" | "J" => selected += 1,
                "enter" | "right" | "i" | "I" => {
                    run_widget(&mut keyboard, WIDGETS[selected.min(WIDGETS.len() - 1)].stem)
                }
                _ => {}
            }
        }

        let (w, h) = tc::size();
        if selected >= WIDGETS.len() {
            selected = WIDGETS.len() - 1;
        }

        let mut body = vec![tc::title("terminal toys", w, &p.accent)];
        body.push(tc::seg(
            &[(
                p.dim.as_str(),
                format!(" {} widgets   ↵ starts one, q leaves", WIDGETS.len()),
            )],
            w - 1,
        ));
        body.push(String::new());
        body.extend(rows_for(w, selected, &p));
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

        // And what it looks like. A picture from the docs rather than the
        // widget itself: starting one to look at it would ping hosts, spend
        // API quota and read the whole agent transcript tree, and browsing
        // a menu should cost nothing at all. Measured against the footer
        // that will actually be drawn, rather than a guess at its height.
        let hints: Vec<Vec<(&str, String)>> = vec![
            vec![(p.accent.as_str(), "↑↓".into()), (p.dim.as_str(), " select".into())],
            vec![(p.accent.as_str(), "↵".into()), (p.dim.as_str(), " launch".into())],
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
            // The menu does not list itself.
            if name == "start" {
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
            with_pictures >= WIDGETS.len() - 1,
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
