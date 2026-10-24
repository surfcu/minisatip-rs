//! Exact text of the session/resource log lines ported from `src/ca.cpp`.
//!
//! Lives in its own binary: [`ms_log::init`] may only run once per process.

use std::io;
use std::sync::{Arc, Mutex};

use ms_ca_drivers::en50221::consts::EN50221_APP_RM_RESOURCEID;
use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_READY};
use ms_ca_drivers::en50221::{CaDevice, CaLink, ManualClock, SlotInfo};
use ms_log::{Backend, Timestamp};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

/// A ready link that accepts every write.
struct FakeLink;

impl CaLink for FakeLink {
    fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::ErrorKind::WouldBlock.into())
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn slot_info(&mut self) -> io::Result<SlotInfo> {
        Ok(SlotInfo {
            kind: CA_CI_LINK,
            flags: CA_CI_MODULE_READY,
        })
    }

    fn sleep_ms(&mut self, _ms: u32) {}
}

/// `ST_OPEN_SESSION_REQUEST` for `resource`, session number 1.
fn open_session(resource: u32) -> Vec<u8> {
    let mut spdu = vec![0x91, 0x06];
    spdu.extend_from_slice(&resource.to_be_bytes());
    spdu.extend_from_slice(&[0, 1]);
    spdu
}

#[test]
fn session_lines_match_ca_cpp() {
    ms_log::set_thread_name("ca-dev");
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let clock = Arc::new(ManualClock::new(0, 0));
    let mut ca = CaDevice::new(2, clock);
    ca.set_link(Some(Box::new(FakeLink)));

    // The resource manager: open, "RM Created", profile enquiry written.
    ca.handle_spdu(&open_session(EN50221_APP_RM_RESOURCEID));
    // An unknown resource: the status byte is SS_NOT_FOUND (0xF0).
    ca.handle_spdu(&open_session(0x0012_3456));
    ca.request_close();

    let prefix = "[05/03 14:07:09.042 ca-dev]: ";
    let expected = [
        format!(
            "{prefix}Found session_number 1 for resource {:06X}, name EN50221_APP_RM_RESOURCEID, status 00",
            EN50221_APP_RM_RESOURCEID
        ),
        format!("{prefix}RM Created"),
        format!(
            "{prefix}ca_write_apdu: CA 2, session 1, name EN50221_APP_RM_RESOURCEID, write tag 9F8010, data length 0"
        ),
        format!("{prefix}CA 2: resource 123456 is not supported"),
        format!("{prefix}Found session_number -1 for resource 123456, name none, status F0"),
        format!("{prefix}Requesting CA 2 close"),
    ];
    // A diag-log build adds events on the same output; the C++ log lines are
    // still matched exactly.
    let captured = lines.lock().unwrap();
    let log_lines: Vec<&String> = captured
        .iter()
        .filter(|line| !line.contains("\"kind\":\"event\""))
        .collect();
    assert_eq!(log_lines.len(), expected.len(), "{captured:?}");
    for (line, want) in log_lines.iter().zip(&expected) {
        assert_eq!(*line, want);
    }
}
