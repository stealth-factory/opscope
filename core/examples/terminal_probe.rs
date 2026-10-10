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
        let plots: Vec<_> = if hidden {
            Vec::new()
        } else {
            tc::Plot::new(3, 2, w.saturating_sub(6), 6, chart)
                .in_viewport(scroll, 1, h.saturating_sub(2))
                .into_iter()
                .collect()
        };
        tc::draw_plots(&rows, w, h, &plots);
        std::thread::sleep(Duration::from_millis(25));
    }
}
