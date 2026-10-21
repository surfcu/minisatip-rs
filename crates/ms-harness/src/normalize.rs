//! Normalisation of RTSP/HTTP text: session ids, ports, dates, SSRC, versions.

use std::collections::HashMap;

/// Per-run normalisation state; placeholders are numbered in first-seen order.
#[derive(Default)]
pub struct Normalizer {
    ports: HashMap<String, String>,
    sessions: Vec<(String, String)>,
    texts: Vec<(String, String)>,
}

impl Normalizer {
    /// Replaces every occurrence of `port` as a whole number by `<name>`.
    /// Fails when the port or the name is already mapped differently (ambiguous).
    pub fn add_port(&mut self, port: u16, name: &str) -> Result<(), String> {
        let (port, name) = (port.to_string(), format!("<{name}>"));
        if let Some(known) = self.ports.get(&port).filter(|known| **known != name) {
            return Err(format!("port {port} is both {known} and {name}"));
        }
        if let Some((other, _)) = self.ports.iter().find(|(p, n)| **n == name && **p != port) {
            return Err(format!("{name} is both port {other} and port {port}"));
        }
        self.ports.insert(port, name);
        Ok(())
    }

    /// Ports that already have a placeholder.
    pub fn ports(&self) -> Vec<u16> {
        self.ports
            .keys()
            .filter_map(|port| port.parse().ok())
            .collect()
    }

    /// Replaces every occurrence of `from` by `to` (per-binary paths).
    pub fn add_text(&mut self, from: &str, to: &str) {
        if !from.is_empty() {
            self.texts.push((from.to_owned(), to.to_owned()));
        }
    }

    /// Registers a session id seen in a `Session:` header.
    pub fn add_session(&mut self, id: &str) {
        if id.is_empty() || self.sessions.iter().any(|(known, _)| known == id) {
            return;
        }
        let name = format!("<SESSION{}>", self.sessions.len() + 1);
        self.sessions.push((id.to_owned(), name));
    }

    /// Normalises a whole message (head and text body).
    pub fn text(&mut self, text: &str) -> String {
        for line in text.lines() {
            if let Some(value) = header(line, "session") {
                self.add_session(value.split(';').next().unwrap_or("").trim());
            }
        }
        let lines: Vec<String> = text.lines().map(|line| self.line(line)).collect();
        lines.join("\n")
    }

    fn line(&self, line: &str) -> String {
        for name in ["date", "last-modified", "expires"] {
            if header(line, name).is_some() {
                let key = line.split(':').next().unwrap_or(name);
                return format!("{key}: <DATE>");
            }
        }
        let mut out = line.to_owned();
        for (from, to) in &self.texts {
            out = out.replace(from.as_str(), to);
        }
        for (id, name) in &self.sessions {
            out = out.replace(id.as_str(), name);
        }
        out = replace_after(&out, "minisatip/", "<VER>", |c| {
            !c.is_whitespace() && c != '<'
        });
        out = replace_after(&out, "<modelNumber>", "<VER>", |c| c != '<');
        out = replace_after(&out, "\"version\":\"", "<VER>", |c| c != '"');
        out = replace_after(&out, "ssrc=", "<SSRC>", |c| c.is_ascii_hexdigit());
        out = replace_after(&out, "seq=", "<SEQ>", |c| c.is_ascii_digit());
        out = replace_after(&out, "rtptime=", "<RTPTIME>", |c| c.is_ascii_digit());
        if let Some(rest) = out.strip_prefix("o=- ") {
            let mut fields = rest.splitn(3, ' ');
            let (_, _, tail) = (fields.next(), fields.next(), fields.next().unwrap_or(""));
            out = format!("o=- <SDPID> <SDPVER> {tail}");
        }
        out = uuids(&out);
        self.numbers(&out)
    }

    fn numbers(&self, line: &str) -> String {
        let mut out = String::with_capacity(line.len());
        let mut digits = String::new();
        let mut previous: Option<char> = None;
        let flush =
            |digits: &mut String, out: &mut String, next: Option<char>, prev: Option<char>| {
                if digits.is_empty() {
                    return;
                }
                let bounded = !prev.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.')
                    && !next.is_some_and(|c| c.is_ascii_alphanumeric() || c == '.');
                match self.ports.get(digits.as_str()) {
                    Some(name) if bounded => out.push_str(name),
                    _ => out.push_str(digits),
                }
                digits.clear();
            };
        let mut before_digits: Option<char> = None;
        for c in line.chars() {
            if c.is_ascii_digit() {
                if digits.is_empty() {
                    before_digits = previous;
                }
                digits.push(c);
            } else {
                flush(&mut digits, &mut out, Some(c), before_digits);
                out.push(c);
            }
            previous = Some(c);
        }
        flush(&mut digits, &mut out, None, before_digits);
        out
    }
}

/// Value of header `name` (case-insensitive) when `line` is that header.
pub fn header<'a>(line: &'a str, name: &str) -> Option<&'a str> {
    let (key, value) = line.split_once(':')?;
    key.trim().eq_ignore_ascii_case(name).then(|| value.trim())
}

fn replace_after(
    line: &str,
    marker: &str,
    placeholder: &str,
    keep: impl Fn(char) -> bool,
) -> String {
    let mut out = String::new();
    let mut rest = line;
    while let Some(position) = rest.find(marker) {
        let (head, tail) = rest.split_at(position + marker.len());
        out.push_str(head);
        let end = tail.find(|c: char| !keep(c)).unwrap_or(tail.len());
        if end > 0 {
            out.push_str(placeholder);
        }
        rest = &tail[end..];
    }
    out.push_str(rest);
    out
}

fn uuids(line: &str) -> String {
    if !line.is_ascii() {
        return line.to_owned();
    }
    let bytes = line.as_bytes();
    let groups = [8usize, 4, 4, 4, 12];
    let mut out = String::new();
    let mut index = 0;
    'scan: while index < bytes.len() {
        let mut cursor = index;
        for (number, length) in groups.iter().enumerate() {
            let end = cursor + length;
            if end > bytes.len() || !bytes[cursor..end].iter().all(u8::is_ascii_hexdigit) {
                out.push(char::from(bytes[index]));
                index += 1;
                continue 'scan;
            }
            cursor = end;
            if number < groups.len() - 1 {
                if bytes.get(cursor) != Some(&b'-') {
                    out.push(char::from(bytes[index]));
                    index += 1;
                    continue 'scan;
                }
                cursor += 1;
            }
        }
        out.push_str("<UUID>");
        index = cursor;
    }
    out
}

/// Replaces every run of digits by `<N>` (for live counters).
pub fn all_numbers(text: &str) -> String {
    let mut out = String::new();
    let mut in_number = false;
    for c in text.chars() {
        if c.is_ascii_digit() {
            if !in_number {
                out.push_str("<N>");
                in_number = true;
            }
        } else {
            in_number = false;
            out.push(c);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises_rtsp_reply() {
        let mut norm = Normalizer::default();
        norm.add_port(40001, "RTSP").unwrap();
        norm.add_port(41000, "u1.rtp").unwrap();
        let text = "RTSP/1.0 200 OK\r\nDate: Wed, 21 Oct 2026 10:00:00 GMT\r\nCSeq: 3\r\n\
                    Session: 0123456789;timeout=30\r\n\
                    Transport: RTP/AVP;unicast;client_port=41000-41001;ssrc=1a2b3c\r\n\
                    RTP-Info: url=rtsp://127.0.0.1:40001/stream=1;seq=4711;rtptime=99\r\n\
                    Server: minisatip/2.0~abc\r\n";
        let out = norm.text(text);
        assert!(out.contains("Date: <DATE>"), "{out}");
        assert!(out.contains("Session: <SESSION1>;timeout=30"), "{out}");
        assert!(
            out.contains("client_port=<u1.rtp>-41001;ssrc=<SSRC>"),
            "{out}"
        );
        assert!(
            out.contains("127.0.0.1:<RTSP>/stream=1;seq=<SEQ>;rtptime=<RTPTIME>"),
            "{out}"
        );
        assert!(out.contains("minisatip/<VER>"), "{out}");
        assert!(out.contains("CSeq: 3"), "{out}");
    }

    #[test]
    fn normalises_uuid_and_sdp_origin() {
        let mut norm = Normalizer::default();
        let out = norm.text("o=- 534863118 534863118 IN IP4 127.0.0.1\nuuid:2fac1234-31f8-11b4-a222-08002b34c003::x");
        assert_eq!(out, "o=- <SDPID> <SDPVER> IN IP4 127.0.0.1\nuuid:<UUID>::x");
    }

    #[test]
    fn normalises_every_version_field() {
        let mut norm = Normalizer::default();
        let out = norm.text(
            "Server: minisatip/2.0.167~e34c2d1\n<modelNumber>2.0.167</modelNumber>\n\
             {\"version\":\"2.0.167\",\"name\":\"x\"}",
        );
        assert_eq!(
            out,
            "Server: minisatip/<VER>\n<modelNumber><VER></modelNumber>\n\
             {\"version\":\"<VER>\",\"name\":\"x\"}"
        );
    }

    #[test]
    fn numbers_inside_words_are_kept() {
        let mut norm = Normalizer::default();
        norm.add_port(554, "P").unwrap();
        assert_eq!(
            norm.text("a554 554 5540 1.554 :554"),
            "a554 <P> 5540 1.554 :<P>"
        );
        assert_eq!(all_numbers("a=12,b=3"), "a=<N>,b=<N>");
    }

    #[test]
    fn a_port_cannot_have_two_names() {
        let mut norm = Normalizer::default();
        norm.add_port(40001, "RTSP").unwrap();
        norm.add_port(40001, "RTSP").unwrap();
        let error = norm.add_port(40001, "SRTP+31").unwrap_err();
        assert!(
            error.contains("<RTSP>") && error.contains("<SRTP+31>"),
            "{error}"
        );
        let error = norm.add_port(40002, "RTSP").unwrap_err();
        assert!(
            error.contains("40001") && error.contains("40002"),
            "{error}"
        );
        assert_eq!(norm.text("127.0.0.1:40001"), "127.0.0.1:<RTSP>");
        assert_eq!(norm.ports(), vec![40001]);
    }
}
