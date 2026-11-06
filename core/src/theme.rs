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
//! **Contrast is enforced when a theme is loaded, not trusted.** Each text
//! role comes in two strengths. [`ink`] is moved, just far enough, to AA
//! 4.5 on `bg` and `dark` - lightened on a dark theme, darkened on a light
//! one. [`ink_on`] with `tinted` set is lifted further, to AA 4.5 on the
//! selected-row tint and every hue tint as well, which is what a row that
//! is selected or tinted by meaning draws its text in. Two strengths and
//! not one because one that cleared every tint would make a theme's own red
//! a pale pink everywhere, to be readable on the one row that needs it -
//! the same trade `heat_on` makes for the ramp, made for every role. That
//! is the rule the site's themes were written to, checked again here so a
//! theme added later, or one built from the terminal's replies, cannot slip
//! under it.
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
    /// Each role lifted to read on the selected-row tint and every hue
    /// tint too; the same as `colours` for a role that is not text.
    lifted: [Rgb; 16],
    /// The selected-row tint moved toward each hue, for a row tinted by
    /// what it is - a blocked agent, a finished one - rather than by being
    /// selected. Worked out from the theme's own hues before contrast is
    /// enforced, and enforced against like `bg`.
    toward: [Rgb; 7],
}

/// The hues a row can be tinted toward.
pub const HUES: [Role; 7] = [Role::Red, Role::Green, Role::Yellow, Role::Blue, Role::Cyan, Role::Magenta, Role::Orange];

/// How far a hue tint moves from the selected-row tint toward its hue:
/// enough to read as that colour, little enough to stay a background.
pub const TOWARD: f64 = 0.18;

impl Theme {
    pub fn colour(&self, role: Role) -> Rgb {
        self.colours[role.index()]
    }

    /// A role's colour for text on a tinted row: readable on the selected-row
    /// tint and every hue tint.
    pub fn lifted(&self, role: Role) -> Rgb {
        self.lifted[role.index()]
    }

    /// The selected-row tint moved toward `hue`; the plain selected-row
    /// tint for a role that is not one of [`HUES`].
    pub fn toward(&self, hue: Role) -> Rgb {
        match HUES.iter().position(|h| *h == hue) {
            Some(i) => self.toward[i],
            None => self.colour(Role::Slot),
        }
    }

    /// A theme from sixteen colours, contrast enforced.
    pub fn new(id: &str, name: &str, light: bool, paints_background: bool, colours: [Rgb; 16]) -> Theme {
        let slot = colours[Role::Slot.index()];
        let toward = HUES.map(|hue| mix(slot, colours[hue.index()], TOWARD));
        let mut theme = Theme {
            id: id.into(),
            name: name.into(),
            group: String::new(),
            film: None,
            light,
            paints_background,
            colours,
            lifted: colours,
            toward,
        };
        theme.enforce_contrast();
        theme
    }

    /// Move each text role just far enough to clear AA 4.5 on `bg`, `dark`
    /// and `slot`, and `onAccent` on `accent`.
    fn enforce_contrast(&mut self) {
        let plain = [self.colour(Role::Bg), self.colour(Role::Dark)];
        let mut tinted = vec![self.colour(Role::Slot)];
        tinted.extend(self.toward);
        for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
            let fixed = nudge(self.colour(role), &plain, self.light);
            self.colours[role.index()] = fixed;
            self.lifted[role.index()] = nudge(fixed, &tinted, self.light);
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

/// The picker's groups, in order: (id, label). Your own themes come last.
pub fn groups() -> Vec<(String, String)> {
    let file: serde_json::Value = serde_json::from_str(THEMES_JSON).unwrap_or_default();
    let mut out: Vec<(String, String)> = file["groups"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|g| {
            let s = |k: &str| g[k].as_str().unwrap_or_default().to_string();
            (s("id"), s("label"))
        })
        .collect();
    out.push(("user".into(), "your themes".into()));
    out
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
        Wanted::Named(id) => match lookup(id) {
            Some(theme) => Ok(theme),
            None => Err(format!(
                "theme {id:?} is not one opscope knows - using the default, auto ({})",
                terminal.map(|t| t.name.clone()).unwrap_or_else(|| fallback().name)
            )),
        },
    }
}

/// The theme in force. Resolved on first use from `OPSCOPE_THEME`, else a
/// top-level `theme` in config.json, else `auto`.
fn active() -> &'static RwLock<Arc<Theme>> {
    static ACTIVE: OnceLock<RwLock<Arc<Theme>>> = OnceLock::new();
    ACTIVE.get_or_init(|| {
        let first = Arc::new(resolve());
        let _ = BUILT.set(first.clone());
        RwLock::new(first)
    })
}

/// The theme in force when a widget built its palette: the first one
/// resolved. Widgets build their colours once; [`recolour`] draws them in
/// whatever theme is in force now.
static BUILT: OnceLock<Arc<Theme>> = OnceLock::new();

fn built() -> Arc<Theme> {
    let _ = active();
    BUILT.get().cloned().unwrap_or_else(current)
}

fn fg((r, g, b): Rgb) -> String {
    format!("\x1b[38;2;{r};{g};{b}m")
}

fn bgc((r, g, b): Rgb) -> String {
    format!("\x1b[48;2;{r};{g};{b}m")
}

fn ulc((r, g, b): Rgb) -> String {
    format!("\x1b[58;2;{r};{g};{b}m")
}

/// Every escape `from` can produce, to the same role's escape in `to`:
/// text, lifted text, backgrounds, hue tints and the threshold underline.
/// Where two roles share a colour in `from`, the plain form of the first
/// in [`Role::ALL`] wins - the same choice [`lift`] makes.
pub fn escape_map(from: &Theme, to: &Theme) -> std::collections::HashMap<String, String> {
    let mut map = std::collections::HashMap::new();
    for role in Role::ALL {
        map.entry(fg(from.colour(role))).or_insert_with(|| fg(to.colour(role)));
        map.entry(bgc(from.colour(role))).or_insert_with(|| bgc(to.colour(role)));
    }
    for role in Role::ALL {
        map.entry(fg(from.lifted(role))).or_insert_with(|| fg(to.lifted(role)));
        map.entry(ulc(from.lifted(role))).or_insert_with(|| ulc(to.lifted(role)));
    }
    for hue in HUES {
        map.entry(bgc(from.toward(hue))).or_insert_with(|| bgc(to.toward(hue)));
    }
    map.retain(|k, v| k != v);
    map
}

/// `text` drawn in the theme in force now, though it was built in the theme
/// a widget started in: every role colour of the one becomes the same role
/// of the other. How a running widget follows a theme change, and how the
/// picker previews one, without any widget building its colours again. A
/// colour that is no role's - a mix, a ramp step - is left as it is; the
/// ramp is worked out each frame from the theme in force anyway.
pub fn recolour(text: &str) -> String {
    let (from, to) = (built(), current());
    if Arc::ptr_eq(&from, &to) || (from.colours == to.colours && from.toward == to.toward && from.lifted == to.lifted) {
        return text.to_string();
    }
    if !text.contains("\x1b[") {
        return text.to_string();
    }
    static CACHE: std::sync::Mutex<Option<(usize, usize, std::collections::HashMap<String, String>)>> =
        std::sync::Mutex::new(None);
    let key = (Arc::as_ptr(&from) as usize, Arc::as_ptr(&to) as usize);
    let mut cache = CACHE.lock().unwrap_or_else(|e| e.into_inner());
    if cache.as_ref().is_none_or(|(a, b, _)| (*a, *b) != key) {
        *cache = Some((key.0, key.1, escape_map(&from, &to)));
    }
    let map = &cache.as_ref().expect("just filled").2;
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(at) = rest.find("\x1b[") {
        out.push_str(&rest[..at]);
        let tail = &rest[at..];
        match tail.find('m') {
            Some(end) if end < 24 => {
                let seq = &tail[..=end];
                out.push_str(map.get(seq).map(String::as_str).unwrap_or(seq));
                rest = &tail[end + 1..];
            }
            _ => {
                out.push_str("\x1b[");
                rest = &tail[2..];
            }
        }
    }
    out.push_str(rest);
    out
}

/// Look at the theme setting again - at most every two seconds - and put
/// a changed one in force. Every widget calls this as it draws, so a theme
/// picked in one pane, or written into config.json by hand, reaches every
/// other pane on its next frame. True when the theme changed.
pub fn watch() -> bool {
    static LAST: std::sync::Mutex<Option<(std::time::Instant, Wanted)>> = std::sync::Mutex::new(None);
    if cfg!(test) {
        return false;
    }
    let now = std::time::Instant::now();
    let mut last = LAST.lock().unwrap_or_else(|e| e.into_inner());
    if last.as_ref().is_some_and(|(at, _)| now.duration_since(*at) < std::time::Duration::from_secs(2)) {
        return false;
    }
    let asked = wanted();
    let changed = last.as_ref().is_some_and(|(_, was)| *was != asked);
    *last = Some((now, asked));
    drop(last);
    if changed {
        let next = resolve();
        let same = current().colours == next.colours && current().id == next.id;
        set(next);
        return !same;
    }
    false
}

/// Keep `choice` as the theme for every widget: `opscope.theme` in the main
/// config file, the one the settings screen writes, keeping the rest of the
/// file as it is. Every running widget picks it up within two seconds.
pub fn save(choice: &str) -> Result<(), String> {
    let path = crate::config_paths()
        .into_iter()
        .find(|p| p.is_file())
        .unwrap_or_else(crate::default_config_path);
    let raw = std::fs::read_to_string(&path).unwrap_or_default();
    let base = if raw.trim().is_empty() { "{}" } else { raw.as_str() };
    let next = crate::settings::set_json_path(base, &[SECTION, "theme"], &serde_json::Value::String(choice.into()))?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    crate::statefile::write_atomic_if(&path, &next, &raw)
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
    // `opscope.theme` is what the settings screen writes; a top-level
    // `theme` is the shorter spelling for a config written by hand.
    let set = file.as_ref().and_then(|f| {
        f.get(SECTION)
            .and_then(|s| s.get("theme"))
            .or_else(|| f.get("theme"))
            .and_then(|v| v.as_str())
            .map(str::to_string)
    });
    match set {
        Some(text) => parse_wanted(&text),
        None => Wanted::Auto,
    }
}

/// The config section every widget shares: where the theme is kept.
pub const SECTION: &str = "opscope";

/// The shared section's settings schema, in the shape a widget's
/// `settings.json` has, for the settings screen to show beside the widget's
/// own. Built here rather than shipped as a file because its choices are
/// every theme there is, user themes included.
pub fn settings_schema() -> String {
    let mut choices = vec!["auto".to_string(), "terminal".to_string()];
    choices.extend(all().iter().map(|t| t.id.clone()));
    serde_json::json!({
        "_comment": "Settings every widget shares.",
        "_schema": { "theme": { "choices": choices } },
        "_theme_comment": format!(
            "Colours for every widget. auto uses the terminal's own colours when it says what they are, and opscope Dusk when it does not. terminal always uses the terminal's own. classic is opscope's colours from before themes. The rest are the {} themes of opscope.stealth-factory.co and any in {}. OPSCOPE_THEME overrides this for one run.",
            builtin().len(),
            themes_dir().display()
        ),
        "theme": "auto",
    })
    .to_string()
}

/// Every theme a person can pick: the built-in ones, with a user theme of
/// the same id in place of the built-in, then the user's others.
pub fn all() -> Vec<Theme> {
    let (mine, _) = user_themes();
    let mut out: Vec<Theme> = builtin()
        .iter()
        .map(|t| mine.iter().find(|m| m.id == t.id).cloned().unwrap_or_else(|| t.clone()))
        .collect();
    out.extend(mine.iter().filter(|m| find(&m.id).is_none()).cloned());
    out
}

/// A theme by id, the user's own first.
pub fn lookup(id: &str) -> Option<Theme> {
    all().into_iter().find(|t| t.id == id)
}

/// Where a person's own themes go: `opscope/themes/` beside config.json,
/// one theme a file, the file name as its id.
pub fn themes_dir() -> std::path::PathBuf {
    let xdg = std::env::var("XDG_CONFIG_HOME").ok().filter(|s| !s.is_empty());
    let home = std::env::var("HOME").unwrap_or_default();
    std::path::PathBuf::from(xdg.unwrap_or(format!("{home}/.config"))).join("opscope/themes")
}

/// One user theme file. `fg` and `bg` are required; every other role is
/// optional and, when absent, worked out from those two and the hues the
/// file does give, as the terminal's own theme is. Contrast is enforced
/// like every theme's, so a hand-made colour that does not read is moved
/// until it does.
pub fn parse_user_theme(id: &str, text: &str) -> Result<Theme, String> {
    let value: serde_json::Value = serde_json::from_str(text).map_err(|e| format!("is not JSON ({e})"))?;
    if !value.is_object() {
        return Err("is not a JSON object".into());
    }
    let colour = |role: Role| -> Result<Option<Rgb>, String> {
        match value.get(role.key()) {
            None => Ok(None),
            Some(v) => {
                let raw = v.as_str().ok_or_else(|| format!("{} is not a string", role.key()))?;
                parse_hex(raw).map(Some).ok_or_else(|| format!("{} is {raw:?}, not #rrggbb", role.key()))
            }
        }
    };
    let fg = colour(Role::Fg)?.ok_or("has no fg")?;
    let bg = colour(Role::Bg)?.ok_or("has no bg")?;
    let mut palette = [None; 16];
    for (n, role) in [(1, Role::Red), (2, Role::Green), (3, Role::Yellow), (4, Role::Blue), (5, Role::Magenta), (6, Role::Cyan)] {
        palette[n] = colour(role)?;
    }
    let derived = crate::termtheme::theme_from(fg, bg, &palette);
    let mut colours = [(0, 0, 0); 16];
    for role in Role::ALL {
        colours[role.index()] = colour(role)?.unwrap_or_else(|| derived.colour(role));
    }
    let light = value.get("light").and_then(|v| v.as_bool()).unwrap_or(derived.light);
    let paints = value.get("paintsBackground").and_then(|v| v.as_bool()).unwrap_or(true);
    let name = value.get("name").and_then(|v| v.as_str()).unwrap_or(id).to_string();
    let mut theme = Theme::new(id, &name, light, paints, colours);
    theme.group = "user".into();
    Ok(theme)
}

/// The person's own themes, and a line for each file that could not be
/// read as one. Read once.
pub fn user_themes() -> &'static (Vec<Theme>, Vec<String>) {
    static MINE: OnceLock<(Vec<Theme>, Vec<String>)> = OnceLock::new();
    MINE.get_or_init(|| {
        if cfg!(test) {
            return (Vec::new(), Vec::new());
        }
        read_user_themes(&themes_dir())
    })
}

/// [`user_themes`] from `dir`.
pub fn read_user_themes(dir: &std::path::Path) -> (Vec<Theme>, Vec<String>) {
    let (mut found, mut wrong) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else {
        return (found, wrong);
    };
    let mut paths: Vec<std::path::PathBuf> = entries
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.extension().and_then(|e| e.to_str()) == Some("json"))
        .collect();
    paths.sort();
    for path in paths {
        let id = path.file_stem().and_then(|s| s.to_str()).unwrap_or_default().to_ascii_lowercase();
        let file = path.file_name().and_then(|s| s.to_str()).unwrap_or_default().to_string();
        match std::fs::read_to_string(&path).map_err(|e| format!("cannot be read ({e})")).and_then(|t| parse_user_theme(&id, &t)) {
            Ok(theme) => found.push(theme),
            Err(why) => wrong.push(format!("theme file {file} {why} - it is left out")),
        }
    }
    (found, wrong)
}

fn resolve() -> Theme {
    // Tests compare frames byte for byte: they draw in Classic, which
    // paints no background, whatever the machine running them has set.
    if cfg!(test) {
        return find("classic").cloned().expect("opscope Classic is built in");
    }
    let (_, wrong) = user_themes();
    crate::notices::when(!wrong.is_empty(), || {
        crate::notices::Notice::warn("theme-file-bad", wrong.join("; "))
    });
    let wanted = wanted();
    let terminal = match wanted {
        Wanted::Named(_) => None,
        Wanted::Auto | Wanted::Terminal => crate::terminal::terminal_theme(),
    };
    match choose(&wanted, terminal.as_ref()) {
        Ok(theme) => theme,
        // A name opscope does not know is the default, auto, and a notice
        // says which name it was.
        Err(why) => {
            crate::notices::when(true, || crate::notices::Notice::warn("theme-unknown", why.clone()));
            choose(&Wanted::Auto, terminal.as_ref()).expect("auto always has a theme")
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

/// Text in `role` on a row that may be tinted: [`ink`] when it is not, and
/// the lifted form, readable on the selected-row tint and every hue tint,
/// when it is. The call every selected or tinted row makes.
pub fn ink_on(role: Role, tinted: bool) -> String {
    let theme = current();
    let (r, g, b) = if tinted { theme.lifted(role) } else { theme.colour(role) };
    format!("\x1b[38;2;{};{};{}m", r, g, b)
}

/// `colour` as it is drawn on a tinted row: a text role's [`ink`] becomes
/// that role's lifted form, and anything that is not a role's ink - the
/// heat ramp, which lifts itself through `heat_on`, or a mixed shade - is
/// left as it is. For a widget that builds its row colours once and puts
/// them on a selected or tinted row later.
pub fn lift(colour: &str) -> String {
    let theme = current();
    Role::ALL
        .into_iter()
        .filter(|r| r.is_text())
        .find(|r| {
            let (red, g, b) = theme.colour(*r);
            format!("\x1b[38;2;{};{};{}m", red, g, b) == colour
        })
        .map(|r| ink_on(r, true))
        .unwrap_or_else(|| colour.to_string())
}

/// A row tinted toward `hue` - [`Role::Red`] for a blocked agent,
/// [`Role::Green`] for a finished one. Text on it is drawn with
/// [`ink_on`] `(role, true)`, as on the selected-row tint. Needs a reset
/// afterwards.
pub fn tint_toward(hue: Role) -> String {
    let (r, g, b) = current().toward(hue);
    format!("\x1b[48;2;{};{};{}m", r, g, b)
}

/// Text part-way between two roles: a shade the sixteen do not name, such
/// as a softer label. When `readable` is set it is nudged like the roles
/// themselves, to AA 4.5 on the background and the bars.
pub fn ink_between(a: Role, b: Role, t: f64, readable: bool) -> String {
    let theme = current();
    let mut c = mix(theme.colour(a), theme.colour(b), t);
    if readable {
        c = nudge(c, &[theme.colour(Role::Bg), theme.colour(Role::Dark)], theme.light);
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
        grounds.extend(theme.toward);
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
            for ground in [Role::Bg, Role::Dark] {
                for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
                    let ratio = contrast(t.colour(role), t.colour(ground));
                    assert!(ratio >= AA, "{}: {:?} on {:?} is {ratio:.2}", t.id, role, ground);
                    let ratio = contrast(t.lifted(role), t.colour(ground));
                    assert!(ratio >= AA, "{}: lifted {:?} on {:?} is {ratio:.2}", t.id, role, ground);
                }
            }
            for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
                let ratio = contrast(t.lifted(role), t.colour(Role::Slot));
                assert!(ratio >= AA, "{}: lifted {:?} on the selected-row tint is {ratio:.2}", t.id, role);
            }
            assert!(contrast(t.colour(Role::OnAccent), t.colour(Role::Accent)) >= AA, "{}: onAccent", t.id);
        }
    }

    #[test]
    fn every_text_role_reads_on_every_hue_tinted_row_in_every_theme() {
        for t in builtin() {
            for hue in HUES {
                let ground = t.toward(hue);
                for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
                    let ratio = contrast(t.lifted(role), ground);
                    assert!(ratio >= AA, "{}: lifted {:?} on a row tinted toward {:?} is {ratio:.2}", t.id, role, hue);
                }
            }
            // And the tint is a background, not the hue: it stays nearer
            // the selected-row tint than the hue itself.
            let slot = t.colour(Role::Slot);
            let red = t.toward(Role::Red);
            assert!(contrast(red, slot) < contrast(t.colour(Role::Red), slot), "{}", t.id);
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
                for hue in HUES {
                    let ratio = contrast(c, t.toward(hue));
                    assert!(ratio >= AA, "{} heat({frac}) on the {:?} row tint is {ratio:.2}", t.id, hue);
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
        assert!(why.contains("nope") && why.contains("default, auto") && why.contains("opscope Dusk"), "{why}");
    }

    #[test]
    fn classic_is_todays_colours_on_the_plain_background() {
        // Only the lifted forms move: on a plain row opscope Classic draws
        // what the widgets drew before themes existed.
        let classic = find("classic").unwrap();
        for (role, today) in [
            (Role::Fg, (225, 235, 245)),
            (Role::Muted, (130, 165, 200)),
            (Role::Accent, (150, 210, 255)),
            (Role::Green, (90, 240, 160)),
            (Role::Yellow, (255, 200, 90)),
            (Role::Red, (255, 100, 110)),
            (Role::Cyan, (30, 220, 180)),
        ] {
            assert_eq!(classic.colour(role), today, "{role:?}");
        }
    }

    #[test]
    fn a_user_theme_needs_only_fg_and_bg_and_a_bad_file_is_named() {
        let dir = std::env::temp_dir().join(format!("opscope-themes-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("Mine.json"), r##"{"name": "Mine", "fg": "#e0e0e0", "bg": "#101418", "red": "#ff0000"}"##).unwrap();
        std::fs::write(dir.join("broken.json"), "{ not json").unwrap();
        std::fs::write(dir.join("nofg.json"), r##"{"bg": "#000000"}"##).unwrap();
        std::fs::write(dir.join("notes.txt"), "ignored").unwrap();
        let (found, wrong) = read_user_themes(&dir);
        std::fs::remove_dir_all(&dir).ok();
        assert_eq!(found.len(), 1);
        let mine = &found[0];
        assert_eq!((mine.id.as_str(), mine.name.as_str(), mine.group.as_str()), ("mine", "Mine", "user"));
        assert!(mine.paints_background && !mine.light);
        assert_eq!(mine.colour(Role::Bg), (16, 20, 24));
        // Every role is there, readable, whether the file gave it or not.
        for role in Role::ALL.into_iter().filter(|r| r.is_text()) {
            assert!(contrast(mine.colour(role), mine.colour(Role::Bg)) >= AA, "{role:?}");
        }
        assert_eq!(wrong.len(), 2, "{wrong:?}");
        assert!(wrong.iter().any(|w| w.contains("broken.json") && w.contains("not JSON")));
        assert!(wrong.iter().any(|w| w.contains("nofg.json") && w.contains("has no fg")));
        // A folder that is not there is no themes, and no complaint.
        assert_eq!(read_user_themes(std::path::Path::new("/nonexistent/opscope/themes")), (Vec::new(), Vec::new()));
    }

    #[test]
    fn the_shared_section_offers_every_theme() {
        let schema: serde_json::Value = serde_json::from_str(&settings_schema()).unwrap();
        let choices = schema["_schema"]["theme"]["choices"].as_array().unwrap();
        assert_eq!(choices[0], "auto");
        assert_eq!(choices[1], "terminal");
        assert_eq!(choices.len(), 2 + all().len());
        assert_eq!(schema["theme"], "auto");
        assert!(schema["_theme_comment"].as_str().unwrap().contains("opscope Dusk"));
    }

    #[test]
    fn lift_turns_a_role_into_its_lifted_form_and_leaves_the_rest() {
        assert_eq!(lift(&ink(Role::Red)), ink_on(Role::Red, true));
        assert_eq!(lift(&ink(Role::Muted)), ink_on(Role::Muted, true));
        let shade = "\x1b[38;2;1;2;3m";
        assert_eq!(lift(shade), shade);
    }

    #[test]
    fn a_frame_built_in_one_theme_is_drawn_in_another_role_for_role() {
        let (classic, nord) = (find("classic").unwrap(), find("nord").unwrap());
        let map = escape_map(classic, nord);
        let row = format!("{}ok {}x{} {}", fg(classic.colour(Role::Red)), bgc(classic.colour(Role::Slot)), fg(classic.lifted(Role::Muted)), bgc(classic.toward(Role::Green)));
        let mut out = row.clone();
        for (from, to) in &map {
            out = out.replace(from.as_str(), to.as_str());
        }
        assert!(out.contains(&fg(nord.colour(Role::Red))));
        assert!(out.contains(&bgc(nord.colour(Role::Slot))));
        assert!(out.contains(&fg(nord.lifted(Role::Muted))));
        assert!(out.contains(&bgc(nord.toward(Role::Green))));
        // A theme to itself changes nothing.
        assert!(escape_map(nord, nord).is_empty());
    }

    #[test]
    fn classic_does_not_paint_and_dusk_does() {
        assert!(!find("classic").unwrap().paints_background);
        assert!(find("dusk").unwrap().paints_background);
        assert!(find("github-light").unwrap().light);
    }
}
