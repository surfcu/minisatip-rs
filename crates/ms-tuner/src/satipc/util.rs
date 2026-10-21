//! C-style string helpers used by the satipc port (`utils.cpp`).

use crate::tuner::DeliverySystem;

/// `parse_int`: skips leading whitespace, accepts a sign and digits, else `dv`.
pub fn parse_int(text: &str, dv: i32) -> i32 {
    let s = text.trim_start_matches([' ', '\t', '\r', '\n', '\u{b}', '\u{c}']);
    let bytes = s.as_bytes();
    let (negative, start) = match bytes.first() {
        Some(b'+') => (false, 1),
        Some(b'-') => (true, 1),
        Some(c) if c.is_ascii_digit() => (false, 0),
        _ => return dv,
    };
    let digits = bytes[start..]
        .iter()
        .take_while(|c| c.is_ascii_digit())
        .count();
    if digits == 0 {
        return dv;
    }
    let mut value: i64 = 0;
    for c in &bytes[start..start + digits] {
        value = value * 10 + i64::from(c - b'0');
        if value > i64::from(i32::MAX) + 1 {
            return dv;
        }
    }
    let value = if negative { -value } else { value };
    i32::try_from(value).unwrap_or(dv)
}

/// `split`: separators and control characters/spaces both end a token.
pub fn split(text: &str, sep: char) -> Vec<&str> {
    let is_sep = |c: char| c == sep || (c as u32) < 33;
    text.split(is_sep)
        .filter(|token| !token.is_empty())
        .collect()
}

/// `header_parameter`: the value following a `Name:` token.
pub fn header_parameter<'a>(args: &[&'a str], i: usize) -> &'a str {
    let Some(current) = args.get(i) else {
        return "";
    };
    if current.ends_with(':') {
        return args.get(i + 1).copied().unwrap_or("");
    }
    if let Some(colon) = current.find(':') {
        return &current[colon + 1..];
    }
    if args.get(i + 1) == Some(&":") {
        return args.get(i + 2).copied().unwrap_or("");
    }
    ""
}

/// Case-insensitive ASCII prefix test (`starts_with_case_insensitive`).
pub fn starts_with_ci(text: &str, prefix: &str) -> bool {
    text.len() >= prefix.len()
        && text.as_bytes()[..prefix.len()].eq_ignore_ascii_case(prefix.as_bytes())
}

/// Case-insensitive ASCII substring search (`strcasestr`).
pub fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    (0..=haystack.len() - needle.len())
        .find(|&i| haystack[i..i + needle.len()].eq_ignore_ascii_case(needle))
}

/// Byte substring search (`strstr` on a bounded buffer).
pub fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// The text of `buf` up to its first NUL, as C string functions see it.
pub fn c_text(buf: &[u8]) -> String {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    String::from_utf8_lossy(&buf[..end]).into_owned()
}

/// `is_rtsp_response`: an `RTSP/1.` header whose `Content-Length` body fits in `len` bytes.
pub fn is_rtsp_response(buf: &[u8]) -> bool {
    let end = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
    let text = &buf[..end];
    if !text.starts_with(b"RTSP/1.") {
        return false;
    }
    let Some(nlnl) = find_bytes(text, b"\r\n\r\n") else {
        return false;
    };
    let Some(cl) = find_ci(text, b"Content-Length:") else {
        return true;
    };
    let icl = parse_int(&String::from_utf8_lossy(&text[cl + 15..]), 0);
    // C++ compares `nlnl + icl` with `buf + len`.
    i64::from(icl) + nlnl as i64 <= buf.len() as i64
}

/// `fe_delsys_map` entries: name and `fe_delivery_system_t` value.
const DELSYS: [(&str, i32); 20] = [
    ("undefined", 0),
    ("dvbc", 1),
    ("dvbcb", 2),
    ("dvbt", 3),
    ("dss", 4),
    ("dvbs", 5),
    ("dvbs2", 6),
    ("dvbh", 7),
    ("isdbt", 8),
    ("isdbs", 9),
    ("isdbc", 10),
    ("atsc", 11),
    ("atscmh", 12),
    ("dmbth", 13),
    ("cmmb", 14),
    ("dab", 15),
    ("dvbt2", 16),
    ("turbo", 17),
    ("dvbcc", 18),
    ("dvbc2", 19),
];

/// The `fe_delivery_system_t` value of a delivery system.
pub fn delsys_code(sys: DeliverySystem) -> i32 {
    match sys {
        DeliverySystem::Undefined => 0,
        DeliverySystem::DvbC => 1,
        DeliverySystem::DvbT => 3,
        DeliverySystem::DvbS => 5,
        DeliverySystem::DvbS2 => 6,
        DeliverySystem::DvbT2 => 16,
        DeliverySystem::DvbC2 => 19,
        DeliverySystem::Other(code) => code,
    }
}

/// The delivery system for a `fe_delivery_system_t` value.
pub fn delsys_from_code(code: i32) -> DeliverySystem {
    match code {
        0 => DeliverySystem::Undefined,
        1 => DeliverySystem::DvbC,
        3 => DeliverySystem::DvbT,
        5 => DeliverySystem::DvbS,
        6 => DeliverySystem::DvbS2,
        16 => DeliverySystem::DvbT2,
        19 => DeliverySystem::DvbC2,
        other => DeliverySystem::Other(other),
    }
}

/// `fe_delsys_map.lookup`: case-insensitive name to delivery system.
pub fn delsys_lookup(name: &str) -> Option<DeliverySystem> {
    DELSYS
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, code)| delsys_from_code(*code))
}

/// `fe_delsys_map.reverse_lookup`: the name of a delivery system, or "".
pub fn delsys_name(sys: DeliverySystem) -> &'static str {
    let code = delsys_code(sys);
    DELSYS
        .iter()
        .find(|(_, value)| *value == code)
        .map_or("", |(key, _)| key)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_int_matches_c() {
        assert_eq!(parse_int(" 42abc", 0), 42);
        assert_eq!(parse_int("+7", 0), 7);
        assert_eq!(parse_int("-3,", 0), -3);
        assert_eq!(parse_int("x", 9), 9);
        assert_eq!(parse_int("", 30), 30);
        assert_eq!(parse_int("99999999999", 5), 5);
    }

    #[test]
    fn split_skips_control_characters() {
        assert_eq!(
            split("Session: AB;timeout=60\r\nX: 1", ' '),
            vec!["Session:", "AB;timeout=60", "X:", "1"]
        );
    }

    #[test]
    fn rtsp_response_completeness() {
        assert!(!is_rtsp_response(b"RTSP/1.0 200 OK\r\n"));
        assert!(is_rtsp_response(b"RTSP/1.0 200 OK\r\n\r\n"));
        assert!(!is_rtsp_response(
            b"RTSP/1.0 200 OK\r\nContent-Length: 10\r\n\r\nab"
        ));
        assert!(is_rtsp_response(
            b"RTSP/1.0 200 OK\r\nContent-Length: 2\r\n\r\nab"
        ));
        assert!(!is_rtsp_response(b"PLAY x RTSP/1.0\r\n\r\n"));
    }
}
