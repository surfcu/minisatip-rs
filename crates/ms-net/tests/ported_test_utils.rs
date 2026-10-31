//! Port of the upstream `tests/test_utils.cpp` cases that live in ms-net.

use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use ms_net::fifo::Fifo;
use ms_net::rtsp::{ParseOutcome, Request, parse_request};
use ms_net::web::{WebConfig, WebService, WebSnapshot};

// Ported from tests/test_utils.cpp:44 (test_fifo, f and f2); `peek` of the whole
// FIFO stands for fifo_pop, which is not ported.
#[test]
fn test_fifo() {
    let buf = [0u8, 1, 2, 3, 4, 5];
    let mut f = Fifo::new(10);
    let (a, b) = f.peek(buf.len(), 0);
    assert_eq!(
        a.len() + b.len(),
        0,
        "Expected 0 bytes to be popped from the fifo"
    );
    assert_eq!(f.push(&buf), buf.len(), "Failed to write record");
    assert_eq!(
        f.push_force(&buf, true),
        buf.len(),
        "Failed to write record with force"
    );

    let mut f2 = Fifo::new(10);
    let buf2 = [0u8, 1, 2, 3, 4, 5, 6, 7];
    let buf3 = [8u8, 9, 10, 11];
    assert_eq!(f2.push(&buf2), buf2.len(), "Failed to fill fifo");
    assert_eq!(
        f2.push(&buf3),
        0,
        "Expected push beyond available space to fail"
    );
    assert_eq!(
        f2.push_force(&buf3, true),
        buf3.len(),
        "Failed to force write beyond available space"
    );
}

// Ported from tests/test_utils.cpp:108 (test_is_rtsp_response, header4): a
// request whose Content-Length body is buffered is complete.
#[test]
fn test_is_rtsp_request() {
    let header4 = b"OPTIONS rtsp://1.2.3.4 RTSP/1.0\r\nContent-Length: 2\r\n\r\nbu\0";
    assert!(
        matches!(parse_request(header4), ParseOutcome::Complete { .. }),
        "Expected1 for header4"
    );
}

fn get(uri: &str) -> Request {
    Request {
        method: "GET".to_owned(),
        uri: uri.to_owned(),
        version: "HTTP/1.0".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

// Ported from tests/test_utils.cpp:187 (test_readfile_content_type): fixture
// files under document_root, served by the web route that calls readfile.
#[test]
fn test_readfile_content_type() {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_nanos());
    let dir = std::env::temp_dir().join(format!("test-ctype-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&dir).expect("could not create fixture dir");
    for name in ["a.ico", "b.png", "c.jpg", "d.html", "e.bin"] {
        fs::write(dir.join(name), b"x").expect("could not create fixture file");
    }
    let service = WebService::new(WebConfig {
        document_root: dir.clone(),
        ..WebConfig::default()
    });
    let snapshot = WebSnapshot::new();
    let cases = [
        ("/a.ico", "image/x-icon"),
        ("/b.png", "image/png"),
        ("/c.jpg", "image/jpeg"),
        ("/d.html", "text/html"),
        ("/e.bin", "application/octet-stream"),
    ];
    for (name, mime) in cases {
        let route = service.route(&get(name), &snapshot).expect("route");
        assert_eq!(
            route.status, 200,
            "readfile should open fixture file {name}"
        );
        assert!(
            route.headers.iter().any(|header| header.contains(mime)),
            "readfile reported wrong Content-Type for {name}: {:?}",
            route.headers
        );
    }
    fs::remove_dir_all(dir).ok();
}
