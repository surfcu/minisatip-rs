//! getopt_long-compatible error type and argument scanning helpers.

use std::fmt;

/// Fatal error returned by [`crate::parse_args`].
///
/// Only conditions where the C++ `set_options` exits the process are fatal.
/// Unknown options, ambiguous prefixes and missing arguments are diagnostics
/// that glibc getopt prints before parsing continues, so they are reported as
/// warnings instead of errors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OptError {
    /// The option argument itself is invalid.
    InvalidValue {
        /// Option name as written on the command line.
        option: String,
        /// Human readable reason.
        message: String,
        /// Whether the C++ binary prints `usage()` before `exit(1)`.
        show_usage: bool,
    },
}

impl fmt::Display for OptError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            OptError::InvalidValue { message, .. } => f.write_str(message),
        }
    }
}

impl std::error::Error for OptError {}

/// Iterates over arguments, treating control bytes that the C++ `split` skips.
pub(crate) fn split_terms(s: &str, sep: char) -> Vec<&str> {
    let bytes = s.as_bytes();
    let sep = sep as u8;
    let mut out = Vec::new();
    let mut start = 0;
    while start < bytes.len() {
        while start < bytes.len() && (bytes[start] == sep || bytes[start] < 33) {
            start += 1;
        }
        if start >= bytes.len() {
            break;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end] != sep && bytes[end] >= 33 {
            end += 1;
        }
        out.push(&s[start..end]);
        start = end;
    }
    out
}

/// C `atoi`: optional whitespace/sign, decimal prefix, 0 when no digits.
pub(crate) fn c_atoi(s: &str) -> i32 {
    let bytes = s.as_bytes();
    let mut i = 0;
    while i < bytes.len() && bytes[i].is_ascii_whitespace() {
        i += 1;
    }
    let mut negative = false;
    if i < bytes.len() && (bytes[i] == b'+' || bytes[i] == b'-') {
        negative = bytes[i] == b'-';
        i += 1;
    }
    let start = i;
    let mut value: i64 = 0;
    while i < bytes.len() && bytes[i].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((bytes[i] - b'0') as i64);
        i += 1;
    }
    if i == start {
        return 0;
    }
    let value = if negative { -value } else { value };
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// C `sscanf("%d:%d")`: returns the values that parsed, leaving others unset.
pub(crate) fn scan_int_pair(s: &str) -> (Option<i32>, Option<i32>) {
    let bytes = s.as_bytes();
    let Some((first, mut pos)) = scan_digits(bytes, 0) else {
        return (None, None);
    };
    if bytes.get(pos) != Some(&b':') {
        return (Some(first), None);
    }
    pos += 1;
    match scan_digits(bytes, pos) {
        Some((second, _)) => (Some(first), Some(second)),
        None => (Some(first), None),
    }
}

/// C `sscanf("%d:%d:%d")`.
pub(crate) fn scan_int_triple(s: &str) -> (Option<i32>, Option<i32>, Option<i32>) {
    let bytes = s.as_bytes();
    let Some((first, mut pos)) = scan_digits(bytes, 0) else {
        return (None, None, None);
    };
    if bytes.get(pos) != Some(&b':') {
        return (Some(first), None, None);
    }
    pos += 1;
    let Some((second, next)) = scan_digits(bytes, pos) else {
        return (Some(first), None, None);
    };
    pos = next;
    if bytes.get(pos) != Some(&b':') {
        return (Some(first), Some(second), None);
    }
    let third = scan_digits(bytes, pos + 1).map(|(value, _)| value);
    (Some(first), Some(second), third)
}

fn scan_digits(bytes: &[u8], mut pos: usize) -> Option<(i32, usize)> {
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    let mut negative = false;
    if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
        negative = bytes[pos] == b'-';
        pos += 1;
    }
    let start = pos;
    let mut value: i64 = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        value = value
            .saturating_mul(10)
            .saturating_add((bytes[pos] - b'0') as i64);
        pos += 1;
    }
    if pos == start {
        return None;
    }
    let value = if negative { -value } else { value };
    Some((
        value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32,
        pos,
    ))
}

/// C++ `parse_int`: strips leading whitespace, requires a sign or digit,
/// parses the numeric prefix and returns `dv` for empty/invalid/out-of-range input.
pub(crate) fn parse_int(s: &str, dv: i32) -> i32 {
    let s = s.trim_start_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c']);
    let bytes = s.as_bytes();
    if bytes.is_empty() {
        return dv;
    }
    if bytes[0] != b'+' && bytes[0] != b'-' && !bytes[0].is_ascii_digit() {
        return dv;
    }
    let digits = if bytes[0] == b'+' { &s[1..] } else { s };
    let bytes = digits.as_bytes();
    let (negative, start) = if bytes.first() == Some(&b'-') {
        (true, 1)
    } else {
        (false, 0)
    };
    let mut pos = start;
    let mut value: i64 = 0;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        let Some(next) = value
            .checked_mul(10)
            .and_then(|v| v.checked_add((bytes[pos] - b'0') as i64))
        else {
            return dv;
        };
        value = next;
        pos += 1;
    }
    if pos == start {
        return dv;
    }
    let value = if negative { -value } else { value };
    if value < i64::from(i32::MIN) || value > i64::from(i32::MAX) {
        return dv;
    }
    value as i32
}

/// C `strtod` prefix parsing, as used by `--multiplier`.
pub(crate) fn parse_f32_prefix(s: &str) -> f32 {
    let bytes = s.as_bytes();
    let mut start = 0;
    while start < bytes.len() && bytes[start].is_ascii_whitespace() {
        start += 1;
    }
    let mut pos = start;
    if pos < bytes.len() && (bytes[pos] == b'+' || bytes[pos] == b'-') {
        pos += 1;
    }
    let int_start = pos;
    while pos < bytes.len() && bytes[pos].is_ascii_digit() {
        pos += 1;
    }
    let mut has_digits = pos > int_start;
    if pos < bytes.len() && bytes[pos] == b'.' {
        pos += 1;
        let frac_start = pos;
        while pos < bytes.len() && bytes[pos].is_ascii_digit() {
            pos += 1;
        }
        has_digits |= pos > frac_start;
    }
    if !has_digits {
        let negative = s.as_bytes().get(start) == Some(&b'-');
        let rest = &s[pos..];
        let value = if rest
            .get(..8)
            .is_some_and(|p| p.eq_ignore_ascii_case("infinity"))
            || rest.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("inf"))
        {
            f32::INFINITY
        } else if rest.get(..3).is_some_and(|p| p.eq_ignore_ascii_case("nan")) {
            f32::NAN
        } else {
            return 0.0;
        };
        return if negative { -value } else { value };
    }
    if pos < bytes.len() && (bytes[pos] == b'e' || bytes[pos] == b'E') {
        let exp_start = pos;
        let mut exp_pos = pos + 1;
        if exp_pos < bytes.len() && (bytes[exp_pos] == b'+' || bytes[exp_pos] == b'-') {
            exp_pos += 1;
        }
        let digits_start = exp_pos;
        while exp_pos < bytes.len() && bytes[exp_pos].is_ascii_digit() {
            exp_pos += 1;
        }
        if exp_pos > digits_start {
            pos = exp_pos;
        } else {
            pos = exp_start;
        }
    }
    s[start..pos].parse::<f64>().map_or(0.0, |v| v as f32)
}
