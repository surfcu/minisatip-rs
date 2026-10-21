//! One representative log line ported from `minisatip.cpp`: the SSDP reply
//! sent by `ssdp_reply` (minisatip.cpp:1855-1858).
//!
//! This lives in its own test binary because [`ms_log::init`] may only run
//! once per process.

use std::io;
use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};
use ms_net::ssdp::{SsdpConfig, SsdpIo, SsdpServer};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        // Fixed so the assertion does not depend on the clock.
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

/// Collects every datagram the server sends; `fd` stays at the trait default.
struct FakeIo(Arc<Mutex<Vec<Vec<u8>>>>);

impl SsdpIo for FakeIo {
    fn send_to(&mut self, data: &[u8], _host: &str, _port: u16) -> io::Result<usize> {
        self.0.lock().unwrap().push(data.to_vec());
        Ok(data.len())
    }
}

#[test]
fn ssdp_reply_log_matches_the_cpp_text() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::set_thread_name("main");
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_SSDP,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let sent = Arc::new(Mutex::new(Vec::new()));
    let config = SsdpConfig {
        uuid: "test-uuid".to_owned(),
        ..SsdpConfig::default()
    };
    let mut server = SsdpServer::new(FakeIo(Arc::clone(&sent)), config);
    server
        .handle(
            b"M-SEARCH * HTTP/1.1\r\nHOST: 239.255.255.250:1900\r\n\
              MAN: \"ssdp:discover\"\r\nST: urn:ses-com:device:SatIPServer:1\r\n\r\n",
            "192.0.2.1",
            1234,
            "Tue, 03 Mar 2026 14:07:09 GMT",
            0,
        )
        .expect("handle M-SEARCH");

    let reply = sent.lock().unwrap().last().cloned().expect("a reply");
    assert!(
        reply.starts_with(b"HTTP/1.1 200 OK"),
        "last datagram: {}",
        String::from_utf8_lossy(&reply)
    );
    // minisatip.cpp:1855-1858: "Send Reply SSDP packet (fd: %d) %s:%d,
    // bootid: %d deviceid: %d http: %s\n%s".
    let expected = format!(
        "[05/03 14:07:09.042 main]: Send Reply SSDP packet (fd: 0) 192.0.2.1:1234, \
         bootid: 0 deviceid: 0 http: 127.0.0.1:8080\n{}",
        String::from_utf8_lossy(&reply)
    );
    let lines = lines.lock().unwrap();
    assert!(
        lines.contains(&expected),
        "missing expected log line:\n{expected}\ngot:\n{}",
        lines.join("\n---\n")
    );
}
