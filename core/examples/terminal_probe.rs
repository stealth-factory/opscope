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
    let mut selected = 0;
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
                "g" => gallery = !gallery,
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
            "  axes and labels remain text".into(),
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
                        40,
                        17,
                        &[vec![Some((green.clone(), 4)); 4]],
                        &grey,
                    ),
                ]
                .into_iter()
                .filter_map(|p| p.in_viewport(scroll, 1, h.saturating_sub(2))),
            );
        }
        rows.resize(h, String::new());
        if h > 9 {
            rows[8] = samples;
        }
        rows[h - 1] = format!("SELECTED:{selected}");
        tc::draw_plots(&rows, w, h, &plots);
        keyboard.wait(Duration::from_millis(300));
    }
}
