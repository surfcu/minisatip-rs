//! Byte-compatible RTSP/HTTP response encoding.

use crate::rtsp::Protocol;
use std::time::{SystemTime, UNIX_EPOCH};

/// The `Public` header emitted by OPTIONS and unimplemented methods.
pub const PUBLIC_HEADER: &str = "Public: OPTIONS, DESCRIBE, SETUP, PLAY, TEARDOWN";

/// An RTSP or HTTP response before serialization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Response {
    /// Wire protocol.
    pub protocol: Protocol,
    /// Status code; unknown codes serialize as 503.
    pub status: u16,
    /// Request sequence number echoed for RTSP when non-zero.
    pub cseq: i32,
    /// Attached stream's session id, used for the automatic `Session` header.
    pub session_id: Option<i64>,
    /// Raw extra header lines; `None` corresponds to the C++ null pointer.
    pub extra_headers: Option<Vec<String>>,
    /// Message body; a non-empty body adds `Content-Length`.
    pub body: Vec<u8>,
    /// Stream the body without `Content-Length` (`process_file` after its
    /// first 8200-byte flush, utils.cpp:399-418).
    pub streamed: bool,
    /// Preformatted `Date` header value.
    pub date: String,
    /// `Server` header value, e.g. `minisatip/1.0.0`.
    pub server: String,
}

impl Response {
    /// Creates an empty response for `protocol` and `status`.
    pub fn new(protocol: Protocol, status: u16, date: String, server: String) -> Self {
        Self {
            protocol,
            status,
            cseq: 0,
            session_id: None,
            extra_headers: None,
            body: Vec::new(),
            streamed: false,
            date,
            server,
        }
    }

    /// Marks the body as streamed without `Content-Length`.
    pub fn streamed(mut self) -> Self {
        self.streamed = true;
        self
    }

    /// Sets the echoed `CSeq`.
    pub fn with_cseq(mut self, cseq: i32) -> Self {
        self.cseq = cseq;
        self
    }

    /// Sets the attached stream's session id.
    pub fn with_session(mut self, session_id: Option<i64>) -> Self {
        self.session_id = session_id;
        self
    }

    /// Sets the raw extra header lines.
    pub fn with_headers(mut self, headers: Vec<String>) -> Self {
        self.extra_headers = Some(headers);
        self
    }

    /// Sets the body.
    pub fn with_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self
    }

    /// Streams the body like the C++ `process_file` (no length, trailing CRLF pair).
    pub fn with_streamed_body(mut self, body: impl Into<Vec<u8>>) -> Self {
        self.body = body.into();
        self.streamed = true;
        self
    }

    /// Serializes the response using the C++ `http_response` layout.
    pub fn to_bytes(&self) -> Vec<u8> {
        let (status, reason) = normalized_status(self.status);
        let mut out = String::new();
        out.push_str(&format!(
            "{}/1.0 {} {}\r\n",
            self.protocol.name(),
            status,
            reason
        ));
        if status != 501 {
            out.push_str(&format!("Date: {}\r\n", self.date));
        }

        let mut headers = self.extra_headers.clone();
        if status == 501 && headers.as_ref().is_none_or(Vec::is_empty) {
            headers = Some(vec![PUBLIC_HEADER.to_owned()]);
        }
        if self.protocol.is_rtsp()
            && self.session_id.is_some()
            && headers
                .as_ref()
                .is_some_and(|lines| !lines.iter().any(|line| line.contains("Session")))
            && status != 454
        {
            out.push_str(&format!(
                "Session: {:010}\r\n",
                self.session_id.unwrap_or(0)
            ));
        }
        if self.protocol.is_rtsp() && self.cseq > 0 {
            out.push_str(&format!("CSeq: {}\r\n", self.cseq));
        }
        if status != 501 {
            out.push_str(&format!("Server: {}\r\n", self.server));
        }
        if let Some(lines) = headers
            && status != 454
            && status != 404
        {
            for line in &lines {
                out.push_str(line);
                out.push_str("\r\n");
            }
        }
        if self.streamed {
            // `http_response(so, 200, ctype, "", 0, 0)` writes only the header
            // block, then the file chunks follow, and process_file ends with
            // strcpy(outp + io, "\r\n\r\n") (utils.cpp:398-419).
            out.push_str("\r\n");
            let mut bytes = out.into_bytes();
            bytes.extend_from_slice(&self.body);
            bytes.extend_from_slice(b"\r\n\r\n");
            bytes
        } else if self.body.is_empty() {
            out.push_str("\r\n");
            out.into_bytes()
        } else {
            out.push_str(&format!("Content-Length: {}\r\n\r\n", self.body.len()));
            let mut bytes = out.into_bytes();
            bytes.extend_from_slice(&self.body);
            bytes
        }
    }
}

fn normalized_status(status: u16) -> (u16, &'static str) {
    match status {
        200 => (200, "OK"),
        400 => (400, "Bad Request"),
        403 => (403, "Forbidden"),
        404 => (404, "Not Found"),
        405 => (405, "Method Not Allowed"),
        454 => (454, "Session Not Found"),
        500 => (500, "Internal Server Error"),
        501 => (501, "Not Implemented"),
        _ => (503, "Service Unavailable"),
    }
}

/// Formats `time` like the C++ `get_current_timestamp` (RFC 1123 with year).
pub fn format_http_date(time: SystemTime) -> String {
    let seconds = time
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    let days = seconds.div_euclid(86_400);
    let rest = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    const WEEKDAYS: [&str; 7] = ["Sun", "Mon", "Tue", "Wed", "Thu", "Fri", "Sat"];
    const MONTHS: [&str; 12] = [
        "Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec",
    ];
    let weekday = WEEKDAYS[(days + 4).rem_euclid(7) as usize];
    let month = MONTHS[(month - 1) as usize];
    format!(
        "{}, {} {} {:02}:{:02}:{:02} {} GMT",
        weekday,
        month,
        day,
        rest / 3600,
        (rest % 3600) / 60,
        rest % 60,
        year
    )
}

fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let shifted = days + 719_468;
    let era = if shifted >= 0 {
        shifted
    } else {
        shifted - 146_096
    } / 146_097;
    let day_of_era = shifted - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    };
    if month <= 2 {
        year += 1;
    }
    (year, month as u32, day as u32)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn http() -> Response {
        Response::new(Protocol::Http, 200, "date".to_owned(), "server".to_owned())
    }

    #[test]
    fn streamed_body_omits_length_and_ends_with_crlf_pair() {
        let text = String::from_utf8(
            http()
                .with_headers(vec!["Content-Type: text/html".to_owned()])
                .with_streamed_body("abc")
                .to_bytes(),
        )
        .expect("utf8");
        assert!(!text.contains("Content-Length"), "{text}");
        assert!(text.contains("\r\n\r\nabc\r\n\r\n"), "{text}");
    }

    #[test]
    fn plain_body_adds_content_length() {
        let text = String::from_utf8(http().with_body("abc").to_bytes()).expect("utf8");
        assert!(text.contains("Content-Length: 3\r\n\r\nabc"), "{text}");
    }
}
