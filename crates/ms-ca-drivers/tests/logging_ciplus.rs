//! The plain-CI notice when CI+ certificates are present (DEV-151).
//!
//! Lives in its own binary: [`ms_log::init`] may only run once per process.

use std::io;
use std::sync::{Arc, Mutex};

use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_READY};
use ms_ca_drivers::en50221::{AdapterInfo, CaLink, CaOpener, CaRegistry, ManualClock, SlotInfo};
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

/// A ready slot that accepts every write and never answers.
struct SilentLink;

impl CaLink for SilentLink {
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

/// Opens a silent slot on a host that has the CI+ root certificate.
struct WithRootCert;

impl CaOpener for WithRootCert {
    fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
        Ok(Box::new(SilentLink))
    }

    fn has_ciplus_root_cert(&self) -> bool {
        true
    }
}

#[test]
fn plain_ci_notice_is_logged_once_per_slot() {
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    let mut reg = CaRegistry::new(Arc::new(ManualClock::new(0, 0)));
    let info = AdapterInfo::dvb(2, 0, 2);
    let _ = reg.init_dev(&info, &mut WithRootCert);
    // A re-init of the same slot (after a teardown) does not repeat it.
    reg.get_mut(2).expect("device allocated").teardown();
    let _ = reg.init_dev(&info, &mut WithRootCert);
    assert!(reg.get(2).expect("device allocated").force_ci);

    let notice = "CA 2: CI+ certificates found but CI+ is not supported, using plain CI";
    let lines = lines.lock().unwrap();
    let count = lines.iter().filter(|line| line.ends_with(notice)).count();
    assert_eq!(count, 1, "{lines:#?}");
}
