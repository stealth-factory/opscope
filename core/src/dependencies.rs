// opscope - small dependency-free terminal widgets
// Copyright (C) 2026 William Li
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU Affero General Public License as published
// by the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.

//! Widget-owned external dependencies and host-owned installation advice.

use semver::{Version, VersionReq};
use std::collections::{BTreeMap, BTreeSet};
use std::os::unix::fs::PermissionsExt;

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Platform {
    Any,
    Linux,
    Macos,
}

impl Platform {
    pub fn current() -> Self {
        match std::env::consts::OS {
            "linux" => Self::Linux,
            "macos" => Self::Macos,
            _ => Self::Any,
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "linux" => Some(Self::Linux),
            "macos" => Some(Self::Macos),
            "any" => Some(Self::Any),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum Tool {
    Cloudflared,
    Curl,
    Getent,
    Herdr,
    Ifconfig,
    Ip,
    Lsof,
    Netstat,
    Nettop,
    Ping,
    Ps,
    Script,
    Ss,
    Tailscale,
    Who,
}

impl Tool {
    fn parse(value: &str) -> Option<Self> {
        match value {
            "cloudflared" => Some(Self::Cloudflared),
            "curl" => Some(Self::Curl),
            "getent" => Some(Self::Getent),
            "herdr" => Some(Self::Herdr),
            "ifconfig" => Some(Self::Ifconfig),
            "ip" => Some(Self::Ip),
            "lsof" => Some(Self::Lsof),
            "netstat" => Some(Self::Netstat),
            "nettop" => Some(Self::Nettop),
            "ping" => Some(Self::Ping),
            "ps" => Some(Self::Ps),
            "script" => Some(Self::Script),
            "ss" => Some(Self::Ss),
            "tailscale" => Some(Self::Tailscale),
            "who" => Some(Self::Who),
            _ => None,
        }
    }

    pub const fn command(self) -> &'static str {
        match self {
            Self::Cloudflared => "cloudflared",
            Self::Curl => "curl",
            Self::Getent => "getent",
            Self::Herdr => "herdr",
            Self::Ifconfig => "ifconfig",
            Self::Ip => "ip",
            Self::Lsof => "lsof",
            Self::Netstat => "netstat",
            Self::Nettop => "/usr/bin/nettop",
            Self::Ping => "ping",
            Self::Ps => "ps",
            Self::Script => "/usr/bin/script",
            Self::Ss => "ss",
            Self::Tailscale => "tailscale",
            Self::Who => "who",
        }
    }

    fn separate_install(self) -> Option<&'static str> {
        match self {
            Self::Tailscale => Some("https://tailscale.com/download"),
            Self::Herdr => Some("https://herdr.dev"),
            Self::Cloudflared => Some(
                "https://developers.cloudflare.com/cloudflare-one/connections/connect-networks/downloads/",
            ),
            _ => None,
        }
    }

    fn version_args(self) -> Option<&'static [&'static str]> {
        match self {
            Self::Cloudflared | Self::Curl | Self::Herdr => Some(&["--version"]),
            Self::Ip | Self::Ss | Self::Ping => Some(&["-V"]),
            Self::Tailscale => Some(&["version"]),
            _ => None,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Dependency {
    pub tool: Tool,
    pub version: VersionReq,
    pub version_text: String,
    pub platforms: Vec<Platform>,
    pub why: Option<String>,
}

impl Dependency {
    pub fn applies_to(&self, platform: Platform) -> bool {
        self.platforms.is_empty()
            || self.platforms.contains(&Platform::Any)
            || self.platforms.contains(&platform)
    }
}

#[derive(Clone, Debug, Default)]
pub struct Dependencies {
    pub required: Vec<Dependency>,
    pub recommended: Vec<Dependency>,
}

/// Parse the npm-shaped file each widget owns.
///
/// An entry may be a range string (`"curl": ">=8"`) or an object carrying
/// `version`, `platforms`, and the optional user-facing `why` message.
pub fn parse_dependencies(text: &str) -> Result<Dependencies, String> {
    let root: serde_json::Value =
        serde_json::from_str(text).map_err(|e| format!("invalid JSON: {e}"))?;
    let object = root
        .as_object()
        .ok_or_else(|| "dependency file must be a JSON object".to_string())?;
    for key in object.keys() {
        if key != "required" && key != "recommended" {
            return Err(format!("unknown top-level dependency key {key:?}"));
        }
    }
    Ok(Dependencies {
        required: parse_tier(object.get("required"), "required")?,
        recommended: parse_tier(object.get("recommended"), "recommended")?,
    })
}

fn parse_tier(value: Option<&serde_json::Value>, tier: &str) -> Result<Vec<Dependency>, String> {
    let Some(value) = value else {
        return Ok(Vec::new());
    };
    let entries = value
        .as_object()
        .ok_or_else(|| format!("{tier} must be an object"))?;
    entries
        .iter()
        .map(|(name, value)| parse_dependency(name, value).map_err(|e| format!("{tier}.{name}: {e}")))
        .collect()
}

fn parse_dependency(name: &str, value: &serde_json::Value) -> Result<Dependency, String> {
    let tool = Tool::parse(name).ok_or_else(|| format!("unknown tool {name:?}"))?;
    let (version_text, platforms, why) = if let Some(version) = value.as_str() {
        (version.to_string(), Vec::new(), None)
    } else {
        let object = value
            .as_object()
            .ok_or_else(|| "must be a version string or object".to_string())?;
        for key in object.keys() {
            if key != "version" && key != "platforms" && key != "why" {
                return Err(format!("unknown field {key:?}"));
            }
        }
        let version = object
            .get("version")
            .and_then(|v| v.as_str())
            .unwrap_or("*")
            .to_string();
        let platforms = object
            .get("platforms")
            .map(|value| {
                value
                    .as_array()
                    .ok_or_else(|| "platforms must be an array".to_string())?
                    .iter()
                    .map(|value| {
                        let value = value
                            .as_str()
                            .ok_or_else(|| "platform must be a string".to_string())?;
                        Platform::parse(value)
                            .ok_or_else(|| format!("unknown platform {value:?}"))
                    })
                    .collect::<Result<Vec<_>, _>>()
            })
            .transpose()?
            .unwrap_or_default();
        let why = object
            .get("why")
            .map(|value| {
                value
                    .as_str()
                    .map(str::to_string)
                    .ok_or_else(|| "why must be a string".to_string())
            })
            .transpose()?;
        (version, platforms, why)
    };
    let version = VersionReq::parse(&version_text)
        .map_err(|e| format!("invalid version range {version_text:?}: {e}"))?;
    if version != VersionReq::STAR && tool.version_args().is_none() {
        return Err(format!(
            "{} does not have a shared version probe; use \"*\" or add one to core",
            tool.command()
        ));
    }
    Ok(Dependency {
        tool,
        version,
        version_text,
        platforms,
        why,
    })
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LinuxFamily {
    Alpine,
    Arch,
    Debian,
    Fedora,
    Unknown,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Host {
    pub platform: Platform,
    pub linux: LinuxFamily,
    pub name: String,
}

impl Host {
    pub fn detect() -> Self {
        match Platform::current() {
            Platform::Linux => {
                let text = std::fs::read_to_string("/etc/os-release")
                    .or_else(|_| std::fs::read_to_string("/usr/lib/os-release"))
                    .unwrap_or_default();
                Self::from_os_release(&text)
            }
            Platform::Macos => Self {
                platform: Platform::Macos,
                linux: LinuxFamily::Unknown,
                name: "macOS".into(),
            },
            Platform::Any => Self {
                platform: Platform::Any,
                linux: LinuxFamily::Unknown,
                name: std::env::consts::OS.into(),
            },
        }
    }

    pub fn from_os_release(text: &str) -> Self {
        let fields = parse_os_release(text);
        let id = fields.get("ID").map(String::as_str).unwrap_or("");
        let likes = fields
            .get("ID_LIKE")
            .map(|s| s.split_whitespace().collect::<Vec<_>>())
            .unwrap_or_default();
        let related = |name: &str| id == name || likes.contains(&name);
        let linux = if related("alpine") {
            LinuxFamily::Alpine
        } else if related("arch") {
            LinuxFamily::Arch
        } else if related("debian") || related("ubuntu") {
            LinuxFamily::Debian
        } else if related("fedora") || related("rhel") || related("centos") {
            LinuxFamily::Fedora
        } else {
            LinuxFamily::Unknown
        };
        Self {
            platform: Platform::Linux,
            linux,
            name: fields
                .get("PRETTY_NAME")
                .or_else(|| fields.get("NAME"))
                .cloned()
                .unwrap_or_else(|| "Linux".into()),
        }
    }
}

/// Parse `os-release` as data; never source it as shell code.
pub fn parse_os_release(text: &str) -> BTreeMap<String, String> {
    let mut out = BTreeMap::new();
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((key, raw)) = line.split_once('=') else {
            continue;
        };
        let raw = raw.trim();
        let value = if raw.len() >= 2
            && ((raw.starts_with('"') && raw.ends_with('"'))
                || (raw.starts_with('\'') && raw.ends_with('\'')))
        {
            &raw[1..raw.len() - 1]
        } else {
            raw
        };
        let mut clean = String::new();
        let mut escaped = false;
        for ch in value.chars() {
            if escaped {
                clean.push(ch);
                escaped = false;
            } else if ch == '\\' {
                escaped = true;
            } else {
                clean.push(ch);
            }
        }
        if escaped {
            clean.push('\\');
        }
        out.insert(key.trim().to_string(), clean);
    }
    out
}

fn executable(path: &std::path::Path) -> bool {
    path.is_file()
        && std::fs::metadata(path)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
}

pub fn tool_available(tool: Tool) -> bool {
    let command = tool.command();
    let direct = std::path::Path::new(command);
    if direct.is_absolute() {
        return executable(direct);
    }
    std::env::var("PATH")
        .unwrap_or_default()
        .split(':')
        .any(|dir| executable(&std::path::Path::new(dir).join(command)))
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DependencyStatus {
    Available(Option<Version>),
    Missing,
    TooOld(Version),
    VersionUnknown,
}

impl DependencyStatus {
    pub fn satisfies(&self) -> bool {
        matches!(self, Self::Available(_))
    }
}

pub fn dependency_status(dependency: &Dependency) -> DependencyStatus {
    if !tool_available(dependency.tool) {
        return DependencyStatus::Missing;
    }
    if dependency.version == VersionReq::STAR {
        return DependencyStatus::Available(None);
    }
    match installed_version(dependency.tool) {
        Some(version) if dependency.version.matches(&version) => {
            DependencyStatus::Available(Some(version))
        }
        Some(version) => DependencyStatus::TooOld(version),
        None => DependencyStatus::VersionUnknown,
    }
}

fn installed_version(tool: Tool) -> Option<Version> {
    let args = tool.version_args()?;
    let mut command = vec![tool.command()];
    command.extend_from_slice(args);
    let output = crate::run_full(&command, 2).ok()?;
    let text = format!(
        "{} {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    parse_version(&text)
}

fn parse_version(text: &str) -> Option<Version> {
    for token in text.split_whitespace() {
        for (start, ch) in token.char_indices() {
            if !ch.is_ascii_digit() {
                continue;
            }
            let core: String = token[start..]
                .chars()
                .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
                .collect();
            if let Some(version) = parse_yyyymmdd(&core) {
                return Some(version);
            }
            let normalized = match core.matches('.').count() {
                1 => format!("{core}.0"),
                2 => core,
                _ => continue,
            };
            if let Ok(version) = Version::parse(&normalized) {
                return Some(version);
            }
        }
    }
    None
}

/// iputils prints snapshot dates such as `s20220713` or `iputils-20190709`.
fn parse_yyyymmdd(digits: &str) -> Option<Version> {
    if digits.len() != 8 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let year: u64 = digits[0..4].parse().ok()?;
    let month: u64 = digits[4..6].parse().ok()?;
    let day: u64 = digits[6..8].parse().ok()?;
    if !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some(Version::new(year, month, day))
}

pub fn unsatisfied_required(
    dependencies: &Dependencies,
    platform: Platform,
) -> Vec<(Dependency, DependencyStatus)> {
    dependencies
        .required
        .iter()
        .filter(|dependency| dependency.applies_to(platform))
        .filter_map(|dependency| {
            let status = dependency_status(dependency);
            (!status.satisfies()).then(|| (dependency.clone(), status))
        })
        .collect()
}

fn package(tool: Tool, host: &Host) -> Option<&'static str> {
    if host.platform == Platform::Macos {
        return match tool {
            Tool::Tailscale => Some("tailscale"),
            Tool::Cloudflared => Some("cloudflared"),
            _ => None,
        };
    }
    match host.linux {
        LinuxFamily::Debian => match tool {
            Tool::Curl => Some("curl"),
            Tool::Getent => Some("libc-bin"),
            Tool::Ifconfig | Tool::Netstat => Some("net-tools"),
            Tool::Ip | Tool::Ss => Some("iproute2"),
            Tool::Lsof => Some("lsof"),
            Tool::Ping => Some("iputils-ping"),
            Tool::Ps => Some("procps"),
            Tool::Script => Some("util-linux"),
            Tool::Who => Some("coreutils"),
            _ => None,
        },
        LinuxFamily::Fedora => match tool {
            Tool::Curl => Some("curl"),
            Tool::Getent => Some("glibc-common"),
            Tool::Ifconfig | Tool::Netstat => Some("net-tools"),
            Tool::Ip | Tool::Ss => Some("iproute"),
            Tool::Lsof => Some("lsof"),
            Tool::Ping => Some("iputils"),
            Tool::Ps => Some("procps-ng"),
            Tool::Script => Some("util-linux"),
            Tool::Who => Some("coreutils"),
            _ => None,
        },
        LinuxFamily::Arch => match tool {
            Tool::Curl => Some("curl"),
            Tool::Getent => Some("glibc"),
            Tool::Ifconfig | Tool::Netstat => Some("net-tools"),
            Tool::Ip | Tool::Ss => Some("iproute2"),
            Tool::Lsof => Some("lsof"),
            Tool::Ping => Some("iputils"),
            Tool::Ps => Some("procps-ng"),
            Tool::Script => Some("util-linux"),
            Tool::Who => Some("coreutils"),
            _ => None,
        },
        LinuxFamily::Alpine => match tool {
            Tool::Curl => Some("curl"),
            Tool::Getent => Some("libc-utils"),
            Tool::Ifconfig | Tool::Netstat => Some("net-tools"),
            Tool::Ip | Tool::Ss => Some("iproute2"),
            Tool::Lsof => Some("lsof"),
            Tool::Ping => Some("iputils"),
            Tool::Ps => Some("procps"),
            Tool::Script => Some("util-linux"),
            Tool::Who => Some("coreutils"),
            _ => None,
        },
        LinuxFamily::Unknown => None,
    }
}

fn install_prefix(host: &Host) -> Option<&'static str> {
    if host.platform == Platform::Macos {
        return Some("brew install");
    }
    match host.linux {
        LinuxFamily::Debian => Some("sudo apt install"),
        LinuxFamily::Fedora => Some("sudo dnf install"),
        LinuxFamily::Arch => Some("sudo pacman -S"),
        LinuxFamily::Alpine => Some("apk add"),
        LinuxFamily::Unknown => None,
    }
}

pub fn install_command<'a>(
    host: &Host,
    dependencies: impl IntoIterator<Item = &'a Dependency>,
) -> Option<String> {
    let packages: BTreeSet<&str> = dependencies
        .into_iter()
        .filter_map(|dependency| package(dependency.tool, host))
        .collect();
    let prefix = install_prefix(host)?;
    (!packages.is_empty()).then(|| {
        format!(
            "{} {}",
            prefix,
            packages.into_iter().collect::<Vec<_>>().join(" ")
        )
    })
}

pub fn dependency_install_hint(host: &Host, dependencies: &[Dependency]) -> String {
    let mut parts = Vec::new();
    if let Some(command) = install_command(host, dependencies) {
        parts.push(command);
    }
    let separate: Vec<String> = dependencies
        .iter()
        .filter(|dependency| package(dependency.tool, host).is_none())
        .filter_map(|dependency| {
            dependency
                .tool
                .separate_install()
                .map(|url| format!("install {} from {}", dependency.tool.command(), url))
        })
        .collect();
    if !separate.is_empty() {
        parts.push(separate.join("; "));
    }
    if !parts.is_empty() {
        return parts.join("; ");
    }
    if host.platform == Platform::Macos {
        "restore the missing macOS system tool".into()
    } else {
        format!(
            "install {} with this system's package manager",
            dependencies
                .iter()
                .map(|dependency| dependency.tool.command())
                .collect::<Vec<_>>()
                .join(", ")
        )
    }
}

#[derive(Default)]
struct ToolUse {
    required_by: BTreeSet<String>,
    recommended_by: BTreeSet<String>,
    required_why: BTreeSet<String>,
    recommended_why: BTreeSet<String>,
    required: Vec<Dependency>,
    recommended: Vec<Dependency>,
}

pub fn doctor_report(host: &Host, widgets: &[(&str, &str)]) -> Result<String, String> {
    let mut tools: BTreeMap<Tool, ToolUse> = BTreeMap::new();
    for (widget, source) in widgets {
        let dependencies =
            parse_dependencies(source).map_err(|e| format!("{widget}/dependencies.json: {e}"))?;
        for (required, list) in [
            (true, dependencies.required),
            (false, dependencies.recommended),
        ] {
            for dependency in list {
                if !dependency.applies_to(host.platform) {
                    continue;
                }
                let usage = tools.entry(dependency.tool).or_default();
                if required {
                    usage.required_by.insert((*widget).to_string());
                    if let Some(why) = &dependency.why {
                        usage.required_why.insert(why.clone());
                    }
                    usage.required.push(dependency);
                } else {
                    usage.recommended_by.insert((*widget).to_string());
                    if let Some(why) = &dependency.why {
                        usage.recommended_why.insert(why.clone());
                    }
                    usage.recommended.push(dependency);
                }
            }
        }
    }

    let status_by_tool: BTreeMap<Tool, (Vec<DependencyStatus>, Vec<DependencyStatus>)> = tools
        .iter()
        .map(|(tool, usage)| {
            (
                *tool,
                (
                    usage.required.iter().map(dependency_status).collect(),
                    usage.recommended.iter().map(dependency_status).collect(),
                ),
            )
        })
        .collect();

    let mut out = vec![format!("Host: {}", host.name)];
    for (heading, required) in [("Required", true), ("Recommended", false)] {
        out.push(String::new());
        out.push(heading.into());
        let mut count = 0;
        for (tool, usage) in &tools {
            let widgets = if required {
                &usage.required_by
            } else {
                &usage.recommended_by
            };
            if widgets.is_empty() {
                continue;
            }
            count += 1;
            let (required_status, recommended_status) = status_by_tool
                .get(tool)
                .expect("every reported tool was evaluated");
            let (dependencies, statuses) = if required {
                (&usage.required, required_status)
            } else {
                (&usage.recommended, recommended_status)
            };
            let (checked, status) = dependencies
                .iter()
                .zip(statuses)
                .find(|(_, status)| !status.satisfies())
                .or_else(|| dependencies.first().zip(statuses.first()))
                .expect("a reported tool has at least one declaration");
            let label = match status {
                DependencyStatus::Available(Some(version)) => format!("ok {version}"),
                DependencyStatus::Available(None) => "ok".into(),
                DependencyStatus::Missing => "missing".into(),
                DependencyStatus::TooOld(version) => {
                    format!("old {version} (needs {})", checked.version_text)
                }
                DependencyStatus::VersionUnknown => {
                    format!("version unknown (needs {})", checked.version_text)
                }
            };
            out.push(format!(
                "  {:<15} {:<12} {}",
                label,
                tool.command(),
                widgets.iter().cloned().collect::<Vec<_>>().join(", ")
            ));
            let reasons = if required {
                &usage.required_why
            } else {
                &usage.recommended_why
            };
            for why in reasons {
                out.push(format!("     {why}"));
            }
        }
        if count == 0 {
            out.push("  none".into());
        }
    }

    let missing: Vec<&Dependency> = tools
        .iter()
        .flat_map(|(tool, usage)| {
            let (required_status, recommended_status) = status_by_tool
                .get(tool)
                .expect("every reported tool was evaluated");
            usage
                .required
                .iter()
                .zip(required_status)
                .chain(usage.recommended.iter().zip(recommended_status))
                .filter(|(_, status)| !status.satisfies())
                .map(|(dependency, _)| dependency)
        })
        .collect();
    if let Some(command) = install_command(host, missing.iter().copied()) {
        out.push(String::new());
        out.push("Install missing packages (review, then run):".into());
        out.push(format!("  {command}"));
    }
    let separate: BTreeSet<(Tool, &str)> = missing
        .iter()
        .filter_map(|dependency| {
            (package(dependency.tool, host).is_none())
                .then(|| dependency.tool.separate_install())
                .flatten()
                .map(|url| (dependency.tool, url))
        })
        .collect();
    if !separate.is_empty() {
        out.push(String::new());
        out.push("Install separately:".into());
        for (tool, url) in separate {
            out.push(format!("  {:<12} {url}", tool.command()));
        }
    }
    let unresolved: BTreeSet<&str> = missing
        .iter()
        .filter(|dependency| {
            package(dependency.tool, host).is_none()
                && dependency.tool.separate_install().is_none()
        })
        .map(|dependency| dependency.tool.command())
        .collect();
    if !unresolved.is_empty() {
        out.push(String::new());
        if host.platform == Platform::Macos {
            out.push("macOS system tools unexpectedly missing:".into());
            out.push(format!(
                "  {}",
                unresolved.into_iter().collect::<Vec<_>>().join(" ")
            ));
            out.push("  Restore them with macOS; opscope will not replace system tools.".into());
        } else {
            out.push("Install with this system's package manager:".into());
            out.push(format!(
                "  {}",
                unresolved.into_iter().collect::<Vec<_>>().join(" ")
            ));
        }
    }
    out.push(String::new());
    out.push("Nothing was installed.".into());
    Ok(out.join("\n"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn declaration_has_two_tiers_versions_platforms_and_optional_why() {
        let got = parse_dependencies(
            r#"{
                "required": {"curl": ">=8", "ss": {"version": "*", "platforms": ["linux"], "why": "socket facts"}},
                "recommended": {"herdr": "^1.2"}
            }"#,
        )
        .unwrap();
        assert_eq!(got.required.len(), 2);
        assert_eq!(got.recommended.len(), 1);
        let ss = got.required.iter().find(|d| d.tool == Tool::Ss).unwrap();
        assert_eq!(ss.why.as_deref(), Some("socket facts"));
        assert_eq!(ss.platforms, vec![Platform::Linux]);
        assert!(got.recommended[0].version.matches(&Version::new(1, 3, 0)));
    }

    #[test]
    fn unknown_tools_and_fields_are_refused() {
        assert!(parse_dependencies(r#"{"required":{"made-up":"*"}}"#).is_err());
        assert!(
            parse_dependencies(r#"{"required":{"curl":{"typo":"yes"}}}"#).is_err()
        );
        assert!(parse_dependencies(r#"{"required":{"lsof":">=4"}}"#).is_err());
    }

    #[test]
    fn distro_family_uses_id_then_id_like() {
        assert_eq!(
            Host::from_os_release("ID=ubuntu\nID_LIKE=debian\nPRETTY_NAME=Ubuntu").linux,
            LinuxFamily::Debian
        );
        assert_eq!(
            Host::from_os_release("ID=rocky\nID_LIKE=\"rhel centos fedora\"").linux,
            LinuxFamily::Fedora
        );
        assert_eq!(Host::from_os_release("ID=alpine").linux, LinuxFamily::Alpine);
        assert_eq!(Host::from_os_release("ID=arch").linux, LinuxFamily::Arch);
    }

    #[test]
    fn distro_parser_does_not_execute_shell() {
        let got = parse_os_release("ID=debian\nPRETTY_NAME=\"Debian \\\"Bookworm\\\"\"\nBAD\n");
        assert_eq!(got.get("ID").map(String::as_str), Some("debian"));
        assert_eq!(
            got.get("PRETTY_NAME").map(String::as_str),
            Some("Debian \"Bookworm\"")
        );
        assert!(!got.contains_key("BAD"));
    }

    #[test]
    fn version_finder_handles_the_common_command_shapes() {
        assert_eq!(parse_version("curl 8.5.0 (x86_64)"), Some(Version::new(8, 5, 0)));
        assert_eq!(
            parse_version("ss utility, iproute2-6.1.0"),
            Some(Version::new(6, 1, 0))
        );
        assert_eq!(parse_version("1.74.1\n  tailscale commit"), Some(Version::new(1, 74, 1)));
        assert_eq!(
            parse_version("ping from iputils s20220713"),
            Some(Version::new(2022, 7, 13))
        );
        assert_eq!(
            parse_version("ping utility, iputils-20190709"),
            Some(Version::new(2019, 7, 9))
        );
        assert_eq!(parse_version("s20221399"), None);
    }

    #[test]
    fn package_commands_are_native_and_deduplicated() {
        let dependencies = parse_dependencies(
            r#"{"required":{"ip":"*","ss":"*","ping":"*","curl":"*"}}"#,
        )
        .unwrap();
        assert_eq!(
            install_command(&Host::from_os_release("ID=debian"), &dependencies.required),
            Some("sudo apt install curl iproute2 iputils-ping".into())
        );
        assert_eq!(
            install_command(&Host::from_os_release("ID=fedora"), &dependencies.required),
            Some("sudo dnf install curl iproute iputils".into())
        );
        assert_eq!(
            install_command(&Host::from_os_release("ID=alpine"), &dependencies.required),
            Some("apk add curl iproute2 iputils".into())
        );
        assert_eq!(
            install_command(&Host::from_os_release("ID=arch"), &dependencies.required),
            Some("sudo pacman -S curl iproute2 iputils".into())
        );
    }

    #[test]
    fn macos_only_offers_homebrew_for_tools_it_can_supply() {
        let dependencies = parse_dependencies(
            r#"{"recommended":{"cloudflared":"*","tailscale":"*","lsof":"*"}}"#,
        )
        .unwrap();
        let host = Host {
            platform: Platform::Macos,
            linux: LinuxFamily::Unknown,
            name: "macOS".into(),
        };
        assert_eq!(
            install_command(&host, &dependencies.recommended),
            Some("brew install cloudflared tailscale".into())
        );
    }

    #[test]
    fn mixed_package_and_vendor_tools_keep_both_install_hints() {
        let dependencies = parse_dependencies(
            r#"{"required":{"curl":"*","herdr":"*"}}"#,
        )
        .unwrap();
        let hint = dependency_install_hint(
            &Host::from_os_release("ID=debian"),
            &dependencies.required,
        );
        assert!(hint.contains("sudo apt install curl"), "{hint}");
        assert!(hint.contains("install herdr from https://herdr.dev"), "{hint}");
    }

    #[test]
    fn unknown_linux_never_guesses_a_package_manager() {
        let dependencies = parse_dependencies(r#"{"required":{"ping":"*"}}"#).unwrap();
        let host = Host::from_os_release("ID=gentoo\nPRETTY_NAME=Gentoo");
        assert_eq!(host.linux, LinuxFamily::Unknown);
        assert_eq!(install_command(&host, &dependencies.required), None);
        let report = doctor_report(&host, &[("latency", r#"{"required":{"ping":"*"}}"#)])
            .unwrap();
        if !tool_available(Tool::Ping) {
            assert!(report.contains("Install with this system's package manager:\n  ping"));
        }
    }

    #[test]
    fn doctor_keeps_required_and_recommended_reasons_apart() {
        let widgets = [
            (
                "must",
                r#"{"required":{"curl":{"why":"required reason"}}}"#,
            ),
            (
                "nice",
                r#"{"recommended":{"curl":{"why":"recommended reason"}}}"#,
            ),
        ];
        let report = doctor_report(&Host::from_os_release("ID=debian"), &widgets).unwrap();
        let (required, recommended) = report.split_once("\nRecommended\n").unwrap();
        assert!(required.contains("required reason"));
        assert!(!required.contains("recommended reason"));
        assert!(recommended.contains("recommended reason"));
    }
}
