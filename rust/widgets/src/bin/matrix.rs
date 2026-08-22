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

//! Digital rain.
//!
//! The one widget in the collection that computes nothing at all. It just
//! looks good, and it knows it - and it is a fair test of the drawing path,
//! since it repaints every cell of every frame.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use toys_core as tc;

const GLYPHS: &str = "ｱｲｳｴｵｶｷｸｹｺｻｼｽｾｿﾀﾁﾂﾃﾄﾅﾆﾇﾈﾉﾊﾋﾌﾍﾎﾏﾐﾑﾒﾓﾔﾕﾖﾗﾘﾙﾚﾛﾜﾝ0123456789ABCDEF<>*+=$#%&@";

/// A tiny xorshift, because the rain does not need a crate to be random.
///
/// Seeded from the clock, so two panes started together do not fall in
/// lockstep - which they would with a fixed seed, and which looks wrong
/// immediately.
struct Rng(u64);

impl Rng {
    fn new() -> Rng {
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos() as u64)
            .unwrap_or(0x2545F4914F6CDD1D);
        Rng(seed | 1)
    }

    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }

    fn float(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }

    fn range(&mut self, lo: f64, hi: f64) -> f64 {
        lo + self.float() * (hi - lo)
    }

    fn below(&mut self, n: usize) -> usize {
        if n == 0 {
            0
        } else {
            (self.next() % n as u64) as usize
        }
    }
}

struct Drop {
    y: f64,
    speed: f64,
    length: usize,
}

impl Drop {
    fn new(h: usize, rng: &mut Rng) -> Drop {
        Drop {
            y: -rng.range(0.0, h as f64 * 1.5),
            speed: rng.range(0.25, 1.15),
            length: rng.below(std::cmp::max(6, h)) + std::cmp::max(4, h / 5),
        }
    }
}

/// The trail's colour at a given distance from the head, 1 being closest.
fn shade(level: f64) -> String {
    let g = (60.0 + 175.0 * level) as u8;
    tc::rgb((10.0 + 20.0 * level) as u8, g, (30.0 + 50.0 * level) as u8)
}

fn main() {
    tc::maybe_help(include_str!("matrix_help.txt"));
    let head = tc::rgb(210, 255, 225);
    let near = tc::rgb(120, 255, 170);
    let glyphs: Vec<char> = GLYPHS.chars().collect();
    let mut rng = Rng::new();

    tc::setup();
    let mut keyboard = tc::Keyboard::new();
    let (mut w, mut h) = tc::size();
    let mut drops: Vec<Drop> = (0..w).map(|_| Drop::new(h, &mut rng)).collect();
    // Glyphs mutate in place, independently of the drops falling over them,
    // which is what stops the rain reading as a repeating pattern.
    let mut field: Vec<Vec<char>> = (0..h)
        .map(|_| (0..w).map(|_| glyphs[rng.below(glyphs.len())]).collect())
        .collect();

    loop {
        for key in keyboard.poll() {
            if key == "q" || key == "Q" {
                keyboard.restore();
                tc::restore_screen();
                return;
            }
        }
        let (nw, nh) = tc::size();
        if (nw, nh) != (w, h) {
            w = nw;
            h = nh;
            drops = (0..w).map(|_| Drop::new(h, &mut rng)).collect();
            field = (0..h)
                .map(|_| (0..w).map(|_| glyphs[rng.below(glyphs.len())]).collect())
                .collect();
        }

        // A handful of cells change character every frame, wherever they
        // happen to be.
        for _ in 0..(w * h / 40).max(1) {
            let y = rng.below(h);
            let x = rng.below(w);
            field[y][x] = glyphs[rng.below(glyphs.len())];
        }

        let mut rows: Vec<String> = vec![String::new(); h];
        let mut cells: Vec<Vec<(String, char)>> =
            vec![vec![(String::new(), ' '); w]; h];
        for (x, drop) in drops.iter_mut().enumerate() {
            drop.y += drop.speed;
            if drop.y - drop.length as f64 > h as f64 {
                *drop = Drop::new(h, &mut rng);
                drop.y = -(drop.length as f64);
            }
            for back in 0..drop.length {
                let y = drop.y as isize - back as isize;
                if y < 0 || y >= h as isize {
                    continue;
                }
                let level = 1.0 - (back as f64 / drop.length as f64);
                let colour = if back == 0 {
                    head.clone()
                } else if back == 1 {
                    near.clone()
                } else {
                    shade(level)
                };
                cells[y as usize][x] = (colour, field[y as usize][x]);
            }
        }
        for (y, line) in cells.iter().enumerate() {
            let parts: Vec<(&str, String)> = line
                .iter()
                .map(|(colour, ch)| {
                    (
                        colour.as_str(),
                        if colour.is_empty() {
                            " ".to_string()
                        } else {
                            ch.to_string()
                        },
                    )
                })
                .collect();
            rows[y] = tc::seg(&parts, w);
        }
        tc::draw(&rows, w, h);
        std::thread::sleep(Duration::from_millis(55));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trail_fades_from_the_head() {
        // Closer to the head is brighter; the far end is nearly dark.
        let bright = shade(1.0);
        let faint = shade(0.0);
        assert_ne!(bright, faint);
        assert!(bright.contains("235"), "head shade was {:?}", bright);
        assert!(faint.contains("60"), "tail shade was {:?}", faint);
    }

    #[test]
    fn two_instances_do_not_fall_in_lockstep() {
        let mut a = Rng::new();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let mut b = Rng::new();
        let left: Vec<u64> = (0..4).map(|_| a.next()).collect();
        let right: Vec<u64> = (0..4).map(|_| b.next()).collect();
        assert_ne!(left, right, "a fixed seed would sync every pane");
    }

    #[test]
    fn random_values_stay_inside_their_bounds() {
        let mut rng = Rng::new();
        for _ in 0..500 {
            let f = rng.float();
            assert!((0.0..1.0).contains(&f));
            let r = rng.range(0.25, 1.15);
            assert!((0.25..=1.15).contains(&r));
            assert!(rng.below(10) < 10);
        }
        assert_eq!(rng.below(0), 0, "an empty range must not divide by zero");
    }
}
