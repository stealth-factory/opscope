// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Notices: warnings and errors that are not about a number on screen.
//!
//! The rule this module exists to keep: **a warning that explains a missing
//! or partial number stays in the body**, beside the number. Everything else
//! - a config file others can read, an old section name still doing the
//! work, a recommended tool that is not installed - had nowhere honest to
//! go. It was glued onto a fetch error with `·`, which made a working pane
//! read as a failing one, or it was dropped on purpose, which made the pane
//! quietly less than it could be.
//!
//! So a notice is raised once, by id, from any thread, and core shows it in
//! three places nobody has to wire twice:
//!
//! - a **banner** of at most two wrapped lines above the footer, for sixty
//!   seconds after a notice first appears or its content changes;
//! - a **footer hint**, `[!] 1 error, 2 warnings`, coloured by the worst
//!   level, for as long as anything is outstanding - so a banner that has
//!   timed out collapses into a counter rather than vanishing while the
//!   notice is still true;
//! - the **`!` screen**, which core draws, to read each notice whole and
//!   dismiss it for this session, or bring a dismissed one back.
//!
//! Nothing here can panic or return an error. A poisoned lock is recovered
//! rather than propagated, because the registry is written from poller
//! threads and a pane that died over a warning would be the failure this
//! whole tree spends its checks avoiding.
//!
//! Dismissals are session-only for now. They are keyed and hashed the way a
//! persisted dismissal will need to be - `core/<id>` or `<widget>/<id>`,
//! with the content hash it was taken against - so dismissing for N days is
//! a new `until` on [`Dismissal`] and a file, not a new shape.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// How long a new or changed notice banners before it collapses into the
/// footer counter. Fixed rather than a setting: it is long enough to be read
/// on a pane somebody glanced at, and short enough that a pane left on a
/// wall is not permanently a row shorter.
pub const BANNER_SECS: f64 = 60.0;

/// The most notices the banner shows at once. More than two and the banner
/// is a second body; the rest are one keypress away on the `!` screen.
pub const BANNER_MAX: usize = 2;

/// How bad it is. Ordered so that `max()` is the worst.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Level {
    Info,
    Warn,
    Error,
}

impl Level {
    /// The word in the screen's LEVEL column.
    pub fn word(self) -> &'static str {
        match self {
            Level::Info => "note",
            Level::Warn => "warn",
            Level::Error => "error",
        }
    }

    /// `1 note`, `2 warnings`: the footer counter's wording.
    fn count(self, n: usize) -> String {
        let noun = match self {
            Level::Info => "note",
            Level::Warn => "warning",
            Level::Error => "error",
        };
        format!("{n} {noun}{}", if n == 1 { "" } else { "s" })
    }

    /// The colour on the terminal background. All three clear AA on black;
    /// the tests measure them.
    fn colour(self) -> &'static str {
        match self {
            Level::Info => INFO,
            Level::Warn => WARN,
            Level::Error => BAD,
        }
    }

    /// The colour on the selected-row tint. The shared red measures 4.05 on
    /// `bg(38,56,76)`, under AA, so a selected error is drawn lifted.
    fn colour_on_tint(self) -> &'static str {
        match self {
            Level::Error => BAD_LIT,
            other => other.colour(),
        }
    }
}

/// Whose fact it is.
///
/// `Core` is the same fact in every pane - the config file's permissions -
/// and is raised only from core; `check.rs` holds widgets to that. `Widget`
/// is this widget's own.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Scope {
    Core,
    Widget,
}

/// What a live notice is found by: its scope and its id, so a widget's
/// notice and a Core notice that happen to share an id stay two notices,
/// exactly as their dismissals are already keyed apart.
type Key = (Scope, &'static str);

/// One thing worth saying that is not about a number on screen.
///
/// `id` is a literal or a const, stable across runs, and unique within the
/// widget that raises it: raising the same id again updates the notice in
/// place rather than adding a second one, and the id is what a dismissal is
/// keyed by.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub id: &'static str,
    pub level: Level,
    pub scope: Scope,
    /// One line, said first: what is wrong.
    pub title: String,
    /// The rest: why it matters and what to do about it. May be empty.
    pub detail: String,
    /// Where it came from, in the SOURCE column - `config`, `tools`, a
    /// section name. Empty means the widget itself.
    pub source: &'static str,
}

impl Notice {
    fn new(id: &'static str, level: Level, title: impl Into<String>) -> Notice {
        Notice {
            id,
            level,
            scope: Scope::Widget,
            title: title.into(),
            detail: String::new(),
            source: "",
        }
    }

    /// Something worth knowing that is not wrong.
    pub fn info(id: &'static str, title: impl Into<String>) -> Notice {
        Notice::new(id, Level::Info, title)
    }

    /// Something that should be fixed, and is not stopping the widget.
    pub fn warn(id: &'static str, title: impl Into<String>) -> Notice {
        Notice::new(id, Level::Warn, title)
    }

    /// Something that is wrong. Dismissible for this session only.
    pub fn error(id: &'static str, title: impl Into<String>) -> Notice {
        Notice::new(id, Level::Error, title)
    }

    /// The rest of what there is to say, and the remedy.
    pub fn detail(mut self, detail: impl Into<String>) -> Notice {
        self.detail = detail.into();
        self
    }

    /// The SOURCE column.
    pub fn source(mut self, source: &'static str) -> Notice {
        self.source = source;
        self
    }

    /// Mark as the same fact in every pane. Core only.
    pub(crate) fn core(mut self) -> Notice {
        self.scope = Scope::Core;
        self
    }

    fn key(&self) -> Key {
        (self.scope, self.id)
    }

    /// FNV-1a over the title and detail.
    ///
    /// Spelled out rather than taken from `DefaultHasher`, whose algorithm
    /// the standard library does not promise to keep: a persisted dismissal
    /// compares this against a value written by an older build, and a hash
    /// that moved under it would bring back every dismissed notice at once.
    fn hash(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        for byte in self
            .title
            .bytes()
            .chain(std::iter::once(0u8))
            .chain(self.detail.bytes())
        {
            h ^= byte as u64;
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
        h
    }
}

/// A dismissal, and the content it was taken against.
///
/// A notice whose title or detail changes after it was dismissed shows
/// again: a *changed* problem is a new one. `until` is `None` for this
/// session, which is the only kind there is yet.
#[derive(Clone, Debug, PartialEq)]
struct Dismissal {
    hash: u64,
    at: f64,
    until: Option<f64>,
}

/// A notice as the registry holds it.
#[derive(Clone, Debug)]
struct Live {
    notice: Notice,
    hash: u64,
    /// When this id was first raised, for SINCE.
    first: f64,
    /// When it last started a banner: its first raise or a content change.
    armed: f64,
    /// Raise order, for newest-first where two arm in the same instant.
    seq: u64,
}

#[derive(Default)]
struct Registry {
    widget: &'static str,
    live: Vec<Live>,
    dismissed: BTreeMap<String, Dismissal>,
    seq: u64,
}

impl Registry {
    fn key(&self, notice: &Notice) -> String {
        match notice.scope {
            Scope::Core => format!("core/{}", notice.id),
            Scope::Widget => format!("{}/{}", self.widget_name(), notice.id),
        }
    }

    fn widget_name(&self) -> &'static str {
        if self.widget.is_empty() {
            "opscope"
        } else {
            self.widget
        }
    }

    fn raise(&mut self, notice: Notice, now: f64) {
        let hash = notice.hash();
        self.seq += 1;
        let seq = self.seq;
        let key = notice.key();
        if let Some(live) = self.live.iter_mut().find(|l| l.notice.key() == key) {
            if live.hash != hash {
                live.hash = hash;
                live.armed = now;
                live.seq = seq;
            }
            live.notice = notice;
            return;
        }
        self.live.push(Live {
            notice,
            hash,
            first: now,
            armed: now,
            seq,
        });
    }

    fn clear(&mut self, key: Key) {
        self.live.retain(|l| l.notice.key() != key);
    }

    fn is_dismissed(&self, live: &Live, now: f64) -> bool {
        self.dismissed
            .get(&self.key(&live.notice))
            .is_some_and(|d| d.hash == live.hash && d.until.is_none_or(|u| u > now))
    }

    /// Worst first, then newest.
    fn sorted<'a>(&self, mut list: Vec<&'a Live>) -> Vec<&'a Live> {
        list.sort_by(|a, b| {
            b.notice
                .level
                .cmp(&a.notice.level)
                .then(b.seq.cmp(&a.seq))
        });
        list
    }

    fn outstanding(&self, now: f64) -> Vec<&Live> {
        self.sorted(
            self.live
                .iter()
                .filter(|l| !self.is_dismissed(l, now))
                .collect(),
        )
    }

    fn dismissed_live(&self, now: f64) -> Vec<&Live> {
        self.sorted(
            self.live
                .iter()
                .filter(|l| self.is_dismissed(l, now))
                .collect(),
        )
    }

    fn banner(&self, now: f64) -> Vec<&Live> {
        let mut list: Vec<&Live> = self
            .outstanding(now)
            .into_iter()
            .filter(|l| now - l.armed < BANNER_SECS)
            .collect();
        list.sort_by(|a, b| b.seq.cmp(&a.seq));
        list.truncate(BANNER_MAX);
        list
    }

    fn dismiss(&mut self, key: Key, now: f64) {
        let Some(live) = self.live.iter().find(|l| l.notice.key() == key) else {
            return;
        };
        let key = self.key(&live.notice);
        let hash = live.hash;
        self.dismissed.insert(
            key,
            Dismissal {
                hash,
                at: now,
                until: None,
            },
        );
    }

    fn dismiss_all(&mut self, now: f64) {
        let keys: Vec<Key> = self.outstanding(now).iter().map(|l| l.notice.key()).collect();
        for key in keys {
            self.dismiss(key, now);
        }
    }

    fn restore(&mut self, key: Key) {
        let Some(live) = self.live.iter().find(|l| l.notice.key() == key) else {
            return;
        };
        let key = self.key(&live.notice);
        self.dismissed.remove(&key);
    }

    fn dismissed_at(&self, live: &Live) -> Option<f64> {
        self.dismissed.get(&self.key(&live.notice)).map(|d| d.at)
    }
}

static REGISTRY: OnceLock<Mutex<Registry>> = OnceLock::new();

/// Run `f` on the registry, recovering a poisoned lock rather than
/// propagating it. Every public function goes through here.
fn with<R>(f: impl FnOnce(&mut Registry) -> R) -> R {
    let lock = REGISTRY.get_or_init(|| Mutex::new(Registry::default()));
    let mut guard = match lock.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    };
    f(&mut guard)
}

/// Name the widget, once, at the top of `main`.
///
/// The name titles the `!` screen and keys this widget's dismissals apart
/// from another widget's notice with the same id.
pub fn start(widget: &'static str) {
    with(|r| r.widget = widget);
}

/// Raise a notice, or update it in place if its id is already raised.
///
/// Safe from any thread. A changed title or detail banners again and voids
/// a dismissal; the same content raised again changes nothing, so a poller
/// can raise on every pass.
pub fn raise(notice: Notice) {
    let now = crate::now();
    with(|r| r.raise(notice, now));
}

/// The condition went away. Clearing an id that is not raised is a no-op.
///
/// Clears this widget's notice by that id; a Core notice sharing the id is
/// a different notice and is left alone.
pub fn clear(id: &'static str) {
    with(|r| r.clear((Scope::Widget, id)));
}

/// Raise when `cond` holds and clear when it does not - the usual shape,
/// called on every pass of whatever finds out.
///
/// The notice is built on both branches: on a clear pass it is built only
/// to learn which id and scope to clear, then dropped. Taking the id
/// separately would save that sentence, at the price of a second copy of
/// the id that could drift from the one inside the notice - and a notice
/// raised under one id and cleared under another never goes away. A pass
/// runs once per poll, so the sentence is the cheaper of the two.
pub fn when(cond: bool, notice: impl FnOnce() -> Notice) {
    let notice = notice();
    if cond {
        raise(notice);
    } else {
        let key = notice.key();
        with(|r| r.clear(key));
    }
}

/// The colours. All measured in the tests, on black and on the tint.
const BAD: &str = "\x1b[38;2;255;100;110m";
const BAD_LIT: &str = "\x1b[38;2;255;130;140m";
const WARN: &str = "\x1b[38;2;255;200;90m";
const INFO: &str = "\x1b[38;2;110;210;235m";
const TXT: &str = "\x1b[38;2;225;235;245m";
const LBL: &str = "\x1b[38;2;130;165;200m";
const DIM: &str = "\x1b[38;2;127;147;172m";
const DIM_LIT: &str = "\x1b[38;2;140;170;195m";
const ACCENT: &str = "\x1b[38;2;150;210;255m";
const TINT: (u8, u8, u8) = (38, 56, 76);

/// The footer hint, or `None` when nothing is outstanding.
///
/// One hint, so `pack_hints` wraps it whole: `[!] 1 error, 2 warnings`,
/// worst level first, zero levels left out, in the worst level's colour.
/// Push it beside `[,] settings`:
///
/// ```ignore
/// hints.extend(tc::notices::hint(w - 2));
/// ```
///
/// `hint_key` reads the `[!]`, so it is clickable through `footer_at` with
/// nothing more done.
///
/// `room` is the width the footer is packed to. A hint is never split, so
/// one wider than the pane would overflow it; below the full wording's
/// width it says `[!] 4 notices`, and below that `[!] 4`.
pub fn hint(room: usize) -> Option<Vec<(&'static str, String)>> {
    let now = crate::now();
    recheck_config_permissions(now);
    with(|r| hint_of(r, now, room))
}

fn hint_of(r: &Registry, now: f64, room: usize) -> Option<Vec<(&'static str, String)>> {
    let outstanding = r.outstanding(now);
    let worst = outstanding.iter().map(|l| l.notice.level).max()?;
    let parts: Vec<String> = [Level::Error, Level::Warn, Level::Info]
        .into_iter()
        .filter_map(|level| {
            let n = outstanding.iter().filter(|l| l.notice.level == level).count();
            (n > 0).then(|| level.count(n))
        })
        .collect();
    let total = outstanding.len();
    let full = format!("[!] {}", parts.join(", "));
    let short = format!("[!] {total} notice{}", if total == 1 { "" } else { "s" });
    let text = if crate::display_width(&full) <= room {
        full
    } else if crate::display_width(&short) <= room {
        short
    } else {
        format!("[!] {total}")
    };
    Some(vec![(worst.colour(), text)])
}

/// The banner: 0 to 2 notices, newest first, each its title alone wrapped
/// to `w` by [`crate::error_rows`] and never clipped.
///
/// Draw these directly above the footer, and count them in the rows the
/// footer takes, so a body that scrolls does not scroll under them. Not
/// injected by `draw()`: that would move the footer out from under the
/// placements `footer_at` registered.
///
/// `h` is the pane's height. The banner takes at most a quarter of it,
/// because it is pinned: only the body scrolls, so a banner as tall as the
/// pane pushes the footer - the `[!]` counter and `[q]uit` with it - below
/// the bottom edge for the minute it shows, and nothing the reader can do
/// brings it back. So the title only, never the detail, which waits on the
/// `!` screen; and a notice whose title does not fit whole in what is left
/// is not shown rather than cut. The counter still counts it, so leaving
/// it out hides nothing.
pub fn banner_rows(w: usize, h: usize) -> Vec<String> {
    let now = crate::now();
    recheck_config_permissions(now);
    with(|r| banner_of(r, now, w, h))
}

/// The most rows the banner may take on a pane `h` tall.
fn banner_budget(h: usize) -> usize {
    h / 4
}

fn banner_of(r: &Registry, now: f64, w: usize, h: usize) -> Vec<String> {
    let budget = banner_budget(h);
    let mut rows = Vec::new();
    // Newest first, and whole notices only: once the newest that is left
    // does not fit, an older one is not shown in its place, so what the
    // banner shows is always the latest news.
    for live in r.banner(now) {
        let wrapped = crate::error_rows(live.notice.level.colour(), &live.notice.title, w);
        if rows.len() + wrapped.len() > budget {
            break;
        }
        rows.extend(wrapped);
    }
    rows
}

/// Which list the screen is showing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Outstanding,
    Dismissed,
}

/// The screen's own state, apart from the registry it reads every frame.
struct Screen {
    view: View,
    /// The notice open in detail, by scope and id.
    open: Option<Key>,
    selected: usize,
    scroll: usize,
    /// Where the list was scrolled when a notice was opened, so backing out
    /// of detail returns to it rather than to the detail's own offset.
    list_scroll: usize,
    chase: bool,
    /// What the frame on screen offers a click.
    placed: Vec<(usize, usize)>,
    head: usize,
    shown_scroll: usize,
    /// How many notices the list on screen held, for keys moving the cursor.
    count: usize,
    /// The notices in the order the frame on screen listed them.
    ids: Vec<Key>,
}

impl Screen {
    fn new() -> Screen {
        Screen {
            view: View::Outstanding,
            open: None,
            selected: 0,
            scroll: 0,
            list_scroll: 0,
            chase: true,
            placed: Vec::new(),
            head: 1,
            shown_scroll: 0,
            count: 0,
            ids: Vec::new(),
        }
    }

    fn step(&mut self, delta: isize) {
        if self.count == 0 {
            return;
        }
        let next = (self.selected as isize + delta).clamp(0, self.count as isize - 1);
        self.selected = next as usize;
        self.chase = true;
    }

    fn selected_id(&self) -> Option<Key> {
        self.ids.get(self.selected).copied()
    }

    /// Back from detail to the list where it was, with the selected row
    /// brought into view if anything moved under it meanwhile.
    fn close(&mut self) {
        self.open = None;
        self.scroll = self.list_scroll;
        self.chase = true;
    }
}

/// The `!` screen. Call it from the widget's `"!"` arm, the way `,` calls
/// `run_settings`; it returns when the reader backs out.
///
/// Core draws it so that every widget's notices read and dismiss the same
/// way - the same reason only core draws the settings screen.
pub fn run(keyboard: &mut crate::Keyboard) {
    // Borrowing the caller's keyboard means inheriting its footer
    // placements; this loop polls before it draws, so drop them on the way
    // in and again on the way out, as settings does.
    keyboard.forget_footer();
    let mut screen = Screen::new();
    loop {
        let mut keys = keyboard.poll();
        if let Some(at) = crate::rows_clicked(
            &mut keys,
            Some(screen.selected),
            screen.head,
            screen.shown_scroll,
            &screen.placed,
            None,
        ) {
            if screen.open.is_none() {
                screen.selected = at;
                screen.chase = true;
            }
        }
        let now = crate::now();
        for key in keys {
            if handle(&mut screen, &key, now) {
                keyboard.forget_footer();
                return;
            }
        }
        let (w, h) = crate::size();
        let (rows, footer, foot_top) = with(|r| frame(r, &mut screen, now, w, h));
        crate::draw(&rows, w, h);
        keyboard.footer_at(&footer, foot_top, 1);
        keyboard.wait(Duration::from_millis(80));
    }
}

/// Answer one key. True when the screen should close.
fn handle(screen: &mut Screen, key: &str, now: f64) -> bool {
    if screen.open.is_some() {
        match key {
            "esc" | "!" | "backspace" => screen.close(),
            "q" | "Q" => return true,
            "s" | "S" => {
                if let Some(id) = screen.open {
                    with(|r| r.dismiss(id, now));
                    screen.close();
                }
            }
            "u" | "U" => {
                if let Some(id) = screen.open {
                    with(|r| r.restore(id));
                    screen.close();
                }
            }
            "ctrl-y" | "wheel-up" | "up" | "k" | "K" => {
                screen.scroll = screen.scroll.saturating_sub(1);
            }
            "ctrl-e" | "wheel-down" | "down" | "j" | "J" => {
                screen.scroll = screen.scroll.saturating_add(1);
            }
            _ => {}
        }
        return false;
    }
    match key {
        "esc" | "!" | "q" | "Q" => return true,
        "up" | "k" | "K" => screen.step(-1),
        "down" | "j" | "J" => screen.step(1),
        "pgup" => screen.step(-10),
        "pgdn" => screen.step(10),
        "home" => {
            screen.selected = 0;
            screen.chase = true;
        }
        "end" => {
            screen.selected = screen.count.saturating_sub(1);
            screen.chase = true;
        }
        // The wheel moves the view and nothing else.
        "ctrl-y" | "wheel-up" => screen.scroll = screen.scroll.saturating_sub(1),
        "ctrl-e" | "wheel-down" => screen.scroll = screen.scroll.saturating_add(1),
        "enter" => {
            if let Some(id) = screen.selected_id() {
                screen.open = Some(id);
                screen.list_scroll = screen.scroll;
                screen.scroll = 0;
            }
        }
        "s" | "S" if screen.view == View::Outstanding => {
            if let Some(id) = screen.selected_id() {
                with(|r| r.dismiss(id, now));
                screen.chase = true;
            }
        }
        "a" | "A" if screen.view == View::Outstanding => {
            with(|r| r.dismiss_all(now));
            screen.selected = 0;
            screen.chase = true;
        }
        "u" | "U" if screen.view == View::Dismissed => {
            if let Some(id) = screen.selected_id() {
                with(|r| r.restore(id));
                screen.chase = true;
            }
        }
        "h" | "H" => {
            screen.view = match screen.view {
                View::Outstanding => View::Dismissed,
                View::Dismissed => View::Outstanding,
            };
            screen.selected = 0;
            screen.scroll = 0;
            screen.chase = true;
        }
        _ => {}
    }
    false
}

/// Push prose wrapped to the pane rather than cut by it.
fn say(body: &mut Vec<String>, colour: &str, indent: &str, text: &str, w: usize) {
    let room = w.saturating_sub(crate::display_width(indent) + 2).max(8);
    for line in crate::wrap_words(text, room) {
        body.push(crate::seg(&[(colour, format!("{indent}{line}"))], w.saturating_sub(1)));
    }
}

fn hint_pair(key: &str, rest: &str) -> Vec<(&'static str, String)> {
    vec![(ACCENT, key.to_string()), (DIM, rest.to_string())]
}

/// The screen's footer hints, for the view and mode on show.
///
/// A hint is never split, so on a pane too narrow for the full wording the
/// shorter one is drawn: still the verb, without "this session".
fn hints_for(screen: &Screen, dismissed: bool, room: usize) -> Vec<Vec<(&'static str, String)>> {
    let narrow = room < 30;
    let dismiss = if narrow { "[s] dismiss" } else { "[s] dismiss this session" };
    let all = if narrow { "[a] dismiss all" } else { "[a] dismiss all this session" };
    let mut hints = Vec::new();
    if screen.open.is_some() {
        if dismissed {
            hints.push(vec![(DIM, "[u] restore".to_string())]);
        } else {
            hints.push(vec![(DIM, dismiss.to_string())]);
        }
        hints.push(vec![(DIM, "esc / [!] back".to_string())]);
        return hints;
    }
    // Nothing to select in an empty list, so no keys that select.
    if screen.count > 0 {
        hints.push(hint_pair("↑↓", " select"));
        hints.push(vec![(DIM, "[↵] detail".to_string())]);
    }
    match screen.view {
        View::Outstanding => {
            if screen.count > 0 {
                hints.push(vec![(DIM, dismiss.to_string())]);
                hints.push(vec![(DIM, all.to_string())]);
            }
            hints.push(vec![(DIM, "[h] show dismissed".to_string())]);
        }
        View::Dismissed => {
            if screen.count > 0 {
                hints.push(vec![(DIM, "[u] restore".to_string())]);
            }
            hints.push(vec![(DIM, "[h] show outstanding".to_string())]);
        }
    }
    hints.push(vec![(DIM, "esc / [!] back".to_string())]);
    hints
}

/// Build one frame of the screen: rows, the packed footer, and the frame
/// row the footer starts on.
fn frame(
    r: &Registry,
    screen: &mut Screen,
    now: f64,
    w: usize,
    h: usize,
) -> (Vec<String>, crate::Footer, usize) {
    let head = crate::title(&format!("{} notices", r.widget_name()), w, ACCENT);
    let outstanding = r.outstanding(now);
    let dismissed = r.dismissed_live(now);
    let list: &Vec<&Live> = match screen.view {
        View::Outstanding => &outstanding,
        View::Dismissed => &dismissed,
    };
    // A notice open in detail that has since been cleared, or moved lists,
    // closes rather than showing a stale reading.
    let open = screen
        .open
        .and_then(|key| r.live.iter().find(|l| l.notice.key() == key));
    if screen.open.is_some() && open.is_none() {
        screen.close();
    }
    screen.ids = list.iter().map(|l| l.notice.key()).collect();
    screen.count = list.len();
    if screen.count > 0 {
        screen.selected = screen.selected.min(screen.count - 1);
    } else {
        screen.selected = 0;
    }

    let mut body: Vec<String> = Vec::new();
    let mut placed: Vec<(usize, usize)> = Vec::new();
    let mut cursor_row = 0usize;
    let open_dismissed = open.is_some_and(|l| r.is_dismissed(l, now));
    let width = w.saturating_sub(1);

    if let Some(live) = open {
        let n = &live.notice;
        say(&mut body, n.level.colour(), " ", &n.title, w);
        body.push(String::new());
        let source = if n.source.is_empty() {
            match n.scope {
                Scope::Core => "core",
                Scope::Widget => r.widget_name(),
            }
        } else {
            n.source
        };
        let scope = match n.scope {
            Scope::Core => "shared by every widget".to_string(),
            Scope::Widget => format!("{} only", r.widget_name()),
        };
        let mut facts: Vec<(&str, String)> = vec![
            ("level", n.level.word().to_string()),
            ("scope", scope),
            ("source", source.to_string()),
            ("first seen", format!("{} ago", crate::age(now - live.first))),
            ("status", "still true".to_string()),
        ];
        if open_dismissed {
            let at = r.dismissed_at(live).unwrap_or(now);
            facts.push((
                "dismissed",
                format!("{} ago, for this session", crate::age(now - at)),
            ));
        }
        for (label, value) in facts {
            // Label and value side by side where the value fits beside its
            // label, and the value wrapped under it where it does not: a
            // fact cut at the pane edge would be a fact half said.
            if 2 + 11 + crate::display_width(&value) < w {
                body.push(crate::seg(
                    &[(LBL, format!("  {:<11}", label)), (TXT, value)],
                    width,
                ));
            } else {
                say(&mut body, LBL, "  ", label, w);
                say(&mut body, TXT, "    ", &value, w);
            }
        }
        if !n.detail.is_empty() {
            body.push(String::new());
            say(&mut body, TXT, "  ", &n.detail, w);
        }
        body.push(String::new());
        let options = if open_dismissed {
            "Restoring it puts it back in the footer counter."
        } else {
            "Dismissing it hides it from the banner and the footer counter until \
             this widget restarts, or until what it says changes."
        };
        say(&mut body, DIM, "  ", options, w);
    } else {
        let summary = format!(
            " {} outstanding · {} dismissed this session",
            outstanding.len(),
            dismissed.len()
        );
        body.push(crate::seg(&[(DIM, summary)], width));
        if list.is_empty() {
            body.push(String::new());
            let empty = match screen.view {
                View::Outstanding if dismissed.is_empty() => {
                    "Nothing outstanding. Nothing this widget has raised is still true."
                }
                View::Outstanding => {
                    "Nothing outstanding. Press h to see what was dismissed this session."
                }
                View::Dismissed => "Nothing dismissed this session.",
            };
            say(&mut body, TXT, " ", empty, w);
        } else {
            // Columns as the pane allows: more width is more columns, and
            // the notice itself wraps rather than being cut.
            let show_since = w >= 40;
            let show_source = w >= 56;
            let source_w = 9usize;
            let since_w = 6usize;
            let lead = 2 + 7; // the mark, then LEVEL
            let notice_w = w
                .saturating_sub(lead + 1)
                .saturating_sub(if show_source { source_w } else { 0 })
                .saturating_sub(if show_since { since_w } else { 0 })
                .max(8);
            let mut header = format!("  {:<7}{}", "LEVEL", crate::pad("NOTICE", notice_w));
            if show_source {
                header.push_str(&crate::pad("SOURCE", source_w));
            }
            if show_since {
                header.push_str("SINCE");
            }
            body.push(crate::seg(&[(LBL, header)], width));
            for (i, live) in list.iter().enumerate() {
                let here = i == screen.selected;
                if here {
                    cursor_row = body.len();
                }
                let tint = if here {
                    crate::bg(TINT.0, TINT.1, TINT.2)
                } else {
                    String::new()
                };
                let on = |colour: &str| -> String {
                    if tint.is_empty() {
                        colour.to_string()
                    } else if colour == DIM {
                        format!("{tint}{DIM_LIT}")
                    } else {
                        format!("{tint}{colour}")
                    }
                };
                let level = if here {
                    live.notice.level.colour_on_tint()
                } else {
                    live.notice.level.colour()
                };
                let n = &live.notice;
                let source = if n.source.is_empty() {
                    match n.scope {
                        Scope::Core => "core",
                        Scope::Widget => r.widget_name(),
                    }
                } else {
                    n.source
                };
                let lines = crate::wrap_words(&n.title, notice_w.saturating_sub(1).max(4));
                for (j, line) in lines.iter().enumerate() {
                    placed.push((body.len(), i));
                    let mut parts: Vec<(String, String)> = Vec::new();
                    if j == 0 {
                        parts.push((
                            on(if here { ACCENT } else { DIM }),
                            if here { "▸ ".into() } else { "  ".into() },
                        ));
                        parts.push((on(level), format!("{:<7}", n.level.word())));
                    } else {
                        parts.push((on(DIM), " ".repeat(9)));
                    }
                    parts.push((on(TXT), crate::pad(line, notice_w)));
                    if j == 0 {
                        if show_source {
                            parts.push((on(DIM), crate::pad(source, source_w)));
                        }
                        if show_since {
                            parts.push((on(DIM), crate::age(now - live.first)));
                        }
                    }
                    if here {
                        parts.push((tint.clone(), " ".repeat(w)));
                    }
                    let refs: Vec<(&str, String)> =
                        parts.iter().map(|(c, t)| (c.as_str(), t.clone())).collect();
                    body.push(crate::seg(&refs, width));
                }
            }
        }
    }

    let hints = hints_for(screen, open_dismissed, w.saturating_sub(2));
    let packed = crate::pack_hints_placed(&hints, w.saturating_sub(2), "  ");
    let foot: Vec<String> = packed.lines.iter().map(|l| format!(" {l}")).collect();
    let room = h.saturating_sub(1 + foot.len()).max(1);
    if screen.chase && screen.open.is_none() {
        screen.scroll = crate::follow(screen.scroll, cursor_row, room);
    }
    screen.chase = false;
    screen.scroll = screen.scroll.min(body.len().saturating_sub(room));

    let mut out = vec![head];
    out.extend(body.iter().skip(screen.scroll).take(room).cloned());
    while out.len() + foot.len() < h {
        out.push(String::new());
    }
    let foot_top = out.len();
    out.extend(foot);
    out.truncate(h);
    screen.placed = if screen.open.is_some() {
        Vec::new()
    } else {
        placed.into_iter().map(|(row, i)| (row + 1, i)).collect()
    };
    screen.head = 1;
    screen.shown_scroll = screen.scroll;
    (out, packed, foot_top)
}

/// When the config file's permissions were last looked at, as `f64` bits;
/// zero until `load_config` first runs, so a widget that never loads config
/// never stats the file.
static CONFIG_CHECKED: AtomicU64 = AtomicU64::new(0);

/// How often a running widget looks at the config file's permissions again.
const CONFIG_RECHECK_SECS: f64 = 5.0;

/// Raise the config file's permissions as the one Core notice every pane
/// shares. Called from `load_config`, so a widget does not have to, and
/// again every few seconds from `hint` and `banner_rows`: widgets load
/// config once at startup, so a check made only there would keep saying
/// "readable by others" after the `chmod 600` it asks for, and would never
/// see a file an editor later rewrote with wider permissions.
pub(crate) fn raise_config_permissions() {
    CONFIG_CHECKED.store(crate::now().max(f64::MIN_POSITIVE).to_bits(), Ordering::Relaxed);
    check_config_permissions();
}

/// Look again if config has been loaded and the last look is stale. Called
/// outside the registry lock, since raising takes it.
fn recheck_config_permissions(now: f64) {
    let last = CONFIG_CHECKED.load(Ordering::Relaxed);
    if last == 0 || now - f64::from_bits(last) < CONFIG_RECHECK_SECS {
        return;
    }
    if CONFIG_CHECKED
        .compare_exchange(last, now.to_bits(), Ordering::Relaxed, Ordering::Relaxed)
        .is_ok()
    {
        check_config_permissions();
    }
}

fn check_config_permissions() {
    when(crate::config_token_warning().is_some(), || {
        Notice::warn(
            "config-world-readable",
            "config.json is readable by others; chmod 600 it",
        )
        .detail(
            "Tokens and cookies set in it are a sign-in for whoever can read \
             the file. `chmod 600` on the config file makes it yours alone.",
        )
        .source("config")
        .core()
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reg() -> Registry {
        Registry {
            widget: "test",
            ..Registry::default()
        }
    }

    fn plain(rows: &[String]) -> String {
        rows.iter()
            .map(|r| crate::source_text(r))
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn raising_the_same_id_again_updates_in_place() {
        let mut r = reg();
        r.raise(Notice::warn("a", "one"), 0.0);
        r.raise(Notice::warn("a", "one"), 5.0);
        assert_eq!(r.live.len(), 1);
        // Same content: the banner is not re-armed.
        assert_eq!(r.live[0].armed, 0.0);
        r.raise(Notice::info("b", "two"), 6.0);
        assert_eq!(r.live.len(), 2);
    }

    #[test]
    fn a_changed_hash_rearms_the_banner_and_voids_a_dismissal() {
        let mut r = reg();
        r.raise(Notice::warn("a", "one"), 0.0);
        assert_eq!(r.banner(10.0).len(), 1);
        assert!(r.banner(BANNER_SECS + 1.0).is_empty(), "the banner times out");
        // But the counter is still there: it never vanishes while true.
        assert!(hint_of(&r, BANNER_SECS + 1.0, 80).is_some());
        r.dismiss((Scope::Widget, "a"), 70.0);
        assert!(r.outstanding(71.0).is_empty());
        r.raise(Notice::warn("a", "one").detail("and now this"), 100.0);
        assert_eq!(r.outstanding(101.0).len(), 1, "a changed problem shows again");
        assert_eq!(r.banner(101.0).len(), 1, "and banners again");
        assert_eq!(r.live[0].first, 0.0, "first seen does not move");
    }

    #[test]
    fn clear_removes_the_banner_and_the_hint() {
        let mut r = reg();
        r.raise(Notice::error("a", "broken"), 0.0);
        assert!(!banner_of(&r, 1.0, 80, 40).is_empty());
        assert!(hint_of(&r, 1.0, 80).is_some());
        r.clear((Scope::Widget, "a"));
        assert!(banner_of(&r, 1.0, 80, 40).is_empty());
        assert!(hint_of(&r, 1.0, 80).is_none());
    }

    #[test]
    fn the_hint_counts_by_level_worst_first() {
        let mut r = reg();
        assert!(hint_of(&r, 0.0, 80).is_none(), "absent when nothing is outstanding");
        r.raise(Notice::info("i", "x"), 0.0);
        let hint = hint_of(&r, 0.0, 80).unwrap();
        assert_eq!(hint[0].1, "[!] 1 note");
        assert_eq!(hint[0].0, INFO);
        r.raise(Notice::warn("w1", "x"), 0.0);
        r.raise(Notice::warn("w2", "y"), 0.0);
        r.raise(Notice::error("e", "z"), 0.0);
        let hint = hint_of(&r, 0.0, 80).unwrap();
        assert_eq!(hint[0].1, "[!] 1 error, 2 warnings, 1 note");
        assert_eq!(hint[0].0, BAD, "coloured by the worst level");
        // And the footer reads its key.
        let packed = crate::pack_hints_placed(&[hint], 80, "  ");
        assert_eq!(packed.spots[0].key, "!");
        // A narrow footer gets a shorter hint rather than an overflowing one.
        assert_eq!(hint_of(&r, 0.0, 20).unwrap()[0].1, "[!] 4 notices");
        assert_eq!(hint_of(&r, 0.0, 8).unwrap()[0].1, "[!] 4");
    }

    #[test]
    fn the_banner_shows_two_newest_first_and_wraps_rather_than_clips() {
        let mut r = reg();
        r.raise(Notice::info("a", "first"), 0.0);
        r.raise(Notice::info("b", "second"), 1.0);
        r.raise(Notice::info("c", "third"), 2.0);
        let ids: Vec<&str> = r.banner(3.0).iter().map(|l| l.notice.id).collect();
        assert_eq!(ids, vec!["c", "b"]);
        let mut r = reg();
        r.raise(
            Notice::warn("long", "a title long enough to need wrapping at this width")
                .detail("and a detail that says what to do"),
            0.0,
        );
        let rows = banner_of(&r, 1.0, 24, 40);
        assert!(rows.len() > 2);
        for row in &rows {
            assert!(crate::display_width(row) <= 23, "{row:?}");
        }
        let text = plain(&rows).split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(text.contains("at this width"), "nothing is clipped: {text}");
        assert!(!text.contains("what to do"), "the detail waits on the ! screen: {text}");
    }

    /// The Core config notice as `check_config_permissions` raises it.
    fn config_notice() -> Notice {
        Notice::warn(
            "config-world-readable",
            "config.json is readable by others; chmod 600 it",
        )
        .detail(
            "Tokens and cookies set in it are a sign-in for whoever can read \
             the file. `chmod 600` on the config file makes it yours alone.",
        )
        .source("config")
        .core()
    }

    #[test]
    fn the_config_notice_at_24_wide_stays_within_its_budget() {
        let mut r = reg();
        r.raise(config_notice(), 0.0);
        for h in 0..=40 {
            let rows = banner_of(&r, 1.0, 24, h);
            assert!(rows.len() <= banner_budget(h), "{h}: {} rows", rows.len());
            for row in &rows {
                assert!(crate::display_width(row) <= 23, "{h}: {row:?}");
            }
            let text = plain(&rows).split_whitespace().collect::<Vec<_>>().join(" ");
            // Whole or not at all: never a title cut part way.
            assert!(
                rows.is_empty() || text.contains("chmod 600 it"),
                "{h}: {text}"
            );
            assert!(!text.contains("Tokens"), "{h}: the detail is not bannered");
        }
        // And at an ordinary height it does show.
        assert!(!banner_of(&r, 1.0, 24, 24).is_empty());
    }

    #[test]
    fn a_short_pane_keeps_its_footer_hints_on_screen() {
        // Built the way the widgets build a frame: the title pinned, a body
        // window of at least one row, the banner, then the packed footer.
        // The banner must never be what pushes the footer off the bottom.
        let mut r = reg();
        r.raise(config_notice(), 0.0);
        r.raise(
            Notice::error("fetch", "the last fetch failed and the figures are stale"),
            0.5,
        );
        for w in [20usize, 24, 30, 60, 120] {
            for h in 6..=30 {
                let mut hints = vec![
                    vec![(DIM, "[r]efresh".to_string())],
                ];
                hints.extend(hint_of(&r, 1.0, w - 2));
                hints.push(vec![(DIM, "[,] settings".to_string())]);
                hints.push(vec![(DIM, "[q]uit".to_string())]);
                let packed = crate::pack_hints_placed(&hints, w - 2, "  ");
                let footer = packed.lines.len();
                // Only panes the footer fits at all, with no banner.
                if 2 + footer > h {
                    continue;
                }
                let banner = banner_of(&r, 1.0, w, h);
                let body = h.saturating_sub(1 + banner.len() + footer).max(1);
                assert!(
                    1 + body + banner.len() + footer <= h,
                    "{w}x{h}: banner {} + footer {footer} pushed the hints off",
                    banner.len()
                );
                for row in banner.iter().chain(packed.lines.iter()) {
                    assert!(crate::display_width(row) <= w, "{w}x{h}: {row:?}");
                }
            }
        }
    }

    #[test]
    fn dismiss_all_and_restore() {
        let mut r = reg();
        r.raise(Notice::warn("a", "x"), 0.0);
        r.raise(Notice::error("b", "y"), 0.0);
        r.dismiss_all(1.0);
        assert!(r.outstanding(2.0).is_empty());
        assert_eq!(r.dismissed_live(2.0).len(), 2, "a dismissed notice is findable");
        assert!(hint_of(&r, 2.0, 80).is_none());
        r.restore((Scope::Widget, "b"));
        assert_eq!(r.outstanding(3.0).len(), 1);
        // A dismissal outlives a clear and re-raise of the same content.
        r.clear((Scope::Widget, "a"));
        r.raise(Notice::warn("a", "x"), 4.0);
        assert_eq!(r.outstanding(5.0).len(), 1);
    }

    #[test]
    fn core_and_widget_dismissals_are_keyed_apart() {
        let mut r = reg();
        let core = Notice::warn("x", "t").core();
        assert_eq!(r.key(&core), "core/x");
        assert_eq!(r.key(&Notice::warn("x", "t")), "test/x");
        r.raise(core, 0.0);
        r.dismiss((Scope::Core, "x"), 0.0);
        assert!(r.dismissed.contains_key("core/x"));
    }

    #[test]
    fn a_widget_notice_sharing_a_core_id_is_a_second_notice() {
        let mut r = reg();
        r.raise(Notice::warn("x", "core's").core(), 0.0);
        r.raise(Notice::warn("x", "the widget's"), 0.0);
        assert_eq!(r.live.len(), 2, "neither overwrites the other");
        r.clear((Scope::Widget, "x"));
        assert_eq!(r.live.len(), 1);
        assert_eq!(r.live[0].notice.scope, Scope::Core, "the Core one survives");
        assert_eq!(r.live[0].notice.title, "core's");
    }

    #[test]
    fn backing_out_of_detail_returns_to_the_list_where_it_was() {
        let mut r = reg();
        for (i, id) in ["n0", "n1", "n2", "n3", "n4", "n5", "n6", "n7", "n8", "n9"]
            .into_iter()
            .enumerate()
        {
            r.raise(Notice::warn(id, format!("notice number {i}")), i as f64);
        }
        let mut screen = Screen::new();
        let h = 8;
        frame(&r, &mut screen, 20.0, 60, h);
        for _ in 0..9 {
            screen.step(1);
        }
        frame(&r, &mut screen, 20.0, 60, h);
        let list_scroll = screen.scroll;
        assert!(list_scroll > 0, "the last notice is below the first pane");
        handle(&mut screen, "enter", 20.0);
        frame(&r, &mut screen, 20.0, 60, h);
        handle(&mut screen, "down", 20.0);
        handle(&mut screen, "esc", 20.0);
        assert!(screen.open.is_none());
        let (rows, _, _) = frame(&r, &mut screen, 20.0, 60, h);
        assert_eq!(screen.scroll, list_scroll);
        assert!(plain(&rows).contains("notice number 0"), "the selected row is on screen");
    }

    #[test]
    fn the_hash_is_stable_across_builds() {
        // A persisted dismissal compares against this number; pin it.
        const PINNED: u64 = 0x566dda1943ee58c7;
        assert_eq!(Notice::info("a", "t").detail("d").hash(), PINNED);
        assert_eq!(
            Notice::info("a", "t").detail("d").hash(),
            Notice::warn("other-id", "t").detail("d").hash(),
            "the id and level are not content"
        );
        assert_ne!(Notice::info("a", "t").hash(), Notice::info("a", "u").hash());
        assert_ne!(
            Notice::info("a", "ab").detail("c").hash(),
            Notice::info("a", "a").detail("bc").hash(),
            "the title and detail are kept apart"
        );
    }

    #[test]
    fn raise_and_clear_are_safe_from_many_threads() {
        const IDS: [&str; 4] = ["thread-a", "thread-b", "thread-c", "thread-d"];
        let handles: Vec<_> = (0..8)
            .map(|t| {
                std::thread::spawn(move || {
                    for i in 0..200 {
                        let id = IDS[(t + i) % IDS.len()];
                        raise(Notice::info(id, format!("pass {}", i % 3)));
                        let _ = hint(40);
                        let _ = banner_rows(40, 24);
                        if i % 7 == 0 {
                            clear(id);
                        }
                    }
                })
            })
            .collect();
        for handle in handles {
            handle.join().expect("no thread panicked");
        }
        // The registry is still usable afterwards, and holds each id once.
        with(|r| {
            for id in IDS {
                assert!(r.live.iter().filter(|l| l.notice.id == id).count() <= 1);
            }
        });
        for id in IDS {
            clear(id);
        }
    }

    #[test]
    fn a_poisoned_registry_is_recovered() {
        let lock: Mutex<Registry> = Mutex::new(reg());
        let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = lock.lock().unwrap();
            panic!("poison it");
        }));
        assert!(lock.is_poisoned());
        let guard = match lock.lock() {
            Ok(g) => g,
            Err(p) => p.into_inner(),
        };
        assert_eq!(guard.widget, "test");
    }

    #[test]
    fn when_raises_and_clears() {
        when(true, || Notice::info("when-test", "on"));
        assert!(with(|r| r.live.iter().any(|l| l.notice.id == "when-test")));
        when(false, || Notice::info("when-test", "on"));
        assert!(!with(|r| r.live.iter().any(|l| l.notice.id == "when-test")));
    }

    fn channels(esc: &str) -> (f64, f64, f64) {
        let tail = esc.trim_start_matches("\x1b[38;2;").trim_end_matches('m');
        let v: Vec<f64> = tail.split(';').map(|n| n.parse().unwrap()).collect();
        (v[0], v[1], v[2])
    }

    fn luminance((r, g, b): (f64, f64, f64)) -> f64 {
        let lin = |c: f64| {
            let c = c / 255.0;
            if c <= 0.03928 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        };
        0.2126 * lin(r) + 0.7152 * lin(g) + 0.0722 * lin(b)
    }

    fn contrast(a: (f64, f64, f64), b: (f64, f64, f64)) -> f64 {
        let (x, y) = (luminance(a), luminance(b));
        (x.max(y) + 0.05) / (x.min(y) + 0.05)
    }

    #[test]
    fn every_colour_clears_aa_on_black_and_on_the_tint() {
        let black = (0.0, 0.0, 0.0);
        let tint = (TINT.0 as f64, TINT.1 as f64, TINT.2 as f64);
        // On the background: everything drawn there.
        for colour in [BAD, BAD_LIT, WARN, INFO, TXT, LBL, DIM, DIM_LIT, ACCENT] {
            let c = contrast(channels(colour), black);
            assert!(c >= 4.5, "{colour:?} is {c:.2} on black");
        }
        // On the selected row: what a selected row can draw. The shared red
        // is not on the list - it is swapped for the lifted one.
        for level in [Level::Info, Level::Warn, Level::Error] {
            let colour = level.colour_on_tint();
            let c = contrast(channels(colour), tint);
            assert!(c >= 4.5, "{:?} is {c:.2} on the tint", level);
        }
        for colour in [TXT, DIM_LIT, ACCENT] {
            let c = contrast(channels(colour), tint);
            assert!(c >= 4.5, "{colour:?} is {c:.2} on the tint");
        }
        // And the reason for the swap, so it is not undone by accident.
        assert!(contrast(channels(BAD), tint) < 4.5);
    }

    #[test]
    fn the_screen_lists_and_details_without_overflowing() {
        let mut r = reg();
        r.raise(
            Notice::error("e", "config.json: `refresh` is not a number — using 4s")
                .source("config"),
            0.0,
        );
        r.raise(Notice::info("i", "curl is not installed (recommended)"), 0.0);
        for w in [20usize, 26, 30, 60, 120] {
            let mut screen = Screen::new();
            let (rows, packed, top) = frame(&r, &mut screen, 30.0, w, 24);
            assert_eq!(rows.len(), 24);
            for row in &rows {
                assert!(crate::display_width(row) <= w, "{w}: {row:?}");
            }
            assert!(top + packed.lines.len() <= 24);
            assert_eq!(screen.count, 2);
            // Worst first.
            assert_eq!(screen.ids[0], (Scope::Widget, "e"));
            assert!(handle(&mut screen, "enter", 30.0) == false);
            let (rows, _, _) = frame(&r, &mut screen, 30.0, w, 24);
            let text = plain(&rows);
            assert!(text.contains("still true"), "{w}: {text}");
        }
    }

    #[test]
    fn the_screen_dismisses_and_restores() {
        // Through the global registry, because that is what the keys act on.
        raise(Notice::warn("screen-test", "dismiss me"));
        let mut screen = Screen::new();
        let now = crate::now();
        with(|r| frame(r, &mut screen, now, 80, 24));
        let at = screen.ids.iter().position(|id| *id == (Scope::Widget, "screen-test")).unwrap();
        screen.selected = at;
        handle(&mut screen, "s", now);
        assert!(with(|r| r.dismissed_live(now).iter().any(|l| l.notice.id == "screen-test")));
        handle(&mut screen, "h", now);
        with(|r| frame(r, &mut screen, now, 80, 24));
        let at = screen.ids.iter().position(|id| *id == (Scope::Widget, "screen-test")).unwrap();
        screen.selected = at;
        handle(&mut screen, "u", now);
        assert!(with(|r| r.outstanding(now).iter().any(|l| l.notice.id == "screen-test")));
        clear("screen-test");
        // The wheel moves the view, never the cursor.
        let before = screen.selected;
        handle(&mut screen, "wheel-down", now);
        assert_eq!(screen.selected, before);
        assert!(handle(&mut screen, "esc", now));
    }
}
