//! RTSP/HTTP request parsing and RTP transport-header decoding.
//!
//! Parsing is separate from dispatch so partial buffers and header limits can be
//! tested without sockets.

/// Largest accepted request buffer, matching the C++ `RBUF`.
pub const MAX_REQUEST_LEN: usize = 32_000;
/// Incomplete buffers above this size are discarded, as in the C++ reader.
pub const DISCARD_THRESHOLD: usize = MAX_REQUEST_LEN - 10;
/// Defensive header-count cap; `MAX_REQUEST_LEN` is the C++ limit.
pub const MAX_HEADERS: usize = 1024;

/// Protocol of a connection or response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Protocol {
    /// RTSP connection (`TYPE_RTSP`).
    Rtsp,
    /// HTTP connection (`TYPE_HTTP`).
    Http,
}

impl Protocol {
    /// Protocol name used on the status line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Rtsp => "RTSP",
            Self::Http => "HTTP",
        }
    }

    /// Whether the protocol is RTSP.
    pub fn is_rtsp(self) -> bool {
        self == Self::Rtsp
    }
}

/// One request header, with the colon removed from `name`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Header name as sent.
    pub name: String,
    /// Header value with surrounding whitespace removed.
    pub value: String,
}

impl Header {
    /// Whether this header's name matches case-insensitively.
    pub fn is(&self, name: &str) -> bool {
        self.name.eq_ignore_ascii_case(name)
    }
}

/// A parsed `Session` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionHeader {
    /// Session id; zero when the value did not start with a number.
    pub id: i64,
    /// `timeout=` suffix in seconds, when present.
    pub timeout: Option<u32>,
}

/// A complete RTSP or HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    /// Method token, e.g. `OPTIONS` or `GET`.
    pub method: String,
    /// Request target.
    pub uri: String,
    /// Protocol version token, e.g. `RTSP/1.0`.
    pub version: String,
    /// Headers in wire order.
    pub headers: Vec<Header>,
    /// Message body selected by `Content-Length`.
    pub body: Vec<u8>,
}

impl Request {
    /// First header with the given case-insensitive name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|header| header.is(name))
            .map(|header| header.value.as_str())
    }

    /// Whether the method matches case-insensitively.
    pub fn is_method(&self, method: &str) -> bool {
        // read_rtsp dispatches with starts_with_case_insensitive
        // (minisatip.cpp:1249-1320); the exact-case completeness gate is
        // covered by DEV-027.
        self.method.eq_ignore_ascii_case(method)
    }

    /// Parsed `CSeq` value; zero when absent or malformed.
    pub fn cseq(&self) -> i32 {
        parse_int_prefix(self.header("CSeq").unwrap_or(""), 0)
    }

    /// Parsed `Session` header, if present.
    pub fn session(&self) -> Option<SessionHeader> {
        let value = self.header("Session")?;
        let id = i64::from(parse_int_prefix(value, 0));
        let timeout = split_tokens(value, ';')
            .into_iter()
            .find_map(|token| token.strip_prefix("timeout="))
            .and_then(|value| u32::try_from(parse_int_prefix(value, -1)).ok());
        Some(SessionHeader { id, timeout })
    }
}

/// Result of trying to parse a buffered request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ParseOutcome {
    /// More bytes are needed; `consumed` leading interleaved bytes can be dropped.
    Incomplete { consumed: usize },
    /// Only complete interleaved RTP frames were present.
    Ignored { consumed: usize },
    /// A complete request was parsed; `consumed` bytes belong to it.
    Complete { request: Request, consumed: usize },
    /// The buffer exceeded the C++ size limit and should be discarded.
    Discarded,
    /// The request line or headers are syntactically invalid.
    Malformed,
}

/// Parses one request from `buf`, handling leading interleaved RTP frames.
pub fn parse_request(buf: &[u8]) -> ParseOutcome {
    if buf.is_empty() {
        return ParseOutcome::Incomplete { consumed: 0 };
    }

    let mut offset = 0;
    while buf.len().saturating_sub(offset) >= 4 && buf[offset] == b'$' && buf[offset + 1] < 2 {
        let frame_len = (usize::from(buf[offset + 2]) << 8) | usize::from(buf[offset + 3]);
        let total = 4 + frame_len;
        if buf.len() - offset < total {
            return ParseOutcome::Incomplete { consumed: offset };
        }
        offset += total;
    }
    if offset == buf.len() {
        return ParseOutcome::Ignored { consumed: offset };
    }

    let rest = &buf[offset..];
    let Some(terminator) = find_terminator(rest) else {
        if rest.len() > DISCARD_THRESHOLD {
            return ParseOutcome::Discarded;
        }
        return ParseOutcome::Incomplete { consumed: offset };
    };
    let header_end = terminator + 4;
    if offset + header_end > MAX_REQUEST_LEN {
        return ParseOutcome::Discarded;
    }

    let header_text = String::from_utf8_lossy(&rest[..terminator]);
    let mut lines = header_text.split("\r\n");
    let Some(request_line) = lines.next() else {
        return ParseOutcome::Malformed;
    };
    let mut parts = request_line.split_whitespace();
    let (Some(method), Some(uri)) = (parts.next(), parts.next()) else {
        return ParseOutcome::Malformed;
    };
    let version = parts.next().unwrap_or("");
    if method.is_empty() {
        return ParseOutcome::Malformed;
    }

    let mut headers = Vec::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        if headers.len() >= MAX_HEADERS {
            return ParseOutcome::Malformed;
        }
        let (name, value) = match line.split_once(':') {
            Some((name, value)) => (name.trim(), value.trim()),
            None => (line.trim(), ""),
        };
        headers.push(Header {
            name: name.to_owned(),
            value: value.to_owned(),
        });
    }

    let request = Request {
        method: method.to_owned(),
        uri: uri.to_owned(),
        version: version.to_owned(),
        headers,
        body: Vec::new(),
    };
    let content_length = request
        .header("Content-Length")
        .map(|value| parse_int_prefix(value, 0).max(0) as usize)
        .unwrap_or(0);
    // The C++ `is_rtsp_http_header` requires `nlnl + icl <= buf + len`, i.e. it
    // measures `Content-Length` from the start of the "\r\n\r\n" terminator.
    // A request is therefore accepted four bytes before the declared body is
    // complete. Match that threshold, which withdraws DEV-026.
    if rest.len() < terminator + content_length {
        return ParseOutcome::Incomplete { consumed: offset };
    }

    // Like the C++ reader, which never reads request bodies, keep only the body
    // bytes already buffered. A short body is not a reason to wait.
    let end = (header_end + content_length).min(rest.len());
    let mut request = request;
    request.body = rest[header_end..end].to_vec();
    ParseOutcome::Complete {
        request,
        consumed: offset + end,
    }
}

fn find_terminator(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|window| window == b"\r\n\r\n")
}

/// Parses a signed decimal prefix, returning `default` without any digits.
pub(crate) fn parse_int_prefix(text: &str, default: i32) -> i32 {
    let text = text.trim_start_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c']);
    let bytes = text.as_bytes();
    if bytes.is_empty() {
        return default;
    }
    let mut index = usize::from(bytes[0] == b'+' || bytes[0] == b'-');
    let digits = index;
    while index < bytes.len() && bytes[index].is_ascii_digit() {
        index += 1;
    }
    if index == digits {
        return default;
    }
    text[..index].parse().unwrap_or(default)
}

/// Splits like the C++ `split`: separators and control characters delimit tokens.
pub(crate) fn split_tokens(text: &str, separator: char) -> Vec<&str> {
    let mut result = Vec::new();
    let mut start = 0;
    let bytes = text.as_bytes();
    while start < bytes.len() {
        while start < bytes.len() && (bytes[start] == separator as u8 || bytes[start] < 33) {
            start += 1;
        }
        if start >= bytes.len() {
            break;
        }
        let mut end = start;
        while end < bytes.len() && bytes[end] != separator as u8 && bytes[end] >= 33 {
            end += 1;
        }
        result.push(&text[start..end]);
        start = end;
    }
    result
}

/// Transport protocol selected by a `Transport` header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum TransportKind {
    /// RTP over UDP (`RTP/AVP`).
    #[default]
    Udp,
    /// RTP over the RTSP TCP connection (`RTP/AVP/TCP`).
    Tcp,
    /// SRT (`RTP/AVP/SRT`).
    Srt,
}

/// Delivery mode selected by a `Transport` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportMode {
    /// Unicast to the client.
    Unicast,
    /// Multicast to the advertised destination.
    Multicast,
}

/// Decoded `Transport` header parameters.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransportSpec {
    /// Transport protocol.
    pub kind: TransportKind,
    /// Unicast or multicast; absent when the header omitted both.
    pub mode: Option<TransportMode>,
    /// Multicast destination or explicit unicast destination.
    pub destination: Option<String>,
    /// Destination port (`port=`), used for multicast.
    pub port: Option<u16>,
    /// Client RTP port (`client_port=`), used for unicast.
    pub client_port: Option<u16>,
    /// Multicast TTL.
    pub ttl: Option<i32>,
    /// RTP-over-TCP interleaved channel.
    pub interleaved: Option<i32>,
    /// SRT stream id.
    pub srt_streamid: Option<String>,
}

/// Decodes a `Transport` header value using the C++ token rules.
pub fn parse_transport(value: &str) -> TransportSpec {
    let mut spec = TransportSpec::default();
    if value.contains("RTP/AVP/SRT") {
        spec.kind = TransportKind::Srt;
    } else if value.contains("RTP/AVP/TCP") {
        spec.kind = TransportKind::Tcp;
    }
    if let Some(position) = value.find("srt_streamid=") {
        let rest = &value[position + "srt_streamid=".len()..];
        let end = rest.find(';').unwrap_or(rest.len());
        spec.srt_streamid = Some(rest[..end].to_owned());
    }

    for token in split_tokens(value, ';') {
        match token {
            "multicast" if spec.mode.is_none() => spec.mode = Some(TransportMode::Multicast),
            "unicast" if spec.mode.is_none() => spec.mode = Some(TransportMode::Unicast),
            _ => {}
        }
        if let Some(number) = token.strip_prefix("ttl=")
            && spec.ttl.is_none()
        {
            spec.ttl = Some(parse_int_prefix(number, 0));
        }
        if let Some(number) = token.strip_prefix("client_port=")
            && spec.client_port.is_none()
        {
            spec.client_port = parse_port(number);
        }
        if let Some(number) = token.strip_prefix("port=")
            && spec.port.is_none()
        {
            spec.port = parse_port(number);
        }
        if let Some(destination) = token.strip_prefix("destination=")
            && spec.destination.is_none()
        {
            spec.destination = Some(destination.to_owned());
        }
        if let Some(number) = token.strip_prefix("interleaved=")
            && spec.interleaved.is_none()
        {
            spec.interleaved = Some(parse_int_prefix(number, 0));
        }
    }
    spec
}

fn parse_port(value: &str) -> Option<u16> {
    u16::try_from(parse_int_prefix(value, -1)).ok()
}
