/// The sequence number both iputils and BSD ping put on a reply.
pub(crate) fn parse_sequence(line: &str) -> Option<u64> {
    let at = line.find("icmp_seq=")? + "icmp_seq=".len();
    let rest = &line[at..];
    let end = rest
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(rest.len());
    rest[..end].parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sequences_from_captured_bsd_replies() {
        let transcript = [
            "64 bytes from 127.0.0.1: icmp_seq=0 ttl=64 time=0.057 ms",
            "64 bytes from 127.0.0.1: icmp_seq=2 ttl=64 time=0.081 ms",
        ];

        assert_eq!(parse_sequence(transcript[0]), Some(0));
        assert_eq!(parse_sequence(transcript[1]), Some(2));
    }

    #[test]
    fn rejects_lines_without_a_sequence() {
        assert_eq!(parse_sequence("PING example.test (192.0.2.1)"), None);
    }
}
