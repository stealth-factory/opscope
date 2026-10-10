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
                "down" => scroll = 2,
                "g" => gallery = !gallery,
                "p" => panic!("intentional protocol fixture panic"),
                _ => keys.push(key),
            }
        }
        let (w, h) = tc::size();
        let rows = vec![
            tc::title(
                "protocol fixture (synthetic test data)",
                w,
                &tc::rgb(180, 210, 220),
            ),
            "  axes and labels remain text".into(),
        ];
        let chart = tc::LineChart {
            slots: 12,
            focus: None,
            traces: vec![tc::Trace {
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
            let cells = vec![vec![Some(green.clone()), None, Some(blue.clone())]; 3];
            plots.extend(
                [
                    tc::Plot::bars(3, 9, &columns, 3, 1.0, false),
                    tc::Plot::bars(13, 9, &columns, 3, 1.0, true),
                    tc::Plot::heatmap(3, 14, &cells, &grey),
                    tc::Plot::meter(3, 18, 20, 0.375, Some(0.65), &green, &grey, &blue),
                ]
                .into_iter()
                .filter_map(|p| p.in_viewport(scroll, 1, h.saturating_sub(2))),
            );
        }
        tc::draw_plots(&rows, w, h, &plots);
        std::thread::sleep(Duration::from_millis(25));
    }
}
