//! Exact text of the CAPMT log lines ported from `src/ca.cpp` in this crate.
//!
//! Lives in its own binary: [`ms_log::init`] may only run once per process.

use std::sync::{Arc, Mutex};

use ms_log::{Backend, Timestamp};

struct Capture(Arc<Mutex<Vec<String>>>);

impl Backend for Capture {
    fn timestamp(&self) -> Timestamp {
        // Fixed so the assertions do not depend on the clock.
        Timestamp::new(5, 3, 14, 7, 9, 42)
    }

    fn syslog(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }

    fn stdout(&self, line: &str) {
        self.0.lock().unwrap().push(line.to_owned());
    }
}

#[test]
fn capmt_lines_match_ca_cpp() {
    ms_log::set_thread_name("ca");
    let lines = Arc::new(Mutex::new(Vec::new()));
    ms_log::init(ms_log::Config {
        log: ms_log::LOG_GENERAL | ms_log::LOG_DVBCA,
        debug: 0,
        slog: 0,
        file_line: false,
        backend: Box::new(Capture(Arc::clone(&lines))),
    });

    // One program, one video stream; C++ prints pid/adapter as integers.
    let mut programs = ms_ca::ProgramTable::new();
    let id = programs.add(1, 100, 0x30);
    let program = programs.get_mut(id).unwrap();
    program.name = "Test".to_owned();
    program.add_stream(0x101, 0x02, false, true);

    let mut ca = ms_ca::CaDevice::new(1, 1, 0);
    let mut driver = ms_ca::RecordingDriver::default();
    assert_eq!(
        ca.process_pmt(id, false, &programs, &mut driver),
        ms_ca::TablesResult::Ok
    );
    // The same CAPMT again: found in place, reported as an update and not re-sent.
    assert_eq!(
        ca.process_pmt(id, true, &programs, &mut driver),
        ms_ca::TablesResult::Ok
    );
    assert_eq!(driver.sent.len(), 1, "unchanged CAPMT is not re-sent");

    // `create_capmt` of a slot whose first program is missing.
    let capmt = ms_ca::Capmt::with_programs(9, [ms_ca::capmt::PMT_INVALID; 2], 0, 100);
    let ctx = ms_ca::capmt::BuildCtx {
        programs: &programs,
        caid_filter: &[],
    };
    assert!(
        ms_ca::create_capmt(&capmt, ms_ca::capmt::CLM_ONLY, 8192, 0, false, true, ctx).is_err()
    );

    let hash = ms_ca::capmt_content_hash(&driver.sent[0]);
    let prefix = "[05/03 14:07:09.042 ca]: ";
    let expected = [
        format!(
            "{prefix}PMT CA 1 pmt 0 pid 48 (Test) ver 4294967295 sid 64 (100), \
             enabled_pmts 1, CLM_ONLY, PMTS to be send 0 -1 -1, pos 0"
        ),
        format!(
            "{prefix}PMT CA 1 pmt 0 pid 48 (Test) ver 0 sid 64 (100), \
             enabled_pmts 1, CLM_UPDATE, PMTS to be send 0 -1 -1, pos 0, update"
        ),
        format!("{prefix}CA 1: CAPMT for PMTs 0 -1 -1 unchanged ({hash:08X}), not re-sending"),
        format!("{prefix}create_capmt: PMT 9 not found"),
    ];
    assert_eq!(*lines.lock().unwrap(), expected);
}
