//! Deterministic protocol fixture, not a widget or a source of operational data.
//! Run via core/tests/terminal_protocol.py; no real terminal or credentials needed.
use opscope_core as tc;
use std::time::Duration;

fn main() {
    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let mut keys = Vec::new();
    let mut hidden = false;
    let mut scroll = 0;
    let mut gallery = false;
    let mut moving_meter = false;
    let mut tick = 0_u64;
    let mut selected = 0_usize;
    loop {
        for key in keyboard.poll() {
            match key.as_str() {
                "q" => {
                    keyboard.restore();
                    tc::restore_screen();
                    println!("KEYS:{}", keys.join(","));
                    return;
                }
                "c" => hidden = !hidden,
                "down" => {
                    scroll = 2;
                    selected += 1;
                }
                "up" => {
                    scroll = 0;
                    selected = selected.saturating_sub(1);
                }
                "g" => gallery = !gallery,
                "m" => moving_meter = !moving_meter,
                "n" => {
                    tc::notify("Pomodoro", "Focus elapsed");
                    tc::notify("Pomodoro", "Focus elapsed, 0:01:00 over");
                }
                "h" => {
                    keyboard.restore();
                    tc::restore_screen();
                    // Stay up until SIGTERM. The printed pid is the sleep
                    // after `exec`, and sleep does not read the tty, so the
                    // test can queue a stale cell report and only then let
                    // the parent reclaim. The long sleep is a backstop if
                    // that signal never arrives.
                    std::process::Command::new("/bin/sh")
                        .args(["-c", "printf 'CHILD_READY %s\\n' \"$$\"; exec sleep 30"])
                        .status()
                        .expect("run handoff fixture");
                    keyboard.reclaim();
                    tc::claim_screen();
                }
                "p" => panic!("intentional protocol fixture panic"),
                _ => keys.push(key),
            }
        }
        let (w, h) = tc::size();
        let mut rows = vec![
            tc::title(
                "protocol fixture (synthetic test data)",
                w,
                &tc::rgb(180, 210, 220),
            ),
            // The visible label is unchanged. The https OSC 8 is what a
            // terminal that understands links opens; the http target and
            // the OSC 52 are what `inert` has to drop before the row is
            // written.
            format!(
                "{}\x1b]8;;http://opscope.invalid\x1b\\\x1b]8;;\x1b\\\x1b]52;c;ZmFrZQ==\x07\x1b]8;;https://example.com/source-pane\x1b\\ source pane text\x1b]8;;\x1b\\",
                tc::link(
                    "https://example.com/opscope-probe",
                    "  axes and labels remain text",
                )
            ),
        ];
        // Synthetic samples so the PTY test can see underline bytes. The
        // words stay the same whether the terminal answers the query.
        let ink = tc::rgb(225, 235, 245);
        let stale = tc::marked(
            &ink,
            tc::Marks {
                stale: true,
                ..tc::Marks::default()
            },
        );
        let partial = tc::marked(
            &ink,
            tc::Marks {
                partial: true,
                ..tc::Marks::default()
            },
        );
        let over = tc::marked(
            &ink,
            tc::Marks {
                threshold: true,
                ..tc::Marks::default()
            },
        );
        // Row 2 is the chart. These words sit on the first row under it,
        // which the chart does not paint over.
        let samples = tc::seg(
            &[
                (stale.as_str(), " stale".into()),
                (partial.as_str(), " partial".into()),
                (over.as_str(), " over".into()),
            ],
            w.saturating_sub(1),
        );
        // Eight columns, plus the space in front of the sequence. Scale 2
        // needs 16 for the time. A narrow pane keeps the line off the frame.
        // The underline sample owns the first row under the chart, so the
        // sized face starts on the next one. A plot spliced into the sized
        // row would make that row wider than the pane, and the frame would
        // drop the sizing.
        let sample = "12:34:56";
        let scale = tc::text_scale(
            sample.len(),
            w.saturating_sub(3),
            h.saturating_sub(9),
            4,
        );
        let chart = tc::LineChart {
            slots: 12,
            focus: None,
            traces: vec![tc::Trace {
                positions: Some((0..12).map(|i| i as f64 / 11.0).collect()),
                values: vec![
                    Some(0.1),
                    Some(0.6),
                    Some(0.4),
                    None,
                    None,
                    Some(0.2),
                    Some(0.5),
                    Some(0.9),
                    Some(0.6),
                    Some(0.7),
                    Some(0.3),
                    Some(0.5),
                ],
                colour: tc::rgb(50, 220, 170),
                baseline: None,
            }],
        };
        let mut plots: Vec<_> = if hidden {
            Vec::new()
        } else {
            tc::Plot::new(3, 2, w.saturating_sub(6), 6, chart)
                .under_text()
                .in_viewport(scroll, 1, h.saturating_sub(2))
                .into_iter()
                .collect()
        };
        if gallery && !hidden {
            let green = tc::rgb(50, 220, 170);
            let blue = tc::rgb(100, 180, 250);
            let grey = tc::rgb(130, 145, 165);
            let columns: Vec<_> = [0.0, 0.1, 0.4, 1.0, 0.3, 0.0, 0.8]
                .iter()
                .map(|v| (*v, green.clone()))
                .collect();
            let cells = vec![
                vec![
                    None,
                    Some((grey.clone(), 0)),
                    Some((green.clone(), 1)),
                    Some((green.clone(), 2)),
                    Some((green.clone(), 3)),
                    Some((green.clone(), 4))
                ];
                3
            ];
            plots.extend(
                [
                    tc::Plot::bars(3, 9, &columns, 3, 1.0, false),
                    tc::Plot::bars(13, 9, &columns, 3, 1.0, true),
                    tc::Plot::heatmap_levels(3, 14, &cells, &grey),
                    tc::Plot::meter(3, 18, 20, 0.375, Some(0.65), &green, &grey,
                        &format!("{}{}", tc::bg(10, 12, 18), tc::rgb(238, 244, 252))),
                    tc::Plot::stacked_bar(
                        3,
                        13,
                        20,
                        &[(0.375, green.clone()), (0.625, blue.clone())],
                    ),
                    // Fully opaque, so the transport can drop the alpha channel.
                    tc::Plot::heatmap_levels(
                        30,
                        17,
                        &[vec![Some((green.clone(), 4)); 48]],
                        &grey,
                    ),
                ]
                .into_iter()
                .filter_map(|p| p.in_viewport(scroll, 1, h.saturating_sub(2))),
            );
        }
        if moving_meter && !hidden {
            tick += 1;
            plots = vec![tc::Plot::gradient_meter(
                3, 2, w.saturating_sub(6), 0.4, Some(0.501 + tick as f64 * 1e-12),
                &tc::rgb(80, 210, 160), &tc::rgb(70, 80, 90), &ink,
            )];
        }
        rows.resize(h, String::new());
        // Inside the line-chart rectangle. Pixel mode keeps it; Braille
        // paints over it, which is the text-mode chart.
        if h > 3 {
            rows[2] = "   now".into();
        }
        if h > 9 {
            rows[8] = samples;
        }
        // The last row is the selection label. The sized face needs `scale`
        // rows under the underline sample and must not cover that label.
        if scale > 1 && 9 + usize::from(scale) < h {
            rows[9] = format!(
                "{} {}",
                tc::rgb(120, 255, 200),
                tc::sized_text(sample, scale)
            );
            for extra in 1..scale {
                rows[9 + extra as usize] = String::new();
            }
        }
        rows[h - 1] = format!("SELECTED:{selected}");
        tc::draw_plots(&rows, w, h, &plots);
        keyboard.wait(Duration::from_millis(300));
    }
}
