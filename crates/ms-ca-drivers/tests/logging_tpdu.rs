//! TPDU read log lines (ca.cpp:3470-3478): a status byte without data is quiet.
//!
//! Lives in its own binary: [`ms_log::init`] may only run once per process.

use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

use ms_ca_drivers::en50221::consts::T_SB;
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

/// A ready slot that returns queued reads and accepts every write.
struct ScriptedLink(VecDeque<Vec<u8>>);

impl CaLink for ScriptedLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(msg) = self.0.pop_front() else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        buf[..msg.len()].copy_from_slice(&msg);
        Ok(msg.len())
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

#[test]
fn status_byte_without_data_is_not_logged_as_unhandled() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 1,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut ca = CaDevice::new(0, Arc::new(ManualClock::new(0, 0)));
    // Slot 0, connection 1: T_SB with status 0x00, then an unknown tag 0x99.
    ca.set_link(Some(Box::new(ScriptedLink(VecDeque::from([
        vec![0, 1, T_SB, 2, 1, 0x00],
        vec![0, 1, 0x99, 2, 1, 0x00],
    ])))));
    let _ = ca.read_tpdu();
    let _ = ca.read_tpdu();

    let lines = lines.lock().unwrap();
    let unhandled: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("unhandled tpdu_tag"))
        .collect();
    assert_eq!(unhandled.len(), 1, "{lines:#?}");
    assert!(unhandled[0].contains("0x99"), "{lines:#?}");
}
