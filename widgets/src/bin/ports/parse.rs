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

//! Parsers for every source this widget reads, always compiled.
//!
//! Acquisition lives in `linux.rs` / `macos.rs` and is gated by
//! `cfg(target_os)`. These functions take a `&str`, so they compile on
//! every target and their fixture tests run on the macOS runners. The
//! other platform's parser is unused in a given binary; that is the
//! cost of not having a test that vanishes from a green build.
#![allow(dead_code)]

/// A listening TCP socket as `/proc/net/tcp` writes it.
pub struct ProcSocket {
    pub port: u16,
    pub bind: String,
    pub inode: String,
    pub uid: u32,
}

/// A listening TCP socket as `lsof -F` writes it.
pub struct LsofSocket {
    pub port: u16,
    pub bind: String,
    pub uid: u32,
    pub pid: Option<i32>,
}

/// Every listener in one `/proc/net/tcp` or `/proc/net/tcp6` table.
pub fn parse_proc_net_tcp(text: &str) -> Vec<ProcSocket> {
    let mut out = Vec::new();
    for line in text.lines().skip(1) {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() < 10 || cols[3] != "0A" {
            continue;
        }
        let (addr, port) = match cols[1].rsplit_once(':') {
            Some(pair) => pair,
            None => continue,
        };
        let port = match u16::from_str_radix(port, 16) {
            Ok(p) => p,
            Err(_) => continue,
        };
        out.push(ProcSocket {
            port,
            bind: super::hex_addr(addr),
            inode: cols[9].to_string(),
            // The uid is in the table even where the process behind it
            // is not reachable, which is the difference between
            // "somebody else's" and "a mystery".
            uid: cols[7].parse().unwrap_or(0),
        });
    }
    out
}

/// Every listener in `lsof -nP -iTCP -sTCP:LISTEN -Fpcunt` output.
///
/// Field-per-line, not the human table: that one reflows with width.
/// `*` means every interface, and the `t` field says which family, so
/// `*:3000` on IPv4 is `0.0.0.0` and on IPv6 is `::`.
pub fn parse_lsof_listen(text: &str) -> Vec<LsofSocket> {
    let mut pid = None;
    let mut uid = 0u32;
    let mut typ = String::new();
    let mut out = Vec::new();
    for line in text.lines() {
        let Some(code) = line.chars().next() else {
            continue;
        };
        let val = &line[code.len_utf8()..];
        match code {
            'p' => {
                pid = val.parse().ok();
                uid = 0;
                typ.clear();
            }
            'u' => uid = val.parse().unwrap_or(0),
            't' => typ = val.to_string(),
            'n' => {
                if val.contains("->") {
                    continue;
                }
                if let Some((bind, port)) = parse_lsof_name(val, &typ) {
                    out.push(LsofSocket {
                        port,
                        bind,
                        uid,
                        pid,
                    });
                }
            }
            _ => {}
        }
    }
    out
}

fn parse_lsof_name(name: &str, typ: &str) -> Option<(String, u16)> {
    let (addr, port) = name.rsplit_once(':')?;
    let port: u16 = port.parse().ok()?;
    let addr = addr.trim_start_matches('[').trim_end_matches(']');
    let bind = if addr == "*" {
        if typ.contains("IPv6") {
            "::".into()
        } else {
            "0.0.0.0".into()
        }
    } else {
        addr.to_string()
    };
    Some((bind, port))
}

/// The cwd path out of `lsof -a -p PID -d cwd -Fn`.
pub fn parse_lsof_cwd(text: &str) -> String {
    for line in text.lines() {
        let Some(path) = line.strip_prefix('n') else {
            continue;
        };
        if path.starts_with('/') || path.ends_with("(deleted)") {
            return path.to_string();
        }
    }
    String::new()
}

/// Elapsed seconds from `ps -o etimes=`.
pub fn parse_ps_etimes(text: &str) -> Option<f64> {
    text.split_whitespace().next()?.parse().ok()
}

/// A uid from `ps -o uid=`.
pub fn parse_ps_uid(text: &str) -> Option<u32> {
    text.split_whitespace().next()?.parse().ok()
}

/// Whether `ps -o state=` names a zombie.
pub fn parse_ps_state_zombie(text: &str) -> bool {
    text.split_whitespace()
        .next()
        .is_some_and(|s| s.starts_with('Z'))
}

/// Whether a `/proc/<pid>/stat` line names a zombie.
///
/// The comm field is in parens and can contain spaces, so the state is
/// the token after the last `)`, not a fixed column.
pub fn parse_proc_stat_zombie(text: &str) -> bool {
    match text.rsplit_once(')') {
        Some((_, rest)) => rest.split_whitespace().next() == Some("Z"),
        None => false,
    }
}

/// Addresses from `ip -j addr`, skipping link-local.
pub fn parse_ip_json(text: &str) -> Vec<(String, String, bool)> {
    let data: serde_json::Value = serde_json::from_str(text).unwrap_or(serde_json::Value::Null);
    let mut found = Vec::new();
    for link in data.as_array().unwrap_or(&Vec::new()) {
        let name = link["ifname"].as_str().unwrap_or("?").to_string();
        for addr in link["addr_info"].as_array().unwrap_or(&Vec::new()) {
            let ip = addr["local"].as_str().unwrap_or("");
            if ip.is_empty() || ip.starts_with("fe80:") {
                continue;
            }
            found.push((
                name.clone(),
                ip.to_string(),
                addr["family"].as_str() == Some("inet6"),
            ));
        }
    }
    found
}

/// Addresses from `ifconfig -a`, skipping link-local.
pub fn parse_ifconfig(text: &str) -> Vec<(String, String, bool)> {
    let mut name = String::new();
    let mut found = Vec::new();
    for line in text.lines() {
        let cols: Vec<&str> = line.split_whitespace().collect();
        if cols.len() >= 2 && (cols[0] == "inet" || cols[0] == "inet6") {
            let mut ip = cols[1].to_string();
            if let Some((bare, _)) = ip.split_once('%') {
                ip = bare.to_string();
            }
            if ip.starts_with("fe80:") {
                continue;
            }
            found.push((name.clone(), ip, cols[0] == "inet6"));
            continue;
        }
        if !line.starts_with(' ') && !line.starts_with('\t') {
            if let Some((n, _)) = line.split_once(':') {
                name = n.to_string();
            }
        }
    }
    found
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn proc_net_tcp_listeners_are_read_from_the_kernel_table() {
        // A captured `/proc/net/tcp`: one listener on 127.0.0.1:8080
        // (state 0A), uid 1000, inode 12345, and an established row that
        // must not count. This test has no cfg(target_os) on purpose: if
        // the parser were gated to linux, cargo test on the macOS runners
        // would not compile this, and a broken parser could sit behind a
        // green build.
        let text = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             0: 0100007F:1F90 00000000:0000 0A 00000000:00000000 00:00000000 00000000  1000        0 12345 1 0000000000000000 100 0 0 10 0\n\
             1: 0100007F:1F91 0100007F:0050 01 00000000:00000000 00:00000000 00000000  1000        0 12346 1 0000000000000000 100 0 0 10 0\n";
        let got = parse_proc_net_tcp(text);
        assert_eq!(
            got.len(),
            1,
            "the established row was counted as a listener"
        );
        assert_eq!(got[0].port, 8080);
        assert_eq!(got[0].bind, "127.0.0.1");
        assert_eq!(got[0].uid, 1000);
        assert_eq!(got[0].inode, "12345");
    }

    #[test]
    fn proc_net_tcp6_listeners_decode_the_same_way() {
        // ::1 as /proc/net/tcp6 writes it, listening on 443.
        let text = "  sl  local_address                         remote_address                        st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode\n\
             0: 00000000000000000000000001000000:01BB 00000000000000000000000000000000:0000 0A 00000000:00000000 00:00000000 00000000     0        0 99 1 0000000000000000 100 0 0 10 0\n";
        let got = parse_proc_net_tcp(text);
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].port, 443);
        assert_eq!(got[0].bind, "::1");
        assert_eq!(got[0].uid, 0);
    }

    #[test]
    fn lsof_listeners_carry_the_pid() {
        let text = "\
p41220\n\
cnext-server\n\
u501\n\
tIPv4\n\
n*:3000\n\
p41220\n\
cnext-server\n\
u501\n\
tIPv6\n\
n*:3000\n\
p99\n\
csshd\n\
u0\n\
tIPv4\n\
n127.0.0.1:22\n";
        let got = parse_lsof_listen(text);
        assert_eq!(got.len(), 3);
        assert_eq!(got[0].port, 3000);
        assert_eq!(got[0].bind, "0.0.0.0");
        assert_eq!(got[0].pid, Some(41220));
        assert_eq!(got[0].uid, 501);
        assert_eq!(got[1].port, 3000);
        assert_eq!(got[1].bind, "::");
        assert_eq!(got[1].pid, Some(41220));
        assert_eq!(got[2].port, 22);
        assert_eq!(got[2].bind, "127.0.0.1");
        assert_eq!(got[2].uid, 0);
    }

    #[test]
    fn lsof_drops_an_established_socket_that_slipped_through() {
        let text = "p1\ncnode\nu501\ntIPv4\nn127.0.0.1:3000->127.0.0.1:51234\n";
        assert!(parse_lsof_listen(text).is_empty());
    }

    #[test]
    fn lsof_cwd_takes_the_path_field() {
        let text = "p41220\nfcwd\nn/Users/me/piaf-web\n";
        assert_eq!(parse_lsof_cwd(text), "/Users/me/piaf-web");
        assert_eq!(
            parse_lsof_cwd("p1\nfcwd\nn/tmp/gone (deleted)\n"),
            "/tmp/gone (deleted)"
        );
    }

    #[test]
    fn ps_etimes_is_seconds_not_the_padded_column() {
        assert_eq!(parse_ps_etimes("   38421\n"), Some(38421.0));
        assert_eq!(parse_ps_etimes(""), None);
    }

    #[test]
    fn ps_uid_is_the_number_not_the_padded_column() {
        assert_eq!(parse_ps_uid("   501\n"), Some(501));
        assert_eq!(parse_ps_uid(""), None);
    }

    #[test]
    fn ps_state_zombie_is_the_leading_letter() {
        assert!(parse_ps_state_zombie("Z\n"));
        assert!(parse_ps_state_zombie(" ZN\n"));
        assert!(!parse_ps_state_zombie("Ss\n"));
        assert!(!parse_ps_state_zombie(""));
    }

    #[test]
    fn proc_stat_zombie_is_the_token_after_comm() {
        assert!(parse_proc_stat_zombie("99 (sshd) Z 1 99 99 0 -1 0"));
        assert!(!parse_proc_stat_zombie("99 (sshd) S 1 99 99 0 -1 0"));
        // A comm with spaces still ends at the last close-paren.
        assert!(parse_proc_stat_zombie("12 (next server) Z 1 12 12"));
        assert!(!parse_proc_stat_zombie("no close paren"));
    }

    #[test]
    fn ifconfig_skips_link_local_and_keeps_the_rest() {
        let text = concat!(
            "lo0: flags=8049<UP,LOOPBACK,RUNNING,MULTICAST> mtu 16384\n",
            "	inet 127.0.0.1 netmask 0xff000000\n",
            "	inet6 ::1 prefixlen 128\n",
            "	inet6 fe80::1%lo0 prefixlen 64 scopeid 0x1\n",
            "en0: flags=8863<UP,BROADCAST,RUNNING,SIMPLEX,MULTICAST> mtu 1500\n",
            "	inet 192.0.2.10 netmask 0xffffff00 broadcast 192.0.2.255\n",
            "	inet6 2001:db8::10 prefixlen 64\n",
        );
        let got = parse_ifconfig(text);
        assert_eq!(
            got,
            vec![
                ("lo0".into(), "127.0.0.1".into(), false),
                ("lo0".into(), "::1".into(), true),
                ("en0".into(), "192.0.2.10".into(), false),
                ("en0".into(), "2001:db8::10".into(), true),
            ]
        );
    }

    #[test]
    fn ip_json_skips_link_local_the_same_way() {
        let text = r#"[{"ifname":"lo","addr_info":[{"local":"127.0.0.1","family":"inet"},{"local":"::1","family":"inet6"},{"local":"fe80::1","family":"inet6"}]},{"ifname":"eth0","addr_info":[{"local":"192.0.2.10","family":"inet"}]}]"#;
        let got = parse_ip_json(text);
        assert_eq!(
            got,
            vec![
                ("lo".into(), "127.0.0.1".into(), false),
                ("lo".into(), "::1".into(), true),
                ("eth0".into(), "192.0.2.10".into(), false),
            ]
        );
    }
}
