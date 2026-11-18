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

//! Shared coding-agent CLI catalogue for `mux-activity` and `agent-activity`.
//!
//! Inclusion follows confidence in the command name rather than popularity: a
//! wrong agent row is worse than a missing one. Confirmed distinctive command
//! names sit in Group 1 (`second_clues` empty). Generic or colliding command
//! names (`forge`, `agent`, `cn`, `vibe`) sit in Group 2 and match a process
//! only when an install or script path supplies a second clue, or when the
//! user explicitly adds that command name to `agents`.

use super::{base_name, is_shell, product, INTERPRETERS};

/// One entry in the shared coding-agent catalogue.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CatalogueEntry {
    /// Human-facing product name (`"Antigravity CLI"`).
    pub display: &'static str,
    /// Canonical short name used on rows and in `DEFAULT_AGENTS` (`"agy"`).
    pub id: &'static str,
    /// Command names the agent's process runs as (`&["kiro-cli", "kiro"]`).
    pub commands: &'static [&'static str],
    /// Names multiplexers or OSC 7501 reports use (`&["agy", "antigravity", "antigravity-cli"]`).
    pub aliases: &'static [&'static str],
    /// Subcommands or flags that run background services rather than interactive sessions.
    pub service_subcommands: &'static [&'static str],
    /// Path or script substrings required when `commands` is generic (Group 2);
    /// empty `&[]` for Group 1 entries.
    pub second_clues: &'static [&'static str],
    /// One-line provenance note recording where the command name was confirmed.
    pub note: &'static str,
}

impl CatalogueEntry {
    /// Group 1 entries have distinctive command names and need no second clue.
    pub const fn is_group_1(&self) -> bool {
        self.second_clues.is_empty()
    }
}

/// Every confirmed CLI coding agent: 26 Group 1 entries (`second_clues` empty)
/// followed by 4 Group 2 entries that require a path/script clue.
pub const CATALOGUE: &[CatalogueEntry] = &[
    // ── Group 1: distinctive, confirmed command names ──
    CatalogueEntry {
        display: "Claude Code",
        id: "claude",
        commands: &["claude"],
        aliases: &["claude", "claude-code", "claude-cli"],
        service_subcommands: &["gateway", "mcp serve"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/claude) and https://docs.anthropic.com/en/docs/claude-code/cli-reference",
    },
    CatalogueEntry {
        display: "Codex",
        id: "codex",
        commands: &["codex"],
        aliases: &["codex", "codex-cli"],
        service_subcommands: &["app-server", "mcp-server", "remote-control"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/codex) and https://github.com/openai/codex",
    },
    CatalogueEntry {
        display: "Gemini CLI",
        id: "gemini",
        commands: &["gemini"],
        aliases: &["gemini", "gemini-cli"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from npm @google/gemini-cli bin 'gemini' at https://github.com/google-gemini/gemini-cli",
    },
    CatalogueEntry {
        display: "OpenCode",
        id: "opencode",
        commands: &["opencode"],
        aliases: &["opencode"],
        service_subcommands: &["serve", "service", "acp", "web"],
        second_clues: &[],
        note: "confirmed on local install (~/.opencode/bin/opencode) and https://opencode.ai/docs/cli",
    },
    CatalogueEntry {
        display: "Antigravity CLI",
        id: "agy",
        commands: &["agy"],
        aliases: &["agy", "antigravity", "antigravity-cli"],
        service_subcommands: &["acp", "mic-serve", "remote-control"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/agy) and Herdr integration 'antigravity-cli' at https://herdr.dev/tui/docs/agents/",
    },
    CatalogueEntry {
        display: "Pi",
        id: "pi",
        commands: &["pi"],
        aliases: &["pi"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed on local install (~/.pi/agent/bin/pi -> node_modules/.bin/pi) and Herdr integration 'pi'",
    },
    CatalogueEntry {
        display: "Oh My Pi",
        id: "omp",
        commands: &["omp"],
        aliases: &["omp"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from @oh-my-pi/pi-coding-agent bin 'omp' (https://omp.sh) and Herdr integration 'omp'",
    },
    CatalogueEntry {
        display: "Cursor Agent",
        id: "cursor-agent",
        commands: &["cursor-agent"],
        aliases: &["cursor-agent", "cursor"],
        service_subcommands: &["worker"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/cursor-agent), Herdr integration 'cursor', and https://docs.cursor.com/en/cli/overview",
    },
    CatalogueEntry {
        display: "GitHub Copilot CLI",
        id: "copilot",
        commands: &["copilot"],
        aliases: &["copilot", "copilot-cli"],
        service_subcommands: &["--acp"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/copilot) and https://docs.github.com/en/copilot/how-tos/use-copilot-agents/use-copilot-cli",
    },
    CatalogueEntry {
        display: "Droid",
        id: "droid",
        commands: &["droid"],
        aliases: &["droid"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Factory CLI docs https://docs.factory.ai/cli/getting-started/quickstart and Herdr integration 'droid'",
    },
    CatalogueEntry {
        display: "Devin CLI",
        id: "devin",
        commands: &["devin"],
        aliases: &["devin", "devin-cli"],
        service_subcommands: &["acp", "forward"],
        second_clues: &[],
        note: "confirmed on local install (~/.local/bin/devin), Herdr integration 'devin', and https://docs.devin.ai",
    },
    CatalogueEntry {
        display: "Grok CLI",
        id: "grok",
        commands: &["grok"],
        aliases: &["grok", "grok-cli"],
        service_subcommands: &["leader", "worker"],
        second_clues: &[],
        note: "confirmed on local install (~/.grok/bin/grok) and Herdr integration 'grok'",
    },
    CatalogueEntry {
        display: "Amp",
        id: "amp",
        commands: &["amp"],
        aliases: &["amp", "amp-cli"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Sourcegraph Amp (@sourcegraph/amp / @ampcode/cli) manual at https://ampcode.com/manual",
    },
    CatalogueEntry {
        display: "Kimi Code",
        id: "kimi",
        commands: &["kimi"],
        aliases: &["kimi", "kimi-code", "kimi-cli"],
        service_subcommands: &["web", "acp"],
        second_clues: &[],
        note: "confirmed from MoonshotAI/kimi-code docs at https://code.kimi.com and Herdr integration 'kimi'",
    },
    CatalogueEntry {
        display: "Qwen Code",
        id: "qwen",
        commands: &["qwen"],
        aliases: &["qwen", "qwen-code"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from @qwen-code/qwen-code at https://github.com/QwenLM/qwen-code and Herdr integration 'qwen'",
    },
    CatalogueEntry {
        display: "Qoder CLI",
        id: "qodercli",
        commands: &["qodercli"],
        aliases: &["qodercli"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from @qoder-ai/qodercli at https://docs.qoder.com/cli/quick-start and Herdr integration 'qodercli'",
    },
    CatalogueEntry {
        display: "Kiro CLI",
        id: "kiro",
        commands: &["kiro-cli", "kiro"],
        aliases: &["kiro", "kiro-cli"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from https://kiro.dev/cli/ ('kiro-cli', also 'kiro') and Herdr integration 'kiro'",
    },
    CatalogueEntry {
        display: "Cline CLI",
        id: "cline",
        commands: &["cline"],
        aliases: &["cline", "cline-cli"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from npm 'cline' at https://docs.cline.bot/cline-cli/overview",
    },
    CatalogueEntry {
        display: "Crush",
        id: "crush",
        commands: &["crush"],
        aliases: &["crush"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Charmbracelet Crush (@charmland/crush) at https://github.com/charmbracelet/crush",
    },
    CatalogueEntry {
        display: "Aider",
        id: "aider",
        commands: &["aider"],
        aliases: &["aider", "aider-chat"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Python aider-chat CLI docs at https://aider.chat/docs/usage.html",
    },
    CatalogueEntry {
        display: "Goose",
        id: "goose",
        commands: &["goose"],
        aliases: &["goose"],
        service_subcommands: &["mcp", "web"],
        second_clues: &[],
        note: "confirmed from Goose CLI docs at https://block.github.io/goose/docs/getting-started/installation",
    },
    CatalogueEntry {
        display: "Codebuff",
        id: "codebuff",
        commands: &["codebuff"],
        aliases: &["codebuff"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from npm 'codebuff' quickstart at https://www.codebuff.com/docs/help/quick-start",
    },
    CatalogueEntry {
        display: "Auggie",
        id: "auggie",
        commands: &["auggie"],
        aliases: &["auggie", "augment"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from npm @augmentcode/auggie docs at https://docs.augmentcode.com/cli/overview",
    },
    CatalogueEntry {
        display: "gptme",
        id: "gptme",
        commands: &["gptme"],
        aliases: &["gptme"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Python 'gptme' CLI reference at https://gptme.org/docs/cli.html",
    },
    CatalogueEntry {
        display: "Codewhale",
        id: "codewhale",
        commands: &["codewhale"],
        aliases: &["codewhale"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from Rust crate 'codewhale' binary at https://crates.io/crates/codewhale",
    },
    CatalogueEntry {
        display: "Jules",
        id: "jules",
        commands: &["jules"],
        aliases: &["jules"],
        service_subcommands: &[],
        second_clues: &[],
        note: "confirmed from npm @google/jules CLI reference at https://jules.google/docs/cli/reference",
    },
    // ── Group 2: generic or colliding command names; require a second clue ──
    CatalogueEntry {
        display: "ForgeCode",
        id: "forgecode",
        commands: &["forge", "forgecode"],
        aliases: &["forgecode"],
        service_subcommands: &[],
        second_clues: &["/forgecode/", "/@antinomyhq/forge/"],
        note: "confirmed from npm 'forgecode' (@antinomyhq/forge) at https://forgecode.dev; requires path clue so Foundry's 'forge' never matches",
    },
    CatalogueEntry {
        display: "Cursor Agent",
        id: "cursor-agent",
        commands: &["agent"],
        aliases: &["cursor", "cursor-agent"],
        service_subcommands: &["worker"],
        second_clues: &["/cursor-agent/"],
        note: "confirmed on local install (~/.local/bin/agent -> ~/.local/share/cursor-agent/versions/.../cursor-agent); requires '/cursor-agent/' path/script clue",
    },
    CatalogueEntry {
        display: "Continue CLI",
        id: "continue",
        commands: &["cn"],
        aliases: &["continue-cli", "continuedev"],
        service_subcommands: &[],
        second_clues: &["/@continuedev/", "/continuedev/"],
        note: "confirmed from npm @continuedev/cli bin 'cn' at https://docs.continue.dev/cli/overview; requires '@continuedev' path/script clue",
    },
    CatalogueEntry {
        display: "Mistral Vibe",
        id: "vibe",
        commands: &["vibe"],
        aliases: &["mistral-vibe"],
        service_subcommands: &[],
        second_clues: &["/mistral-vibe/", "/mistral_vibe/"],
        note: "confirmed from uv tool 'mistral-vibe' bin 'vibe' at https://github.com/mistralai/mistral-vibe; requires 'mistral-vibe' path/script clue",
    },
];

/// Default `agents` list for `mux-activity` and `agent-activity`: every
/// Group 1 catalogue entry's canonical id, in catalogue order.
pub const DEFAULT_AGENTS: &[&str] = &[
    "claude",
    "codex",
    "gemini",
    "opencode",
    "agy",
    "pi",
    "omp",
    "cursor-agent",
    "copilot",
    "droid",
    "devin",
    "grok",
    "amp",
    "kimi",
    "qwen",
    "qodercli",
    "kiro",
    "cline",
    "crush",
    "aider",
    "goose",
    "codebuff",
    "auggie",
    "gptme",
    "codewhale",
    "jules",
];

/// Whether `agent` has a local session-log reader in [`super::LogReader`].
pub fn has_log_reader(agent: &str) -> bool {
    let lower = agent.trim().to_ascii_lowercase();
    matches!(
        canonical_id_only(&lower).unwrap_or(lower.as_str()),
        "claude" | "codex" | "opencode"
    )
}

fn canonical_id_only(lower: &str) -> Option<&'static str> {
    CATALOGUE
        .iter()
        .find(|e| {
            e.id == lower
                || e.commands.contains(&lower)
                || e.aliases.contains(&lower)
        })
        .map(|e| e.id)
}

fn strip_script_ext(word: &str) -> &str {
    for ext in [".js", ".mjs", ".cjs", ".py"] {
        if let Some(stem) = word.strip_suffix(ext).filter(|s| !s.is_empty()) {
            return stem;
        }
    }
    word
}

fn clean_command_word(word: &str) -> String {
    let base = base_name(word.trim().trim_start_matches('-')).to_ascii_lowercase();
    strip_script_ext(&base).to_string()
}

/// Whether `app` matches a custom command name `cmd` outside `CATALOGUE`.
fn suffix_matches(app_lower: &str, cmd_lower: &str) -> bool {
    !cmd_lower.is_empty()
        && (app_lower == cmd_lower
            || app_lower == format!("{cmd_lower}-code")
            || app_lower == format!("{cmd_lower}-cli")
            || app_lower == format!("{cmd_lower}-agent"))
}

fn entry_named(entry: &CatalogueEntry, lower: &str) -> bool {
    entry.id == lower
        || entry.commands.contains(&lower)
        || entry.aliases.contains(&lower)
}

fn entry_enabled<S: AsRef<str>>(entry: &CatalogueEntry, configured: &[S]) -> bool {
    configured.iter().any(|c| {
        let c = c.as_ref().trim().to_ascii_lowercase();
        !c.is_empty() && entry_named(entry, &c)
    })
}

fn has_all_defaults<S: AsRef<str>>(configured: &[S]) -> bool {
    DEFAULT_AGENTS.iter().all(|def| {
        configured
            .iter()
            .any(|c| c.as_ref().trim().eq_ignore_ascii_case(def))
    })
}

fn group_2_enabled<S: AsRef<str>>(entry: &CatalogueEntry, configured: &[S]) -> bool {
    if entry_enabled(entry, configured) {
        return true;
    }
    if DEFAULT_AGENTS.contains(&entry.id) {
        return false;
    }
    has_all_defaults(configured)
}

/// Resolve a multiplexer-reported or OSC 7501 agent name (`"claude-code"`,
/// `"antigravity-cli"`, `"cursor"`) to its canonical command/id (`"claude"`,
/// `"agy"`, `"cursor-agent"`). Names outside the enabled list are returned unchanged.
pub fn canonical_command<S: AsRef<str>>(name: &str, configured: &[S]) -> String {
    let lower = name.trim().to_ascii_lowercase();
    if lower.is_empty() {
        return name.to_string();
    }
    for entry in CATALOGUE {
        let enabled = if entry.is_group_1() {
            entry_enabled(entry, configured)
        } else {
            group_2_enabled(entry, configured)
        };
        if entry_named(entry, &lower) && enabled {
            return entry.id.to_string();
        }
    }
    for c in configured {
        let c_lower = c.as_ref().trim().to_ascii_lowercase();
        if suffix_matches(&lower, &c_lower) {
            return c_lower;
        }
    }
    name.to_string()
}

/// Whether an OSC 7501 `app` name or multiplexer kind belongs to an agent in
/// `configured` (or a Group 2 unambiguous alias such as `"forgecode"` when enabled).
pub fn matches_reported_app<S: AsRef<str>>(app: &str, configured: &[S]) -> bool {
    let lower = app.trim().to_ascii_lowercase();
    if lower.is_empty() || configured.is_empty() {
        return false;
    }
    for entry in CATALOGUE {
        if entry.is_group_1() {
            if entry_enabled(entry, configured) && entry_named(entry, &lower) {
                return true;
            }
        } else if group_2_enabled(entry, configured)
            && (entry.aliases.contains(&lower.as_str())
                || (entry.id == lower && !entry.commands.contains(&lower.as_str())))
        {
            // Group 2 aliases like `forgecode`, `continue-cli`, `mistral-vibe`
            // are unambiguous reports even when the generic command (`forge`,
            // `cn`, `vibe`) is not in `configured`.
            return true;
        }
    }
    configured.iter().any(|c| {
        let c_lower = c.as_ref().trim().to_ascii_lowercase();
        suffix_matches(&lower, &c_lower)
    })
}

/// Extract `(program, script, first_two_subcommands)` from a process's `comm`
/// and tokenized `argv`.
fn dissect_argv<'a>(comm: &str, words: &[&'a str]) -> (&'a str, Option<&'a str>, [&'a str; 2]) {
    let Some(&program) = words.first() else {
        return ("", None, ["", ""]);
    };
    let is_interp = INTERPRETERS.contains(&base_name(comm))
        || INTERPRETERS.contains(&base_name(program))
        || is_shell("", program);
    if is_interp {
        let mut idx = 1;
        while let Some(&w) = words.get(idx) {
            if matches!(w, "-c" | "-e" | "--eval") {
                return (program, None, ["", ""]);
            }
            if w == "-m" && !is_shell("", program) {
                let module = words.get(idx + 1).copied();
                let s0 = words.get(idx + 2).copied().unwrap_or("");
                let s1 = words.get(idx + 3).copied().unwrap_or("");
                return (program, module, [s0, s1]);
            }
            if w.starts_with('-') {
                idx += 1;
                continue;
            }
            let s0 = words.get(idx + 1).copied().unwrap_or("");
            let s1 = words.get(idx + 2).copied().unwrap_or("");
            return (program, Some(w), [s0, s1]);
        }
        return (program, None, ["", ""]);
    }
    let s0 = words.get(1).copied().unwrap_or("");
    let s1 = words.get(2).copied().unwrap_or("");
    (program, None, [s0, s1])
}

fn collect_candidates<'a>(
    comm: &'a str,
    program: &'a str,
    script: Option<&'a str>,
) -> (Vec<String>, Vec<&'a str>) {
    let mut names = Vec::with_capacity(3);
    for raw in [Some(comm), (!program.is_empty()).then_some(program), script]
        .into_iter()
        .flatten()
    {
        let cleaned = clean_command_word(raw);
        if !cleaned.is_empty() && !is_shell(&cleaned, "") && !names.contains(&cleaned) {
            names.push(cleaned);
        }
    }
    let mut paths = Vec::with_capacity(3);
    if !is_shell(comm, "") {
        paths.push(comm);
    }
    if !program.is_empty() && !is_shell("", program) {
        paths.push(program);
    }
    if let Some(s) = script {
        paths.push(s);
    }
    (names, paths)
}

fn is_service_invocation(entry: &CatalogueEntry, sub: [&str; 2]) -> bool {
    let a = sub[0].to_ascii_lowercase();
    let b = sub[1].to_ascii_lowercase();
    if a.is_empty() {
        return false;
    }
    entry.service_subcommands.iter().any(|svc| {
        if let Some((first, second)) = svc.split_once(' ') {
            a == first && b == second
        } else {
            a == *svc
        }
    })
}

fn has_second_clue(entry: &CatalogueEntry, paths: &[&str]) -> bool {
    entry.second_clues.iter().any(|clue| {
        let clue = clue.to_ascii_lowercase();
        paths.iter().any(|p| {
            let norm = p.replace('\\', "/").to_ascii_lowercase();
            norm.contains(&clue)
        })
    })
}

fn match_candidates<S: AsRef<str>>(
    names: &[String],
    paths: &[&str],
    sub: [&str; 2],
    configured: &[S],
) -> Option<String> {
    if configured.is_empty() || names.is_empty() {
        return None;
    }
    // 1. Group 1 entries enabled by `configured`.
    for entry in CATALOGUE.iter().filter(|e| e.is_group_1()) {
        if !entry_enabled(entry, configured) {
            continue;
        }
        if names.iter().any(|n| entry.commands.contains(&n.as_str())) {
            if is_service_invocation(entry, sub) {
                return None;
            }
            return Some(entry.id.to_string());
        }
    }
    // 2. Group 2 entries: an unambiguous command (e.g. `forgecode`) or a
    //    generic command backed by `second_clues` in its install/script path.
    for entry in CATALOGUE.iter().filter(|e| !e.is_group_1()) {
        if !group_2_enabled(entry, configured) {
            continue;
        }
        let matched = names.iter().find(|n| entry.commands.contains(&n.as_str()));
        let Some(cmd) = matched else { continue };
        let unambiguous_cmd = cmd == entry.id && entry.aliases.contains(&cmd.as_str());
        if unambiguous_cmd || has_second_clue(entry, paths) {
            if is_service_invocation(entry, sub) {
                return None;
            }
            return Some(entry.id.to_string());
        }
    }
    // 3. Explicit user entries in `configured` (custom commands, or a Group 2
    //    command name the user deliberately added to `agents`).
    for c in configured {
        let c_lower = c.as_ref().trim().to_ascii_lowercase();
        if c_lower.is_empty() {
            continue;
        }
        if CATALOGUE
            .iter()
            .any(|e| e.is_group_1() && entry_named(e, &c_lower))
        {
            continue;
        }
        if names.iter().any(|n| n == &c_lower) {
            return Some(c_lower);
        }
    }
    None
}

/// Classify a `ps` row `(comm, args)` against `configured` (e.g. `DEFAULT_AGENTS`
/// or the user's `agents` list). Returns the canonical agent name when matched.
pub fn classify_process<S: AsRef<str>>(
    comm: &str,
    args: &str,
    configured: &[S],
) -> Option<String> {
    let words: Vec<&str> = args.split_whitespace().collect();
    if words.first().is_some_and(|w| w.starts_with('-')) {
        return None;
    }
    let (program, script, sub) = dissect_argv(comm, &words);
    let (names, paths) = collect_candidates(comm, program, script);
    match_candidates(&names, &paths, sub, configured)
}

/// Classify a foreground `(name, argv)` slice (as Herdr reports it) against `configured`.
pub fn classify_argv<S: AsRef<str>>(
    name: &str,
    argv: &[String],
    configured: &[S],
) -> Option<String> {
    if argv.first().is_some_and(|w| w.starts_with('-')) {
        return None;
    }
    let words: Vec<&str> = argv.iter().map(String::as_str).collect();
    let (program, script, sub) = dissect_argv(name, &words);
    let (names, paths) = collect_candidates(name, program, script);
    match_candidates(&names, &paths, sub, configured)
}

/// Classify a Rex foreground block's fields against `configured`.
pub fn classify_foreground<S: AsRef<str>>(
    name: &str,
    argv0: &str,
    path: &str,
    invoked_path: &str,
    script: &str,
    configured: &[S],
) -> Option<String> {
    if argv0.starts_with('-') {
        return None;
    }
    let argv0_prog = argv0.split_whitespace().next().unwrap_or("");
    let mut names = Vec::with_capacity(5);
    for raw in [invoked_path, argv0_prog, path, name, script] {
        let cleaned = clean_command_word(raw);
        if !cleaned.is_empty() && !is_shell(&cleaned, "") && !names.contains(&cleaned) {
            names.push(cleaned);
        }
    }
    let mut paths = Vec::with_capacity(4);
    for p in [invoked_path, argv0_prog, path, script] {
        if !p.is_empty() && !is_shell("", p) {
            paths.push(p);
        }
    }
    match_candidates(&names, &paths, ["", ""], configured)
}

/// Classify a bare command label (used only when a multiplexer snapshot carries
/// no argv or process table). Never matches a shell or a Group 2 command that
/// lacks a path clue.
pub fn classify_command_only<S: AsRef<str>>(command: &str, configured: &[S]) -> Option<String> {
    if is_shell(command, command) {
        return None;
    }
    let cleaned = clean_command_word(command);
    if cleaned.is_empty() {
        return None;
    }
    let names = [cleaned];
    let paths = [command];
    match_candidates(&names, &paths, ["", ""], configured)
}

/// Whether `(comm, args)` is the process for an already-identified `agent_name`
/// (such as Herdr's `"antigravity-cli"` or `"cursor"`, or Rex's `"claude-code"`).
pub(super) fn process_matches_agent(comm: &str, args: &str, agent_name: &str) -> bool {
    let target = agent_name.trim().to_ascii_lowercase();
    if target.is_empty() {
        return false;
    }
    let words: Vec<&str> = args.split_whitespace().collect();
    if words.first().is_some_and(|w| w.starts_with('-')) {
        return false;
    }
    let (program, script, sub) = dissect_argv(comm, &words);
    let (names, paths) = collect_candidates(comm, program, script);

    let mut matched_catalogue_target = false;
    for entry in CATALOGUE {
        if !entry_named(entry, &target) {
            continue;
        }
        matched_catalogue_target = true;
        if is_service_invocation(entry, sub) {
            continue;
        }
        if entry.is_group_1() {
            if names.iter().any(|n| {
                entry.commands.contains(&n.as_str()) || entry.aliases.contains(&n.as_str())
            }) {
                return true;
            }
        } else if names.iter().any(|n| entry.commands.contains(&n.as_str()))
            && (has_second_clue(entry, &paths)
                || names.iter().any(|n| n == entry.id && entry.aliases.contains(&n.as_str())))
        {
            return true;
        }
    }
    if matched_catalogue_target {
        return false;
    }

    let wanted = product(&target);
    names.iter().any(|n| product(n) == wanted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_catalogue_entry_has_a_confirmation_note_and_matches_default_agents() {
        let mut group1_ids = Vec::new();
        for entry in CATALOGUE {
            assert!(!entry.display.trim().is_empty());
            assert!(!entry.id.trim().is_empty());
            assert!(!entry.commands.is_empty(), "{} has no commands", entry.id);
            assert!(!entry.note.trim().is_empty(), "{} has no confirmation note", entry.id);
            if entry.is_group_1() {
                group1_ids.push(entry.id);
            }
        }
        assert_eq!(group1_ids.as_slice(), DEFAULT_AGENTS);
    }

    #[test]
    fn group_1_agents_and_multiplexer_aliases_resolve_cleanly() {
        assert_eq!(classify_process("agy", "agy", DEFAULT_AGENTS), Some("agy".into()));
        assert_eq!(
            classify_process("node", "node /home/u/.pi/install/node_modules/.bin/pi", DEFAULT_AGENTS),
            Some("pi".into())
        );
        assert_eq!(
            classify_process("node", "node /usr/local/bin/gemini", DEFAULT_AGENTS),
            Some("gemini".into())
        );
        assert_eq!(classify_process("kiro-cli", "kiro-cli", DEFAULT_AGENTS), Some("kiro".into()));
        assert_eq!(classify_process("kiro", "kiro", DEFAULT_AGENTS), Some("kiro".into()));
        assert_eq!(classify_process("qodercli", "qodercli", DEFAULT_AGENTS), Some("qodercli".into()));
        assert_eq!(classify_process("omp", "omp", DEFAULT_AGENTS), Some("omp".into()));

        assert!(process_matches_agent("agy", "agy", "antigravity-cli"));
        assert!(process_matches_agent("agy", "/opt/bin/agy", "antigravity"));
        assert!(process_matches_agent("kiro-cli", "kiro-cli", "kiro"));
        assert_eq!(canonical_command("antigravity-cli", DEFAULT_AGENTS), "agy");
        assert_eq!(canonical_command("claude-code", DEFAULT_AGENTS), "claude");
    }

    #[test]
    fn generic_group_2_commands_never_match_unrelated_programs_and_match_with_second_clue() {
        // Unrelated programs sharing Group 2 command names must NEVER match.
        for (comm, args) in [
            ("forge", "/home/u/.foundry/bin/forge test"),
            ("forge", "forge build"),
            ("vibe", "/usr/bin/vibe track.wav"),
            ("cn", "/usr/local/bin/cn status"),
            ("agent", "/usr/bin/agent --watch /home/u/.local/share/cursor-agent"),
            ("hermes", "python3 -c 'import hermes'"),
        ] {
            assert_eq!(classify_process(comm, args, DEFAULT_AGENTS), None, "{args}");
        }

        // With the install/script path clue, Group 2 entries match.
        assert_eq!(
            classify_process("forge", "/opt/homebrew/Cellar/forgecode/1.0/bin/forge", DEFAULT_AGENTS),
            Some("forgecode".into())
        );
        assert_eq!(
            classify_process("node", "node /usr/lib/node_modules/@antinomyhq/forge/dist/forge.js", DEFAULT_AGENTS),
            Some("forgecode".into())
        );
        assert_eq!(
            classify_process(
                "node",
                "/home/u/.local/bin/agent --use-system-ca /home/u/.local/share/cursor-agent/versions/2026.11/index.js",
                DEFAULT_AGENTS,
            ),
            Some("cursor-agent".into())
        );
        assert_eq!(
            classify_process("node", "node /usr/lib/node_modules/@continuedev/cli/dist/cn.js", DEFAULT_AGENTS),
            Some("continue".into())
        );
        assert_eq!(
            classify_process("python3", "/home/u/.local/share/uv/tools/mistral-vibe/bin/python3 /home/u/.local/bin/vibe", DEFAULT_AGENTS),
            Some("vibe".into())
        );

        // And Herdr's `cursor` alias matches Cursor's new `agent` launcher too,
        // without matching an unrelated `/usr/bin/agent`.
        assert!(process_matches_agent(
            "node",
            "/home/u/.local/bin/agent --use-system-ca /home/u/.local/share/cursor-agent/versions/2026.11/index.js",
            "cursor",
        ));
        assert!(!process_matches_agent("agent", "/usr/bin/agent", "cursor"));
    }

    #[test]
    fn background_service_subcommands_are_not_counted_as_agent_sessions() {
        for (comm, args) in [
            ("opencode", "opencode serve"),
            ("opencode", "opencode acp"),
            ("agy", "agy acp"),
            ("agy", "agy mic-serve"),
            ("devin", "devin acp"),
            ("claude", "claude gateway"),
            ("claude", "claude mcp serve"),
            ("codex", "codex app-server"),
            ("copilot", "copilot --acp"),
            ("kimi", "kimi web"),
        ] {
            assert_eq!(classify_process(comm, args, DEFAULT_AGENTS), None, "{args}");
        }
        // Prompt mode that mentions a service word after `-p` is still an agent run.
        assert_eq!(
            classify_process("claude", "claude -p gateway", DEFAULT_AGENTS),
            Some("claude".into())
        );
    }

    #[test]
    fn user_added_agents_setting_matches_custom_and_opted_in_names() {
        let mut custom: Vec<String> = DEFAULT_AGENTS.iter().map(|s| (*s).into()).collect();
        custom.push("my-custom-agent".into());
        assert_eq!(
            classify_process("my-custom-agent", "/opt/bin/my-custom-agent", &custom),
            Some("my-custom-agent".into())
        );
        assert!(matches_reported_app("my-custom-agent-cli", &custom));

        // Explicitly adding `forge` to `agents` opts it in without a path clue.
        custom.push("forge".into());
        assert_eq!(
            classify_process("forge", "/usr/local/bin/forge", &custom),
            Some("forge".into())
        );
    }

    #[test]
    fn shell_wrappers_and_narrowed_agents_list_behave_honestly() {
        // Shebang shell launchers resolve on both Linux (comm = script) and macOS (comm = sh).
        assert_eq!(
            classify_process("pi", "/bin/sh /home/u/.pi/agent/bin/pi", DEFAULT_AGENTS),
            Some("pi".into())
        );
        assert_eq!(
            classify_process("sh", "/bin/sh /home/u/.pi/agent/bin/pi", DEFAULT_AGENTS),
            Some("pi".into())
        );
        // Interactive shells and `-c` command shells never match an agent.
        for (comm, args) in [
            ("zsh", "-zsh"),
            ("bash", "/bin/bash -l"),
            ("bash", "/bin/bash -c claude"),
            ("sh", "/bin/sh /srv/repo/build.sh"),
        ] {
            assert_eq!(classify_process(comm, args, DEFAULT_AGENTS), None, "{args}");
        }

        // Narrowing `agents` to `["claude"]` excludes Group 2 entries too unless named.
        let claude_only = ["claude"];
        assert_eq!(
            classify_process(
                "node",
                "/home/u/.local/bin/agent --use-system-ca /home/u/.local/share/cursor-agent/versions/2026.11/index.js",
                &claude_only,
            ),
            None
        );
        assert_eq!(
            classify_process(
                "forge",
                "/opt/homebrew/Cellar/forgecode/1.0/bin/forge",
                &claude_only,
            ),
            None
        );
    }
}
