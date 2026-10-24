//! Ported test_ca.cpp tests for the host stack (device lifecycle, reads, sessions).

use super::consts::*;
use super::resources::find_handler;
use super::*;
use std::collections::VecDeque;
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

/// Scripted CA device: queued read results, recorded writes, optional write error.
#[derive(Default, Clone)]
struct Mock {
    reads: Arc<Mutex<VecDeque<io::Result<Vec<u8>>>>>,
    writes: Arc<Mutex<Vec<Vec<u8>>>>,
    write_error: Option<i32>,
    dropped: Arc<AtomicBool>,
}

impl Mock {
    fn boxed(&self) -> Option<Box<dyn CaLink>> {
        Some(Box::new(self.clone()))
    }

    fn push_read(&self, r: io::Result<Vec<u8>>) {
        self.reads.lock().unwrap().push_back(r);
    }

    fn take_writes(&self) -> Vec<Vec<u8>> {
        std::mem::take(&mut self.writes.lock().unwrap())
    }
}

impl Drop for Mock {
    fn drop(&mut self) {
        self.dropped.store(true, Ordering::SeqCst);
    }
}

impl CaLink for Mock {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.reads.lock().unwrap().pop_front() {
            Some(Ok(m)) => {
                buf[..m.len()].copy_from_slice(&m);
                Ok(m.len())
            }
            Some(Err(e)) => Err(e),
            None => Err(io::ErrorKind::WouldBlock.into()),
        }
    }
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(e) = self.write_error {
            return Err(io::Error::from_raw_os_error(e));
        }
        self.writes.lock().unwrap().push(buf.to_vec());
        Ok(buf.len())
    }
    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }
    fn slot_info(&mut self) -> io::Result<SlotInfo> {
        Ok(SlotInfo::default())
    }
    fn sleep_ms(&mut self, _: u32) {}
}

struct NoDevices;

impl CaOpener for NoDevices {
    fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
        Err(io::ErrorKind::NotFound.into())
    }
    fn has_ciplus_root_cert(&self) -> bool {
        false
    }
}

fn clock() -> (ManualClock, Arc<dyn Clock>) {
    let c = ManualClock::new(0, 1_700_000_000);
    (c.clone(), Arc::new(c))
}

fn dev(id: usize) -> (ManualClock, CaDevice) {
    let (m, c) = clock();
    (m, CaDevice::new(id, c))
}

fn session(number: u16, resource: u32) -> Option<Session> {
    Some(Session {
        number,
        resource,
        handler: find_handler(resource).unwrap(),
        ai_version: 0,
    })
}

// Ported from tests/test_ca.cpp:757
#[test]
fn get_authdata_filename() {
    assert_eq!(
        authdata_filename("/tmp", 4, b"CI PLUS CAM"),
        "/tmp/ci_auth_CI_PLUS_CAM_4.bin"
    );
}

#[test]
fn get_authdata_filename_never_leaves_cache_dir() {
    assert_eq!(
        authdata_filename("/tmp", 1, b"a/b\xffc\0d"),
        "/tmp/ci_auth_a_b_c_1.bin"
    );
}

// Ported from tests/test_ca.cpp:770
#[test]
fn get_ca_caids_string() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    assert_eq!(reg.caids_string(0, 64), "", "invalid empty CAID string");
    reg.device(0).set_forced_caids(&[0x0B00]);
    assert_eq!(
        reg.caids_string(0, 64),
        "0B00",
        "invalid single CAID string"
    );
    reg.device(0).set_forced_caids(&[0x0B00, 0x0B01, 0x0B02]);
    assert_eq!(reg.caids_string(0, 64), "0B00, 0B01, 0B02");
}

// Ported from tests/test_ca.cpp:1024
#[test]
fn ca_reconnect_due() {
    let (_, mut d) = dev(0);
    assert!(!d.reconnect_due(1000), "disabled device not due");
    d.enabled = true;
    d.set_link(Mock::default().boxed());
    assert!(!d.reconnect_due(1000), "live device not due");
    d.set_link(None);
    d.reconnect_next_try = 2000;
    assert!(!d.reconnect_due(1000), "interval not elapsed");
    assert!(d.reconnect_due(2000), "due at the deadline");
    assert!(d.reconnect_due(9000), "due after the deadline");
}

// Ported from tests/test_ca.cpp:1043 (CAPMT/PMT-mask release is M6: checked as the event)
#[test]
fn ca_teardown_releases_pmts() {
    let (_, mut d) = dev(2);
    d.enabled = true;
    d.state = CaState::Initialized;
    d.set_link(Mock::default().boxed());
    d.new_sock();
    d.set_forced_caids(&[1, 2, 3]);
    d.poll_fails = 2;
    d.datetime_next_send = 1234;
    d.pending_tag = TAG_CA_INFO_ENQUIRY;
    d.pending_session = 3;
    d.pending_since = 999;
    d.sessions[0] = session(7, EN50221_APP_RM_RESOURCEID);
    d.key = [[0xAB; 16]; 2];
    d.iv = [[0xCD; 16]; 2];

    d.teardown();

    assert!(d.enabled, "enabled stays sticky");
    assert_eq!(d.state, CaState::Inactive, "state reset");
    assert!(!d.has_link(), "fd reset");
    assert_eq!(d.sock(), None, "sock reset");
    assert!(d.caids().is_empty(), "caids cleared");
    assert_eq!(d.poll_fails, 0, "poll fails cleared");
    assert_eq!(d.datetime_next_send, 0, "datetime resend due");
    assert_eq!(d.pending_tag, 0, "pending tag cleared");
    assert_eq!(d.pending_session, 0, "pending session cleared");
    assert_eq!(d.pending_since, -1, "pending anchor cleared");
    assert_eq!(d.sessions().count(), 0, "sessions cleared");
    assert_eq!(d.key, [[0; 16]; 2], "keys cleared");
    assert_eq!(d.iv, [[0; 16]; 2], "keys cleared");
    assert_eq!(
        d.next_event(),
        Some(HostEvent::ReleasePmts),
        "pmts released"
    );
    // Idempotent: a second teardown changes nothing.
    d.teardown();
    assert_eq!(d.state, CaState::Inactive, "second teardown stable");
}

// Ported from tests/test_ca.cpp:1116
#[test]
fn ca_close_null_safe() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    assert!(!reg.close(-1, 0), "negative sid");
    assert!(!reg.close(MAX_ADAPTERS as i32, 0), "sid out of range");
    assert!(!reg.close(9, 0), "no device");

    // Happy path: the current socket tears the device down.
    let d = reg.device(9);
    d.enabled = true;
    d.state = CaState::Initialized;
    d.set_link(Mock::default().boxed());
    d.sock = Some(41);
    d.sessions[0] = session(1, EN50221_APP_RM_RESOURCEID);
    d.key = [[0xAB; 16]; 2];
    assert!(reg.close(9, 41), "close runs teardown");
    let d = reg.device(9);
    assert_eq!(d.state, CaState::Inactive, "state reset");
    assert!(!d.has_link(), "fd reset");
    assert_eq!(d.sock(), None, "sock reset");
    assert!(d.enabled, "enabled stays sticky");
    assert_eq!(d.sessions().count(), 0, "sessions cleared");
    assert_eq!(d.key, [[0; 16]; 2], "keys cleared");

    // Stale generation: an old socket must not wipe the new state.
    d.state = CaState::Initialized;
    d.sock = Some(43);
    assert!(!reg.close(9, 41), "stale close skipped");
    assert_eq!(reg.device(9).state, CaState::Initialized, "new state kept");
}

// Ported from tests/test_ca.cpp:1160
#[test]
fn is_ca_initializing_reconnect_pending() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    let d = reg.device(7);
    d.enabled = true;
    d.state = CaState::Inactive;
    assert!(reg.is_ca_initializing(7), "reconnect pending retries");
    reg.device(7).state = CaState::Active;
    assert!(reg.is_ca_initializing(7), "activating retries");
    reg.device(7).state = CaState::Initialized;
    assert!(!reg.is_ca_initializing(7), "initialized runs");
    reg.device(7).enabled = false;
    reg.device(7).state = CaState::Inactive;
    assert!(!reg.is_ca_initializing(7), "disabled skips");
    reg.remove(7);
    assert!(!reg.is_ca_initializing(7), "missing device skips");
    assert!(!reg.is_ca_initializing(-1), "negative index skips");
    assert!(
        !reg.is_ca_initializing(MAX_ADAPTERS as i32),
        "index over range skips"
    );
}

// Ported from tests/test_ca.cpp:1197
#[test]
fn ca_init_failed() {
    let (_, mut d) = dev(0);
    d.sock = Some(5);
    d.state = CaState::Active;
    d.init_failed();
    assert!(d.enabled, "stays enabled");
    assert!(!d.has_link(), "fd dropped");
    assert_eq!(d.sock(), None, "sock dropped");
    assert_eq!(d.state, CaState::Inactive, "state reset");

    let m = Mock::default();
    let dropped = Arc::clone(&m.dropped);
    d.set_link(m.boxed());
    drop(m);
    dropped.store(false, Ordering::SeqCst);
    d.init_failed();
    assert!(!d.has_link(), "open fd dropped");
    assert!(dropped.load(Ordering::SeqCst), "fd really closed");
}

// Ported from tests/test_ca.cpp:1223
#[test]
fn ca_reconnect_poller() {
    let (m, c) = clock();
    let mut reg = CaRegistry::new(c);
    let d = reg.device(60);
    d.enabled = true;
    let mut ad = AdapterInfo::dvb(60, 99, 0);

    let next = m.tick_ms() + 60000;
    reg.device(60).reconnect_next_try = next;
    reg.reconnect(|i| (i == 60).then_some(ad), &mut NoDevices);
    assert_eq!(
        reg.device(60).reconnect_next_try,
        next,
        "future interval skipped"
    );

    ad.enabled = false;
    reg.device(60).reconnect_next_try = 0;
    reg.reconnect(|i| (i == 60).then_some(ad), &mut NoDevices);
    assert_eq!(
        reg.device(60).reconnect_next_try,
        0,
        "disabled adapter skipped"
    );
    ad.enabled = true;

    m.advance(100);
    let before = m.tick_ms();
    reg.reconnect(|i| (i == 60).then_some(ad), &mut NoDevices);
    assert_eq!(
        reg.device(60).reconnect_next_try,
        before + CA_RECONNECT_INTERVAL_MS,
        "failure re-arms the timer one interval out"
    );
}

// Ported from tests/test_ca.cpp:1262
#[test]
fn ca_keepalive_counts_failures() {
    let (_, mut d) = dev(61);
    d.enabled = true;
    d.state = CaState::Active;
    d.timeout();
    assert_eq!(d.poll_fails, 1, "first failure counts");
    d.timeout();
    assert_eq!(d.poll_fails, 2, "second failure counts");
    assert!(!d.take_close_request());
    d.timeout();
    assert_eq!(d.poll_fails, 0, "third failure closes and resets");
    assert!(d.take_close_request(), "close was requested");

    let m = Mock::default();
    d.set_link(m.boxed());
    d.poll_fails = 2;
    d.timeout();
    assert_eq!(d.poll_fails, 0, "success resets the count");
    assert_eq!(
        m.take_writes(),
        [vec![0, 1, T_DATA_LAST, 1, 1]],
        "keepalive poll"
    );
}

#[test]
fn eio_write_requests_close() {
    let (_, mut d) = dev(0);
    let mut m = Mock::default();
    m.write_error = Some(5);
    d.set_link(m.boxed());
    assert!(!d.write_tpdu(T_DATA_LAST, &[]));
    assert!(d.take_close_request(), "EIO means the CAM was removed");
}

// Ported from tests/test_ca.cpp:1309
#[test]
fn ca_cmd_watchdog() {
    let (m, mut d) = dev(65);
    let t0: i64 = 1_000_000;
    m.set_tick(t0);
    d.enabled = true;
    d.state = CaState::Active;
    let link = Mock::default();
    d.set_link(link.boxed());
    d.new_sock();

    d.pending_tag = TAG_CA_INFO_ENQUIRY;
    d.pending_session = 3;
    d.pending_since = t0;
    d.cmd_watchdog(t0 + 1000);
    assert!(!d.take_close_request(), "fresh command not closed");
    assert_ne!(d.pending_tag, 0, "fresh command stays tracked");

    d.pending_tag = TAG_PROFILE_ENQUIRY;
    d.pending_session = 1;
    d.pending_since = t0;
    d.cmd_watchdog(t0 + CA_CMD_TIMEOUT_MS);
    assert!(d.take_close_request(), "stale command resets");
    assert_eq!(d.pending_tag, 0, "reset clears pending");
    assert_eq!(d.pending_since, -1, "reset disarms");

    d.pending_since = t0; // stalled init: nothing pending, quiet since t0
    d.cmd_watchdog(t0 + CA_CMD_TIMEOUT_MS);
    assert!(d.take_close_request(), "stalled init resets");
    assert_eq!(d.pending_since, -1, "init reset disarms");

    d.state = CaState::Initialized;
    d.pending_since = t0;
    d.cmd_watchdog(t0 + 10 * CA_CMD_TIMEOUT_MS);
    assert!(!d.take_close_request(), "initialized idles quietly");
    assert_eq!(d.pending_since, t0, "quiet keeps anchor");

    d.pending_tag = TAG_CA_INFO_ENQUIRY; // post-init commands still guarded
    d.pending_session = 3;
    d.pending_since = t0;
    d.cmd_watchdog(t0 + CA_CMD_TIMEOUT_MS);
    assert!(d.take_close_request(), "post-init stale resets");

    d.state = CaState::Active;
    d.pending_since = -1; // fully unarmed: late ticks stay quiet
    d.cmd_watchdog(t0 + 10 * CA_CMD_TIMEOUT_MS);
    assert!(!d.take_close_request(), "unarmed never resets");

    d.pending_tag = TAG_CA_INFO_ENQUIRY; // tick-zero anchor still arms
    d.pending_session = 3;
    d.pending_since = 0;
    d.cmd_watchdog(CA_CMD_TIMEOUT_MS);
    assert!(d.take_close_request(), "zero anchor fires");

    d.sessions[2] = session(3, EN50221_APP_CA_RESOURCEID);
    assert!(
        d.write_apdu(2, TAG_CA_INFO_ENQUIRY, &[]),
        "tracked send succeeds"
    );
    assert_eq!(d.pending_tag, TAG_CA_INFO_ENQUIRY, "tracked send arms");
    assert_eq!(d.pending_session, 3, "armed on its session");
    let since = d.pending_since;
    m.advance(10);
    assert!(d.write_apdu(2, TAG_CA_INFO_ENQUIRY, &[]), "repeat succeeds");
    assert_eq!(d.pending_since, since, "repeat keeps deadline");
    assert!(d.write_apdu(2, TAG_CA_PMT, &[]), "one-way send succeeds");
    assert_eq!(d.pending_tag, TAG_CA_INFO_ENQUIRY, "one-way keeps tag");
    assert_eq!(d.pending_since, since, "one-way keeps deadline");

    let wrong = [0x9F, 0x80, 0x21, 0x00]; // APP_INFO, not our reply
    d.read_apdu(2, &wrong);
    assert_ne!(d.pending_tag, 0, "wrong reply keeps pending");
    let right = [0x9F, 0x80, 0x31, 0x00]; // CA_INFO, our reply
    d.read_apdu(4, &right);
    assert_ne!(d.pending_tag, 0, "off-session reply keeps pending");
    d.read_apdu(2, &right);
    assert_eq!(d.pending_tag, 0, "reply disarms");
    assert_eq!(d.pending_since, since, "reply keeps anchor");

    d.sessions[4] = session(5, EN50221_APP_CA_RESOURCEID);
    assert!(d.write_apdu(2, TAG_CA_INFO_ENQUIRY, &[]), "rearm succeeds");
    let aged = d.pending_since - 5000;
    d.pending_since = aged;
    assert!(
        d.write_apdu(4, TAG_CA_INFO_ENQUIRY, &[]),
        "second send succeeds"
    );
    assert_eq!(d.pending_session, 5, "newer session supersedes");
    assert!(d.pending_since > aged, "supersede refreshes deadline");
    d.read_apdu(2, &right);
    assert_ne!(d.pending_tag, 0, "old session cannot disarm");
    d.read_apdu(4, &right);
    assert_eq!(d.pending_tag, 0, "newer session disarms");

    assert!(d.write_apdu(2, TAG_CA_INFO_ENQUIRY, &[]), "rearm2 succeeds");
    let aged2 = d.pending_since - 5000;
    d.pending_since = aged2;
    assert!(
        d.write_apdu(2, TAG_PROFILE_ENQUIRY, &[]),
        "other tag succeeds"
    );
    assert_eq!(d.pending_tag, TAG_PROFILE_ENQUIRY, "other tag supersedes");
    assert!(d.pending_since > aged2, "overwrite refreshes deadline");
    let prof = [0x9F, 0x80, 0x11, 0x00]; // PROFILE, our reply
    d.read_apdu(2, &prof);
    assert_eq!(d.pending_tag, 0, "other reply disarms");

    assert!(
        d.write_apdu(2, TAG_PROFILE_ENQUIRY, &[]),
        "profile send succeeds"
    );
    assert_eq!(d.pending_tag, TAG_PROFILE_ENQUIRY, "profile send arms");
    d.read_apdu(2, &prof);
    assert_eq!(d.pending_tag, 0, "profile reply disarms");

    assert!(
        d.write_apdu(2, TAG_APP_INFO_ENQUIRY, &[]),
        "appinfo send succeeds"
    );
    assert_eq!(d.pending_tag, TAG_APP_INFO_ENQUIRY, "appinfo send arms");
    d.read_apdu(2, &[0x9F, 0x80, 0x21, 0x00]);
    assert_eq!(d.pending_tag, 0, "appinfo reply disarms");

    assert!(
        d.write_apdu(2, CIPLUS_TAG_OPERATOR_INFO_REQ, &[]),
        "oprf send succeeds"
    );
    assert_eq!(d.pending_tag, CIPLUS_TAG_OPERATOR_INFO_REQ, "oprf arms");
    d.read_apdu(2, &[0x9F, 0x9C, 0x05, 0x00]); // OPERATOR_INFO
    assert_eq!(d.pending_tag, 0, "oprf reply disarms");

    assert!(
        d.write_apdu(2, CIPLUS_TAG_OPERATOR_SEARCH_START, &[]),
        "search send succeeds"
    );
    assert_eq!(
        d.pending_tag, CIPLUS_TAG_OPERATOR_SEARCH_START,
        "search arms"
    );
    d.read_apdu(2, &[0x9F, 0x9C, 0x07, 0x00]); // SEARCH_STATUS, progress
    assert_eq!(d.pending_tag, 0, "search reply disarms");

    assert!(d.write_apdu(2, TAG_CA_INFO, &[]), "reply-tag send succeeds");
    assert_eq!(d.pending_tag, 0, "reply tag never arms");
    d.set_link(None);
    assert!(
        !d.write_apdu(2, TAG_CA_INFO_ENQUIRY, &[]),
        "failed write errors"
    );
    assert_eq!(d.pending_tag, 0, "failed write never arms");
    d.set_link(link.boxed());

    d.pending_tag = CIPLUS_TAG_OPERATOR_SEARCH_START;
    d.pending_session = 3;
    d.read_apdu(2, &[0x9F, 0x9C, 0x09, 0x00]); // TUNE ends the search ladder
    assert_eq!(d.pending_tag, 0, "ladder reply disarms");
    assert_eq!(d.pending_session, 0, "ladder clears session");

    d.pending_tag = TAG_CA_INFO_ENQUIRY;
    d.pending_session = 3;
    d.pending_since = t0;
    d.handle_spdu(&[ST_CLOSE_SESSION_REQUEST, 0x02, 0x00, 0x04]);
    assert_eq!(d.pending_tag, TAG_CA_INFO_ENQUIRY, "other keeps pending");
    assert_eq!(d.pending_since, t0, "other keeps deadline");

    m.advance(10);
    d.handle_spdu(&[ST_CLOSE_SESSION_REQUEST, 0x02, 0x00, 0x03]);
    assert_eq!(d.pending_tag, 0, "close clears pending");
    assert_eq!(d.pending_since, m.tick_ms(), "close restarts anchor");

    d.state = CaState::Active; // stale anchor, then a close, then a tick
    d.pending_since = t0;
    d.handle_spdu(&[ST_CLOSE_SESSION_REQUEST, 0x02, 0x00, 0x03]);
    let hi = m.tick_ms();
    d.cmd_watchdog(hi + 1000);
    assert!(!d.take_close_request(), "no instant reset");
    d.cmd_watchdog(hi + CA_CMD_TIMEOUT_MS);
    assert!(d.take_close_request(), "quiet after close resets");
}

// Ported from tests/test_ca.cpp:1534 (the NULL-pointer guards have no Rust equivalent)
#[test]
fn ca_read_tpdu_outcomes() {
    let (_, mut d) = dev(62);
    assert_eq!(d.read_tpdu(), ReadOutcome::Eof, "no fd");
    let m = Mock::default();
    d.set_link(m.boxed());
    assert_eq!(d.read_tpdu(), ReadOutcome::Again, "EAGAIN stays up");
    m.push_read(Ok(vec![0x00]));
    d.poll_fails = 5;
    assert_eq!(
        d.read_tpdu(),
        ReadOutcome::Data(Vec::new()),
        "short read parses"
    );
    assert_eq!(d.poll_fails, 0, "traffic resets the counter");
    m.push_read(Ok(Vec::new()));
    assert_eq!(d.read_tpdu(), ReadOutcome::Eof, "EOF tears down");
    m.push_read(Err(io::ErrorKind::Interrupted.into()));
    assert_eq!(d.read_tpdu(), ReadOutcome::Again, "EINTR stays up");
}

#[test]
fn read_tpdu_answers_ctc_and_status_byte() {
    let (_, mut d) = dev(0);
    let m = Mock::default();
    d.set_link(m.boxed());
    // CTC_REPLY then T_SB with data pending
    m.push_read(Ok(vec![0, 1, T_CTC_REPLY, 1, 1, T_SB, 2, 1, 0x80]));
    assert_eq!(d.read_tpdu(), ReadOutcome::Data(Vec::new()));
    assert_eq!(d.state, CaState::Active);
    assert_eq!(
        m.take_writes(),
        [vec![0, 1, T_DATA_LAST, 1, 1], vec![0, 1, T_RCV, 1, 1]]
    );
    // two data objects are joined; a bad length stops the walk
    m.push_read(Ok(vec![
        0,
        1,
        T_DATA_MORE,
        3,
        1,
        9,
        8,
        T_DATA_LAST,
        2,
        1,
        7,
        T_SB,
        9,
        1,
    ]));
    assert_eq!(d.read_tpdu(), ReadOutcome::Data(vec![9, 8, 7]));
    m.push_read(Ok(vec![0, 1, T_DELETE_TC, 1, 1]));
    assert_eq!(
        d.read_tpdu(),
        ReadOutcome::Eof,
        "delete TC resets the module"
    );
}

#[test]
fn read_tpdu_status_byte_without_data_is_quiet() {
    let (_, mut d) = dev(0);
    let m = Mock::default();
    d.set_link(m.boxed());
    m.push_read(Ok(vec![0, 1, T_SB, 2, 1, 0x00]));
    assert_eq!(d.read_tpdu(), ReadOutcome::Data(Vec::new()));
    assert!(m.take_writes().is_empty(), "no T_RCV without data");
}

#[test]
fn read_tpdu_status_byte_with_data_requests_it() {
    let (_, mut d) = dev(0);
    let m = Mock::default();
    d.set_link(m.boxed());
    m.push_read(Ok(vec![0, 1, T_SB, 2, 1, 0x80]));
    assert_eq!(d.read_tpdu(), ReadOutcome::Data(Vec::new()));
    assert_eq!(m.take_writes(), [vec![0, 1, T_RCV, 1, 1]]);
}

// Ported from tests/test_ca.cpp:1580 (NULL-pointer guards have no Rust equivalent)
#[test]
fn ca_read_enigma_removal() {
    let (_, mut d) = dev(63);
    d.mode = LinkMode::Enigma;
    d.enabled = true;
    d.state = CaState::Active;
    d.set_link(Mock::default().boxed());

    assert_eq!(d.read_enigma(true), ReadOutcome::Again, "removal read ok");
    assert!(d.take_close_request(), "removal requests close");
    assert_eq!(d.read_enigma(false), ReadOutcome::Again, "plain poll quiet");
    assert!(!d.take_close_request(), "no close without PRI");
    assert_eq!(
        d.read_enigma(true),
        ReadOutcome::Again,
        "pure PRI requests close"
    );
    assert!(d.take_close_request(), "close on pure PRI");
    d.state = CaState::Inactive;
    assert_eq!(
        d.read_enigma(true),
        ReadOutcome::Again,
        "inactive stays quiet"
    );
    assert!(!d.take_close_request(), "no close when inactive");
}

// Ported from tests/test_ca.cpp:1637 (the tables CA bit belongs to the M6 reconciler)
#[test]
fn dvbca_init_dev_guards() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    reg.device(64).state = CaState::Initialized;
    let mut ad = AdapterInfo::dvb(64, 0, 0);
    assert_eq!(
        reg.init_dev(&ad, &mut NoDevices),
        InitResult::Ok,
        "live device ok"
    );
    ad.kind = AdapterKind::Other;
    ad.id = 65;
    assert_eq!(
        reg.init_dev(&ad, &mut NoDevices),
        InitResult::NoRetry,
        "wrong adapter type"
    );
    ad.id = -1;
    assert_eq!(
        reg.init_dev(&ad, &mut NoDevices),
        InitResult::NoRetry,
        "negative adapter id"
    );
    ad.id = MAX_ADAPTERS as i32;
    assert_eq!(
        reg.init_dev(&ad, &mut NoDevices),
        InitResult::NoRetry,
        "id out of range"
    );
    reg.close_dev(&ad);
    ad.kind = AdapterKind::Dvb;
    ad.id = 66;
    assert_eq!(
        reg.init_dev(&ad, &mut NoDevices),
        InitResult::NoRetry,
        "no CA device"
    );
}

// Ported from tests/test_ca.cpp:1685
#[test]
fn ca_timeout_branches() {
    let (m, c) = clock();
    let mut reg = CaRegistry::new(c);
    reg.timeout(-1);
    reg.timeout(MAX_ADAPTERS as i32);
    reg.timeout(66);

    let link = Mock::default();
    let d = reg.device(66);
    d.enabled = true;
    d.set_link(link.boxed());
    d.state = CaState::Inactive;
    reg.timeout(66);
    assert_eq!(reg.device(66).poll_fails, 0, "INACTIVE poll succeeds");
    assert_eq!(
        link.take_writes(),
        [vec![0, 1, T_CREATE_TC, 1, 1]],
        "INACTIVE polls with create TC"
    );

    let d = reg.device(66);
    d.mode = LinkMode::Enigma;
    d.set_link(None);
    d.state = CaState::Active;
    d.poll_fails = 0;
    reg.timeout(66);
    assert_eq!(reg.device(66).poll_fails, 0, "enigma counts nothing");

    let d = reg.device(66);
    d.mode = LinkMode::Tpdu;
    d.state = CaState::Initialized;
    d.datetime_response_interval = 30000;
    d.datetime_next_send = -1;
    m.advance(5);
    let before = m.tick_ms();
    reg.timeout(66);
    assert!(
        reg.device(66).datetime_next_send >= before + 30000,
        "datetime re-armed"
    );
}

// Ported from tests/test_ca.cpp:1731
#[test]
fn ca_read_guards() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    reg.read(-1, &[0x91, 0x04, 0, 1, 0, 0x41]);
    reg.read(MAX_ADAPTERS as i32, &[0x91, 0x04, 0, 1, 0, 0x41]);
    reg.read(67, &[0x91, 0x04, 0, 1, 0, 0x41]);
    assert!(reg.get(67).is_none(), "no device");
}

// Ported from tests/test_ca.cpp:1747
#[test]
fn ca_app_info_name_bounds() {
    let (_, mut d) = dev(71);
    // ST_OPEN_SESSION_REQUEST for the Application Information resource
    d.handle_spdu(&[0x91, 0x04, 0x00, 0x02, 0x00, 0x41]);
    assert_eq!(
        d.session(1).map(|s| s.resource),
        Some(0x20041),
        "AI session"
    );

    // ST_SESSION_NUMBER 1, APP_INFO tag, length 206, name length 200
    let mut info = vec![b'A'; 4 + 5 + 206];
    let head = [
        0x90, 0x02, 0x00, 0x01, 0x9F, 0x80, 0x21, 0x81, 0xCE, 0x01, 0x00, 0x00, 0x00, 0x00, 0xC8,
    ];
    info[..head.len()].copy_from_slice(&head);
    d.handle_spdu(&info);
    assert_eq!(d.ci_name_bytes().len(), CI_NAME_MAX, "name cut");
    assert!(d.ci_name_bytes().iter().all(|&b| b == b'A'));

    // APP_INFO of 3 bytes: too short for the name length byte
    d.set_ci_name(b"keep");
    d.handle_spdu(&[
        0x90, 0x02, 0x00, 0x01, 0x9F, 0x80, 0x21, 0x03, 0x01, 0x00, 0x00,
    ]);
    assert_eq!(d.ci_name(), "keep", "short app info ignored");
}

// Ported from tests/test_ca.cpp:1793
#[test]
fn ca_open_session_table_full() {
    let (_, mut d) = dev(72);
    for i in 0..MAX_SESSIONS {
        d.sessions[i] = session(i as u16 + 1, EN50221_APP_AI_RESOURCEID);
    }
    let link = Mock::default();
    d.set_link(link.boxed());
    d.mode = LinkMode::Enigma; // SPDUs go to the fd unwrapped
    d.handle_spdu(&[0x91, 0x04, 0x00, 0x02, 0x00, 0x41]);
    let w = link.take_writes();
    assert_eq!(w.len(), 1);
    assert_eq!(w[0].len(), 9, "response written");
    assert_eq!(w[0][0], ST_OPEN_SESSION_RESPONSE, "open session response");
    assert_eq!(w[0][2], 0xF0, "refused");
    assert_eq!(w[0][7..], [0xFF, 0xFF], "no session number");
}

// Ported from tests/test_ca.cpp:1831
#[test]
fn ca_open_session_unknown_resource() {
    let (_, mut d) = dev(73);
    let link = Mock::default();
    d.set_link(link.boxed());
    d.mode = LinkMode::Enigma;
    // ST_OPEN_SESSION_REQUEST for TS 103 205 MMI, which has no handler
    d.handle_spdu(&[0x91, 0x04, 0x00, 0x40, 0x00, 0x81]);
    let w = link.take_writes();
    assert_eq!(w[0].len(), 9, "response written");
    assert_eq!(w[0][0], ST_OPEN_SESSION_RESPONSE, "open session response");
    assert_eq!(w[0][2], 0xF0, "unknown resource refused");
    assert_eq!(d.sessions().count(), 0, "no session opened");
    // An APDU for that session number must not reach a handler
    d.handle_spdu(&[0x90, 0x02, 0x00, 0x01, 0x9F, 0x88, 0x01, 0x00]);
    assert!(link.take_writes().is_empty());
}

// Ported from tests/test_ca.cpp:1871 (the tables CA bit belongs to the M6 reconciler)
#[test]
fn ca_reconnect_success() {
    let (_, c) = clock();
    let mut reg = CaRegistry::new(c);
    let d = reg.device(68);
    d.enabled = true;
    d.state = CaState::Initialized;
    d.reconnect_next_try = -1;
    let ad = AdapterInfo::dvb(68, 0, 0);
    reg.reconnect(|i| (i == 68).then_some(ad), &mut NoDevices);
    assert_eq!(
        reg.device(68).reconnect_next_try,
        0,
        "timer cleared on success"
    );
}

// Ported from tests/test_ca.cpp:1898
#[test]
fn ca_read_resets_fails() {
    let (_, mut d) = dev(69);
    d.state = CaState::Inactive;
    d.poll_fails = 5;
    d.handle_spdu(&[0x00, 0x00, 0x00, 0x00]);
    assert_eq!(d.state, CaState::Active, "inactive->active");
    assert_eq!(d.poll_fails, 0, "fails reset");
}

// Ported from tests/test_ca.cpp:1973
#[test]
fn ca_read_bad_session() {
    let (_, mut d) = dev(70);
    d.state = CaState::Active;
    let link = Mock::default();
    d.set_link(link.boxed());
    d.handle_spdu(&[ST_CLOSE_SESSION_REQUEST, 0x02, 0x00, 65]);
    d.handle_spdu(&[ST_CLOSE_SESSION_REQUEST, 0x02, 0x00, 0x00]);
    d.handle_spdu(&[ST_SESSION_NUMBER, 0x02, 0x00, 99]);
    assert!(link.take_writes().is_empty(), "bad sessions dropped");
    d.handle_spdu(&[ST_SESSION_NUMBER, 0x02, 0x00, 0x01]);
    assert_eq!(d.sessions().count(), 0, "table untouched");
}

#[test]
fn truncated_spdus_are_dropped() {
    let (_, mut d) = dev(0);
    let link = Mock::default();
    d.set_link(link.boxed());
    for spdu in [
        &[0x91][..],
        &[0x91, 0x04, 0x00, 0x01],
        &[0x95, 0x02, 0x00],
        &[0x90, 0x02, 0x00],
        &[0x90, 0x83, 0, 0, 0],
    ] {
        d.handle_spdu(spdu);
    }
    assert!(link.take_writes().is_empty());
    // an APDU longer than its SPDU is not dispatched
    d.sessions[0] = session(1, EN50221_APP_RM_RESOURCEID);
    d.handle_spdu(&[0x90, 0x02, 0x00, 0x01, 0x9F, 0x80, 0x10, 0x05, 0x00]);
    assert!(link.take_writes().is_empty());
}
