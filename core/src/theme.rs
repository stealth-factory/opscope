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

//! Colour themes: widgets ask for a role, the theme says what colour it is.
//!
//! A widget never names a colour. It asks for what the colour means -
//! [`Role::Fg`] for text, [`Role::Muted`] for a label, [`Role::Red`] for a
//! failure, [`Role::Slot`] for the selected-row tint - and the theme in
//! force answers. Themes are the sixteen roles the website's themes use, so
//! a theme picked on opscope.stealth-factory.co looks the same here; both
//! read `core/themes.json`.
//!
//! **Contrast is enforced when a theme is loaded, not trusted.** Every text
//! role is moved, just far enough, to AA 4.5 against `bg`, `dark` and
//! `slot` - lightened on a dark theme, darkened on a light one - so a
//! widget can put any text role on any of those backgrounds. That is the
//! rule the site's themes were written to, and it is checked again here so
//! a theme added later, or one built from the terminal's replies, cannot
//! slip under it.
//!
//! Output stays truecolor whatever the theme: Kitty charts recover RGB from
//! the escape, and the contrast arithmetic needs real values.

use std::sync::{Arc, OnceLock, RwLock};

/// A colour, as three bytes.
pub type Rgb = (u8, u8, u8);

/// What a colour is for. The names are the website's.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Role {
    /// The pane's background.
    Bg,
    /// Title and status bars, a step darker than `Bg`.
    Dark,
    /// The selected row's tint.
    Slot,
    /// Text.
    Fg,
    /// Secondary text: labels, units, explanations.
    Muted,
    /// Decoration only - bar tracks, rules, empty cells. Never text.
    Dim,
    /// Borders and dividers.
    Border,
    /// The theme's highlight: the title, the selected marker, links.
    Accent,
    /// Text drawn on an `Accent` background.
    OnAccent,
    Green,
    Blue,
    Yellow,
    Red,
    Cyan,
    Magenta,
    Orange,
}

impl Role {
    pub const ALL: [Role; 16] = [
        Role::Bg,
        Role::Dark,
        Role::Slot,
        Role::Fg,
        Role::Muted,
        Role::Dim,
        Role::Border,
        Role::Accent,
        Role::OnAccent,
        Role::Green,
        Role::Blue,
        Role::Yellow,
        Role::Red,
        Role::Cyan,
        Role::Magenta,
        Role::Orange,
    ];

    /// The key in `themes.json`, the same as the website's.
    pub fn key(self) -> &'static str {
        match self {
            Role::Bg => "bg",
            Role::Dark => "dark",
            Role::Slot => "slot",
            Role::Fg => "fg",
            Role::Muted => "muted",
            Role::Dim => "dim",
            Role::Border => "border",
            Role::Accent => "accent",
            Role::OnAccent => "onAccent",
            Role::Green => "green",
            Role::Blue => "blue",
            Role::Yellow => "yellow",
            Role::Red => "red",
            Role::Cyan => "cyan",
            Role::Magenta => "magenta",
            Role::Orange => "orange",
        }
    }

    /// Whether text is drawn in this role, so it must clear AA 4.5 on the
    /// backgrounds. `Dim` and `Border` are decoration; `OnAccent` is
    /// measured against `Accent` instead.
    pub fn is_text(self) -> bool {
        matches!(
            self,
            Role::Fg
                | Role::Muted
                | Role::Accent
                | Role::Green
                | Role::Blue
                | Role::Yellow
                | Role::Red
                | Role::Cyan
                | Role::Magenta
                | Role::Orange
        )
    }

    fn index(self) -> usize {
        Role::ALL.iter().position(|r| *r == self).unwrap_or(0)
    }
}

/// One theme: its name, where it sits in the picker, and sixteen colours.
#[derive(Clone, Debug, PartialEq)]
pub struct Theme {
    pub id: String,
    pub name: String,
    pub group: String,
    /// The film a hacker-group theme is named after.
    pub film: Option<String>,
    pub light: bool,
    /// Whether the pane is painted in `bg`. A theme with its own text
    /// colours must, or its dark text lands on a dark terminal. opscope
    /// Classic and the terminal's own theme do not: the terminal's
    /// background is already the one they were made for.
    pub paints_background: bool,
    colours: [Rgb; 16],
}

impl Theme {
    pub fn colour(&self, role: Role) -> Rgb {
        self.colours[role.index()]
    }

    /// A theme from sixteen colours, contrast enforced.
    pub fn new(id: &str, name: &str, light: bool, paints_background: bool, colours: [Rgb; 16]) -> Theme {
        let mut theme = Theme {
            id: id.into(),
            name: name.into(),
            group: String::new(),
            film: None,
            light,
            paints_background,
            colours,
        };
        theme.enforce_contrast();
        theme
    }

    /// Move each text role just far enough to clear AA 4.5 on `bg`, `dark`
    /// and `slot`, and `onAccent` on `accent`.
    fn enforce_contrast(&mut self) {
        let grounds = [self.colour(Role::Bg), self.colour(Role::Dark), self.colour(Role::Slot)];
        for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
            let fixed = nudge(self.colour(role), &grounds, self.light);
            self.colours[role.index()] = fixed;
        }
        let on = nudge(self.colour(Role::OnAccent), &[self.colour(Role::Accent)], !self.light);
        self.colours[Role::OnAccent.index()] = on;
    }
}

/// WCAG relative luminance.
pub fn luminance((r, g, b): Rgb) -> f64 {
    let lin = |c: u8| {
        let c = c as f64 / 255.0;
        if c <= 0.03928 { c / 12.92 } else { ((c + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
}

/// WCAG contrast ratio, 1 to 21.
pub fn contrast(a: Rgb, b: Rgb) -> f64 {
    let (x, y) = (luminance(a), luminance(b));
    let (hi, lo) = if x > y { (x, y) } else { (y, x) };
    (hi + 0.05) / (lo + 0.05)
}

/// `a` moved `t` of the way to `b`.
pub fn mix(a: Rgb, b: Rgb, t: f64) -> Rgb {
    let t = t.clamp(0.0, 1.0);
    let m = |x: u8, y: u8| (x as f64 + (y as f64 - x as f64) * t).round() as u8;
    (m(a.0, b.0), m(a.1, b.1), m(a.2, b.2))
}

/// The AA ratio every text role keeps.
pub const AA: f64 = 4.5;

/// `colour`, moved toward white (or black on a light theme) in small steps
/// until it clears [`AA`] on every one of `grounds`. Unmoved when it
/// already does: a theme's own colours are kept wherever they are readable.
pub fn nudge(colour: Rgb, grounds: &[Rgb], light: bool) -> Rgb {
    let toward = if light { (0, 0, 0) } else { (255, 255, 255) };
    // A hair over AA, so a check computing luminance with the other
    // published threshold (0.04045 for 0.03928) agrees at the boundary.
    let clears = |c: Rgb| grounds.iter().all(|g| contrast(c, *g) >= AA + 0.02);
    if clears(colour) {
        return colour;
    }
    for step in 1..=100 {
        let c = mix(colour, toward, step as f64 / 100.0);
        if clears(c) {
            return c;
        }
    }
    toward
}

fn parse_hex(text: &str) -> Option<Rgb> {
    let hex = text.strip_prefix('#')?;
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let byte = |i: usize| u8::from_str_radix(&hex[i..i + 2], 16).ok();
    Some((byte(0)?, byte(2)?, byte(4)?))
}

/// One theme from its JSON object, the shape `themes.json` uses.
pub fn parse_theme(value: &serde_json::Value) -> Result<Theme, String> {
    let text = |k: &str| value.get(k).and_then(|v| v.as_str()).map(str::to_string);
    let id = text("id").filter(|s| !s.is_empty()).ok_or("a theme with no id")?;
    let mut colours = [(0, 0, 0); 16];
    for role in Role::ALL {
        let raw = text(role.key()).ok_or_else(|| format!("{id}: no {}", role.key()))?;
        colours[role.index()] =
            parse_hex(&raw).ok_or_else(|| format!("{id}: {} is {raw:?}, not #rrggbb", role.key()))?;
    }
    let group = text("group").unwrap_or_default();
    let light = value
        .get("light")
        .and_then(|v| v.as_bool())
        .unwrap_or(group == "light");
    let paints = value.get("paintsBackground").and_then(|v| v.as_bool()).unwrap_or(true);
    let mut theme = Theme::new(&id, &text("name").unwrap_or_else(|| id.clone()), light, paints, colours);
    theme.group = group;
    theme.film = text("film");
    Ok(theme)
}

/// The themes file the binary and the website share.
pub const THEMES_JSON: &str = include_str!("../themes.json");

/// Every built-in theme, in the order the picker shows them.
pub fn builtin() -> &'static [Theme] {
    static THEMES: OnceLock<Vec<Theme>> = OnceLock::new();
    THEMES.get_or_init(|| {
        let file: serde_json::Value = serde_json::from_str(THEMES_JSON).unwrap_or_default();
        file["themes"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|t| parse_theme(t).ok())
            .collect()
    })
}

/// The picker's groups, in order: (id, label).
pub fn groups() -> Vec<(String, String)> {
    let file: serde_json::Value = serde_json::from_str(THEMES_JSON).unwrap_or_default();
    file["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            let s = |k: &str| g[k].as_str().unwrap_or_default().to_string();
            (s("id"), s("label"))
        })
        .collect()
}

/// A built-in theme by id.
pub fn find(id: &str) -> Option<&'static Theme> {
    builtin().iter().find(|t| t.id == id)
}

/// The theme `auto` falls back to when the terminal says nothing about its
/// own colours: opscope Dusk, as on the website.
pub const FALLBACK: &str = "dusk";

/// What a `theme` setting asks for.
#[derive(Clone, Debug, PartialEq)]
pub enum Wanted {
    /// The terminal's theme when it answered, else [`FALLBACK`].
    Auto,
    /// The terminal's own colours, whatever they are.
    Terminal,
    /// A named theme.
    Named(String),
}

/// A `theme` value, as a person would type it.
pub fn parse_wanted(text: &str) -> Wanted {
    match text.trim().to_ascii_lowercase().as_str() {
        "" | "auto" => Wanted::Auto,
        "terminal" => Wanted::Terminal,
        id => Wanted::Named(id.to_string()),
    }
}

/// The theme to draw with, from what was asked for and what the terminal
/// said about itself. Pure: `terminal` is the theme built from the
/// terminal's replies when it gave any.
pub fn choose(wanted: &Wanted, terminal: Option<&Theme>) -> Result<Theme, String> {
    let fallback = || find(FALLBACK).cloned().expect("opscope Dusk is built in");
    match wanted {
        Wanted::Auto => Ok(terminal.cloned().unwrap_or_else(fallback)),
        Wanted::Terminal => Ok(terminal.cloned().unwrap_or_else(|| {
            find("classic").cloned().expect("opscope Classic is built in")
        })),
        Wanted::Named(id) => match find(id) {
            Some(theme) => Ok(theme.clone()),
            None => Err(format!("theme {id:?} is not one opscope knows - drawing {}", fallback().name)),
        },
    }
}

/// The theme in force. Resolved on first use from `OPSCOPE_THEME`, else a
/// top-level `theme` in config.json, else `auto`.
fn active() -> &'static RwLock<Arc<Theme>> {
    static ACTIVE: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();
    ACTIVE.get_or_init(|| RwLock::new(Arc::new(resolve())))
}

/// What the person asked for, before the terminal has answered anything.
pub fn wanted() -> Wanted {
    if let Ok(env) = std::env::var("OPSCOPE_THEME") {
        if !env.trim().is_empty() {
            return parse_wanted(&env);
        }
    }
    let file = crate::config_paths()
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .find_map(|t| serde_json::from_str::<serde_json::Value>(&t).ok());
    match file.as_ref().and_then(|f| f.get("theme")).and_then(|v| v.as_str()) {
        Some(text) => parse_wanted(text),
        None => Wanted::Auto,
    }
}

fn resolve() -> Theme {
    // Tests compare frames byte for byte: they draw in Classic, which
    // paints no background, whatever the machine running them has set.
    if cfg!(test) {
        return find("classic").cloned().expect("opscope Classic is built in");
    }
    let wanted = wanted();
    match choose(&wanted, crate::terminal::terminal_theme().as_ref()) {
        Ok(theme) => theme,
        Err(why) => {
            crate::notices::when(true, || crate::notices::Notice::warn("theme-unknown", why.clone()));
            find(FALLBACK).cloned().expect("opscope Dusk is built in")
        }
    }
}

/// The theme in force now.
pub fn current() -> Arc<Theme> {
    active().read().unwrap_or_else(|e| e.into_inner()).clone()
}

/// Put `theme` in force, for the terminal's reply arriving or changing.
pub fn set(theme: Theme) {
    *active().write().unwrap_or_else(|e| e.into_inner()) = Arc::new(theme);
}

/// Work out the theme again: after the terminal answered, or changed.
pub fn refresh() {
    set(resolve());
}

/// A role's colour in the theme in force.
pub fn colour(role: Role) -> Rgb {
    current().colour(role)
}

/// Text in `role`: a truecolor foreground escape.
pub fn ink(role: Role) -> String {
    let (r, g, b) = colour(role);
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

/// A background in `role`: the selected-row tint is `tint(Role::Slot)`.
/// Needs a reset afterwards, like every background.
pub fn tint(role: Role) -> String {
    let (r, g, b) = colour(role);
    format!("\x1b[48;2;{};{};{}m", r, g, b)
}

/// Text part-way between two roles, still readable: a shade the sixteen do
/// not name, such as a softer label. Nudged like the roles themselves when
/// `readable` is set.
pub fn ink_between(a: Role, b: Role, t: f64, readable: bool) -> String {
    let theme = current();
    let mut c = mix(theme.colour(a), theme.colour(b), t);
    if readable {
        let grounds = [theme.colour(Role::Bg), theme.colour(Role::Dark), theme.colour(Role::Slot)];
        c = nudge(c, &grounds, theme.light);
    }
    format!("\x1b[38;2;{};{};{}m", c.0, c.1, c.2)
}

/// The escape that puts the pane background back, for a theme that paints
/// one; empty for one that leaves the terminal's own.
pub fn background() -> String {
    let theme = current();
    if theme.paints_background {
        let (r, g, b) = theme.colour(Role::Bg);
        format!("\x1b[48;2;{};{};{}m", r, g, b)
    } else {
        String::new()
    }
}

/// The temperature ramp from this theme: green, through yellow, to red.
/// `tinted` keeps every step readable on the selected-row tint as well.
pub fn heat_colour(frac: f64, tinted: bool) -> Rgb {
    heat_in(&current(), frac, tinted)
}

/// [`heat_colour`] in a given theme.
pub fn heat_in(theme: &Theme, frac: f64, tinted: bool) -> Rgb {
    let frac = frac.clamp(0.0, 1.0);
    let (green, yellow, red) = (theme.colour(Role::Green), theme.colour(Role::Yellow), theme.colour(Role::Red));
    let c = if frac < 0.5 { mix(green, yellow, frac / 0.5) } else { mix(yellow, red, (frac - 0.5) / 0.5) };
    let mut grounds = vec![theme.colour(Role::Bg)];
    if tinted {
        grounds.push(theme.colour(Role::Slot));
    }
    nudge(c, &grounds, theme.light)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_built_in_theme_loads_with_all_sixteen_roles() {
        let file: serde_json::Value = serde_json::from_str(THEMES_JSON).unwrap();
        let listed = file["themes"].as_array().unwrap().len();
        assert_eq!(builtin().len(), listed, "a theme in themes.json did not parse");
        assert!(listed >= 51, "the site's 50 and opscope Classic");
        for id in ["dusk", "classic", "github-light", "dracula", "solarized-light"] {
            assert!(find(id).is_some(), "{id}");
        }
        let groups: Vec<String> = groups().into_iter().map(|(id, _)| id).collect();
        for t in builtin() {
            assert!(groups.contains(&t.group), "{} is in group {:?}, which the picker has no section for", t.id, t.group);
        }
    }

    #[test]
    fn every_text_role_of_every_theme_clears_aa_on_every_background() {
        for t in builtin() {
            for ground in [Role::Bg, Role::Dark, Role::Slot] {
                for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
                    let ratio = contrast(t.colour(role), t.colour(ground));
                    assert!(ratio >= AA, "{}: {:?} on {:?} is {ratio:.2}", t.id, role, ground);
                }
            }
            assert!(contrast(t.colour(Role::OnAccent), t.colour(Role::Accent)) >= AA, "{}: onAccent", t.id);
        }
    }

    #[test]
    fn the_heat_ramp_is_readable_at_every_step_in_every_theme() {
        for t in builtin() {
            for step in 0..=20 {
                let frac = step as f64 / 20.0;
                let c = heat_in(t, frac, true);
                for ground in [Role::Bg, Role::Slot] {
                    let ratio = contrast(c, t.colour(ground));
                    assert!(ratio >= AA, "{} heat({frac}) on {:?} is {ratio:.2}", t.id, ground);
                }
            }
        }
    }

    #[test]
    fn a_readable_colour_is_left_alone_and_an_unreadable_one_moves_just_enough() {
        let black = (0, 0, 0);
        assert_eq!(nudge((225, 235, 245), &[black], false), (225, 235, 245));
        let dark_blue = (20, 40, 120);
        let moved = nudge(dark_blue, &[black], false);
        assert!(contrast(moved, black) >= AA);
        assert!(contrast(moved, black) < AA + 1.0, "not pushed past what it needs: {moved:?}");
        // On a light theme it darkens instead.
        let white = (255, 255, 255);
        let pale = nudge((200, 200, 120), &[white], true);
        assert!(contrast(pale, white) >= AA && luminance(pale) < luminance((200, 200, 120)));
    }

    #[test]
    fn what_a_theme_setting_asks_for() {
        assert_eq!(parse_wanted(""), Wanted::Auto);
        assert_eq!(parse_wanted(" AUTO "), Wanted::Auto);
        assert_eq!(parse_wanted("terminal"), Wanted::Terminal);
        assert_eq!(parse_wanted("Dracula"), Wanted::Named("dracula".into()));
        // auto with no reply from the terminal is Dusk; with one, the terminal's.
        assert_eq!(choose(&Wanted::Auto, None).unwrap().id, "dusk");
        let mine = Theme::new("terminal", "the terminal", false, false, find("dracula").unwrap().colours);
        assert_eq!(choose(&Wanted::Auto, Some(&mine)).unwrap().id, "terminal");
        assert_eq!(choose(&Wanted::Named("nord".into()), None).unwrap().id, "nord");
        let why = choose(&Wanted::Named("nope".into()), None).unwrap_err();
        assert!(why.contains("nope") && why.contains("opscope Dusk"), "{why}");
    }

    #[test]
    fn classic_does_not_paint_and_dusk_does() {
        assert!(!find("classic").unwrap().paints_background);
        assert!(find("dusk").unwrap().paints_background);
        assert!(find("github-light").unwrap().light);
    }
}
