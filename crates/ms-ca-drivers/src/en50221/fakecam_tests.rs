//! The host stack driven by `ms-fakecam`, checked through its APDU log.

use super::clock::{Clock, ManualClock};
use super::consts::*;
use super::device::{CaDevice, CaState, HostEvent};
use super::link::{
    AdapterInfo, CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY, CaLink, CaOpener, SlotInfo,
};
use super::registry::{CaRegistry, InitResult};
use super::resources::APPLICATION_HANDLERS;
use ms_fakecam::record::{Direction, Event};
use ms_fakecam::{CamConfig, FakeCam};
use std::collections::VecDeque;
use std::io;
use std::sync::{Arc, Mutex};

const UNIX: u64 = 1_700_000_000;

/// CA device backed by a [`FakeCam`]; writes return the CAM's R_TPDU.
struct CamLink {
    cam: Arc<Mutex<FakeCam>>,
    rx: VecDeque<Vec<u8>>,
}

impl CaLink for CamLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let Some(msg) = self.rx.pop_front() else {
            return Err(io::ErrorKind::WouldBlock.into());
        };
        assert!(msg.len() <= buf.len(), "R_TPDU fits the host buffer");
        buf[..msg.len()].copy_from_slice(&msg);
        Ok(msg.len())
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut cam = self.cam.lock().unwrap();
        if let Some(reply) = cam.handle_tpdu(buf) {
            self.rx.push_back(reply);
        }
        Ok(buf.len())
    }

    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn slot_info(&mut self) -> io::Result<SlotInfo> {
        Ok(SlotInfo {
            kind: CA_CI_LINK,
            flags: CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY,
        })
    }

    fn sleep_ms(&mut self, _: u32) {}
}

struct CamOpener {
    cam: Arc<Mutex<FakeCam>>,
    /// `/etc/ssl/certs/root.pem` present.
    root_cert: bool,
}

impl CaOpener for CamOpener {
    fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
        Ok(Box::new(CamLink {
            cam: Arc::clone(&self.cam),
            rx: VecDeque::new(),
        }))
    }

    fn has_ciplus_root_cert(&self) -> bool {
        self.root_cert
    }
}

fn setup(config: CamConfig, force_ci: bool) -> (ManualClock, Arc<Mutex<FakeCam>>, CaDevice) {
    let clock = ManualClock::new(0, UNIX);
    let cam = Arc::new(Mutex::new(FakeCam::new(
        config,
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let mut dev = CaDevice::new(0, Arc::new(clock.clone()));
    dev.force_ci = force_ci;
    dev.enabled = true;
    dev.set_link(Some(Box::new(CamLink {
        cam: Arc::clone(&cam),
        rx: VecDeque::new(),
    })));
    assert!(dev.init_en50221(), "the fake slot resets ready");
    assert!(dev.service(), "the link stays up");
    (clock, cam, dev)
}

/// The APDUs the host sent, in order, as `(tag, body)`.
fn host_apdus(cam: &FakeCam) -> Vec<(u32, Vec<u8>)> {
    cam.recorder()
        .apdus()
        .into_iter()
        .filter_map(|r| match (r.dir, r.event) {
            (Direction::HostToCam, Event::Apdu { tag, body, .. }) => Some((tag, body)),
            _ => None,
        })
        .collect()
}

fn host_profile() -> Vec<u8> {
    APPLICATION_HANDLERS
        .iter()
        .flat_map(|h| h.resource.to_be_bytes())
        .collect()
}

fn drain_events(dev: &mut CaDevice) -> Vec<HostEvent> {
    std::iter::from_fn(|| dev.next_event()).collect()
}

/// A CAM-queued SPDU only reaches the host when the host polls the link.
fn deliver(dev: &mut CaDevice, cam: &Arc<Mutex<FakeCam>>, resource: u32, tag: u32, body: &[u8]) {
    assert!(
        cam.lock().unwrap().send_to_resource(resource, tag, body),
        "session for {resource:08X}"
    );
    dev.timeout();
    assert!(dev.service(), "the link stays up");
}

#[test]
fn handshake_matches_the_fake_cam_apdu_log() {
    let config = CamConfig {
        caids: vec![0x0B00, 0x0B01],
        datetime_interval_s: 30,
        ..CamConfig::default()
    };
    let (_, cam, mut dev) = setup(config, false);

    assert_eq!(dev.state, CaState::Initialized, "CA_INFO initializes");
    assert_eq!(dev.caids(), [0x0B00, 0x0B01]);
    assert_eq!(dev.ci_name(), "Fake CAM");
    assert_eq!(
        dev.next_event(),
        Some(HostEvent::CaInfo {
            caids: vec![0x0B00, 0x0B01]
        })
    );
    let host: Vec<u32> = dev.sessions().map(|s| s.resource).collect();
    let cam = cam.lock().unwrap();
    let guest: Vec<u32> = cam.sessions().into_iter().map(|(_, r)| r).collect();
    assert_eq!(host, guest, "host and CAM agree on the sessions");
    assert_eq!(
        host,
        [
            EN50221_APP_RM_RESOURCEID,
            EN50221_APP_AI_RESOURCEID,
            EN50221_APP_CA_RESOURCEID,
            EN50221_APP_DATETIME_RESOURCEID,
            EN50221_APP_MMI_RESOURCEID,
        ]
    );
    assert_eq!(
        cam.host_resources(),
        APPLICATION_HANDLERS
            .iter()
            .map(|h| h.resource)
            .collect::<Vec<_>>()
    );
    let apdus = host_apdus(&cam);
    let tags: Vec<u32> = apdus.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        tags,
        [
            TAG_PROFILE_ENQUIRY,
            TAG_PROFILE_CHANGE,
            TAG_PROFILE,
            TAG_APP_INFO_ENQUIRY,
            TAG_CA_INFO_ENQUIRY,
            TAG_DATE_TIME,
        ]
    );
    assert_eq!(apdus[2].1, host_profile(), "full host profile");
    let when = ms_fakecam::host::date_time_body(UNIX);
    assert_eq!(apdus[5].1, when);
    assert_eq!(cam.last_date_time(), Some(when.as_slice()));
}

#[test]
fn force_ci_host_profile_stops_before_ci_plus() {
    let (_, cam, dev) = setup(CamConfig::default(), true);
    assert_eq!(dev.state, CaState::Initialized);
    let cam = cam.lock().unwrap();
    let expected: Vec<u32> = APPLICATION_HANDLERS[..6]
        .iter()
        .map(|h| h.resource)
        .collect();
    assert_eq!(cam.host_resources(), expected);
    let profile = host_apdus(&cam)
        .into_iter()
        .find(|(tag, _)| *tag == TAG_PROFILE)
        .expect("profile reply")
        .1;
    assert_eq!(profile.len(), 6 * 4, "CI profile only");
}

// DEV-151: without the `ci-plus` feature the slot still runs plain CI, so
// the host profile stops before the CI+ resources.
#[cfg(not(feature = "ci-plus"))]
#[test]
fn force_ci_with_root_cert_still_stops_before_ci_plus() {
    let cam = Arc::new(Mutex::new(FakeCam::new(
        CamConfig::default(),
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let mut reg = CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    let mut opener = CamOpener {
        cam: Arc::clone(&cam),
        root_cert: true,
    };
    assert_eq!(
        reg.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        InitResult::Ok
    );
    let dev = reg.get_mut(0).expect("device allocated");
    assert!(dev.force_ci, "plain CI is forced");
    assert!(dev.service(), "the link stays up");
    assert_eq!(dev.state, CaState::Initialized);
    let cam = cam.lock().unwrap();
    let expected: Vec<u32> = APPLICATION_HANDLERS[..6]
        .iter()
        .map(|h| h.resource)
        .collect();
    assert_eq!(cam.host_resources(), expected);
    let profile = host_apdus(&cam)
        .into_iter()
        .find(|(tag, _)| *tag == TAG_PROFILE)
        .expect("profile reply")
        .1;
    assert_eq!(profile.len(), 6 * 4, "EN 50221 resources only");
}

#[test]
fn refused_resource_is_logged_by_the_fake_cam() {
    let bad = TS103205_APP_MMI_RESOURCEID;
    let config = CamConfig {
        resources: vec![EN50221_APP_RM_RESOURCEID, bad, EN50221_APP_CA_RESOURCEID],
        ..CamConfig::default()
    };
    let (_, cam, dev) = setup(config, false);
    assert!(dev.find_session_for_resource(bad).is_none());
    assert_eq!(dev.state, CaState::Initialized);
    let cam = cam.lock().unwrap();
    let log = cam.recorder().to_untimed_text();
    assert!(log.contains(&format!("open_session {bad:08X}")), "{log}");
    assert!(
        log.contains(&format!("open_session_rsp s65535 {bad:08X} st=F0")),
        "{log}"
    );
}

#[test]
fn registry_init_dev_drives_the_fake_cam() {
    let cam = Arc::new(Mutex::new(FakeCam::new(
        CamConfig::default(),
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let clock = ManualClock::new(0, UNIX);
    let mut reg = CaRegistry::new(Arc::new(clock));
    let mut opener = CamOpener {
        cam: Arc::clone(&cam),
        root_cert: false,
    };
    assert_eq!(
        reg.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        InitResult::Ok
    );
    let dev = reg.get_mut(0).expect("device allocated");
    assert_eq!(dev.next_event(), Some(HostEvent::Opened));
    assert!(dev.service(), "the link stays up");
    assert_eq!(dev.state, CaState::Initialized);
    assert_eq!(dev.ci_name(), "Fake CAM");
    assert!(reg.has_ci);
}

// DEV-151: a successful slot init emits `cam_state` with the slot's mode.
#[cfg(feature = "diag-log")]
#[test]
fn registry_init_emits_cam_state() {
    // Initialise the capture before the registry emits anything.
    let _ = crate::test_log::captured_lines();
    let cam = Arc::new(Mutex::new(FakeCam::new(
        CamConfig::default(),
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let mut reg = CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    let mut opener = CamOpener {
        cam: Arc::clone(&cam),
        root_cert: false,
    };
    assert_eq!(
        reg.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        InitResult::Ok
    );

    let captured = crate::test_log::events("cam_state");
    let event = captured
        .iter()
        .find(|line| line.contains("\"state\":\"init\""))
        .unwrap_or_else(|| panic!("no cam_state event: {captured:?}"));
    assert!(event.contains("\"ca\":0"), "{event}");
    assert!(event.contains("\"state\":\"init\""), "{event}");
    assert!(event.contains("\"mode\":\"ci\""), "{event}");
}

// A slot that never answers is reset by the command watchdog every 15 s; its
// init/reset loop is reported once, and a CAM that gets ready is reported again.
#[cfg(feature = "diag-log")]
#[test]
fn an_empty_slot_reports_its_init_reset_loop_once() {
    let _ = crate::test_log::captured_lines();
    let mut reg = CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    // No other test emits cam_state for CA 7, so the shared capture holds only these.
    let dev = reg.device(7);
    for _ in 0..3 {
        dev.report_state("init");
        dev.request_close();
    }
    dev.report_state("init");
    dev.report_state("ready");
    let states: Vec<String> = crate::test_log::events("cam_state")
        .iter()
        .filter(|line| line.contains("\"ca\":7,"))
        .filter_map(|line| line.split("\"state\":\"").nth(1))
        .filter_map(|rest| rest.split('"').next())
        .map(str::to_owned)
        .collect();
    assert_eq!(states, ["init", "reset", "ready"]);
    // Two muted init/reset pairs and the last init.
    let ready = crate::test_log::events("cam_state")
        .into_iter()
        .find(|line| line.contains("\"ca\":7,") && line.contains("\"state\":\"ready\""))
        .expect("ready event");
    assert!(ready.contains("\"suppressed\":5"), "{ready}");
}

#[test]
fn datetime_resends_on_the_configured_interval() {
    let config = CamConfig {
        datetime_interval_s: 30,
        ..CamConfig::default()
    };
    let (clock, cam, mut dev) = setup(config, false);
    let first = ms_fakecam::host::date_time_body(UNIX);
    assert_eq!(
        cam.lock().unwrap().last_date_time(),
        Some(first.as_slice()),
        "initial time"
    );

    clock.set_unix(UNIX + 60);
    clock.advance(30_001);
    dev.timeout();
    let later = ms_fakecam::host::date_time_body(UNIX + 60);
    assert_eq!(
        cam.lock().unwrap().last_date_time(),
        Some(later.as_slice()),
        "resend after the interval"
    );
}

#[test]
fn mmi_pin_enquiry_is_answered_from_the_configured_pin() {
    let (_, cam, mut dev) = setup(CamConfig::default(), false);
    dev.set_pin(b"1234");
    {
        let mut cam = cam.lock().unwrap();
        assert!(cam.send_to_resource(
            EN50221_APP_MMI_RESOURCEID,
            TAG_ENQUIRY,
            &[0x01, 4, b'P', b'I', b'N']
        ));
    }
    dev.timeout();
    assert!(dev.service());
    let events = drain_events(&mut dev);
    assert!(events.contains(&HostEvent::MmiEnquiry {
        text: "PIN".into(),
        blind: true,
        answer_len: 4,
    }));
    let cam = cam.lock().unwrap();
    let apdus = host_apdus(&cam);
    let tail = &apdus[apdus.len() - 2..];
    assert_eq!(tail[0].0, TAG_ANSWER);
    assert_eq!(tail[0].1, [MMI_ANSW_ID_ANSWER, b'1', b'2', b'3', b'4']);
    assert_eq!(tail[1].0, TAG_CLOSE_MMI);
    assert_eq!(tail[1].1, [MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
}

#[test]
fn mmi_menu_is_logged_then_cancelled() {
    let (_, cam, mut dev) = setup(CamConfig::default(), false);
    let mut body = vec![0x01];
    for text in ["Title", "Sub", "Bottom", "One", "Two"] {
        body.extend_from_slice(&[0x9F, 0x88, 0x03, text.len() as u8]);
        body.extend_from_slice(text.as_bytes());
    }
    assert!(
        cam.lock()
            .unwrap()
            .send_to_resource(EN50221_APP_MMI_RESOURCEID, TAG_MENU_LAST, &body)
    );
    dev.timeout();
    assert!(dev.service());
    let events = drain_events(&mut dev);
    let lines: Vec<String> = ["Title", "Sub", "Bottom", "One", "Two"]
        .iter()
        .map(|s| (*s).to_owned())
        .collect();
    assert!(events.contains(&HostEvent::MmiMenu { lines }));
    let cam = cam.lock().unwrap();
    let apdus = host_apdus(&cam);
    let tail = &apdus[apdus.len() - 2..];
    assert_eq!(tail[0].0, TAG_MENU_ANSWER);
    assert_eq!(tail[0].1, [0x01]);
    assert_eq!(tail[1].0, TAG_CLOSE_MMI);
    assert_eq!(tail[1].1, [MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
}

#[test]
fn cam_closed_ca_session_returns_to_active() {
    let (_, cam, mut dev) = setup(CamConfig::default(), false);
    drain_events(&mut dev);
    assert!(cam.lock().unwrap().request_close(EN50221_APP_CA_RESOURCEID));
    dev.timeout();
    assert!(dev.service());
    assert_eq!(dev.state, CaState::Active, "CA close leaves ACTIVE");
    assert_eq!(drain_events(&mut dev), [HostEvent::CaSessionClosed]);
    assert!(!cam.lock().unwrap().ca_ready());
}

#[test]
fn ciplus_app_info_v3_gets_the_data_rate_info() {
    let config = CamConfig {
        resources: vec![EN50221_APP_RM_RESOURCEID, CIPLUS_APP_AI_RESOURCEID],
        ..CamConfig::default()
    };
    let (_, cam, mut dev) = setup(config, false);
    assert!(
        dev.find_session_for_resource(CIPLUS_APP_AI_RESOURCEID)
            .is_some()
    );
    let mut body = vec![0x01, 0x00, 0x00, 0x00, 0x00, 6];
    body.extend_from_slice(b"CI+CAM");
    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_AI_RESOURCEID,
        CIPLUS_TAG_APP_INFO,
        &body,
    );
    assert_eq!(dev.ci_name(), "CI+CAM");
    let cam = cam.lock().unwrap();
    let last = host_apdus(&cam).pop().expect("host APDU");
    assert_eq!(last.0, CI_DATA_RATE_INFO);
    assert_eq!(last.1, [CIPLUS_DATA_RATE_96_MBPS]);
}

#[test]
fn ciplus_language_upgrade_and_operator_stubs() {
    let config = CamConfig {
        resources: vec![
            EN50221_APP_RM_RESOURCEID,
            CIPLUS_APP_LANG_RESOURCEID,
            CIPLUS_APP_UPGR_RESOURCEID,
            CIPLUS_APP_OPRF_RESOURCEID,
        ],
        ..CamConfig::default()
    };
    let (_, cam, mut dev) = setup(config, false);

    // APP_LANG_create answers both enquiries up front.
    let apdus = host_apdus(&cam.lock().unwrap());
    assert!(apdus.contains(&(CIPLUS_TAG_COUNTRY_REPLY, b"USA".to_vec())));
    assert!(apdus.contains(&(CIPLUS_TAG_LANG_REPLY, b"eng".to_vec())));

    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_LANG_RESOURCEID,
        CIPLUS_TAG_COUNTRY_ENQ,
        &[],
    );
    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_LANG_RESOURCEID,
        CIPLUS_TAG_LANG_ENQ,
        &[],
    );
    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_UPGR_RESOURCEID,
        CIPLUS_TAG_FIRMWARE_UPGR,
        &[],
    );
    let asked = host_apdus(&cam.lock().unwrap());
    let tail = &asked[asked.len() - 3..];
    assert_eq!(tail[0].0, CIPLUS_TAG_COUNTRY_REPLY);
    assert_eq!(tail[1].0, CIPLUS_TAG_LANG_REPLY);
    assert_eq!(tail[2].0, CIPLUS_TAG_FIRMWARE_UPGR_REPLY);
    assert_eq!(tail[2].1, [0]);

    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_OPRF_RESOURCEID,
        CIPLUS_TAG_OPERATOR_STATUS,
        &[0, 0, 0],
    );
    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_OPRF_RESOURCEID,
        CIPLUS_TAG_OPERATOR_INFO,
        &[0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
    );
    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_OPRF_RESOURCEID,
        CIPLUS_TAG_OPERATOR_TUNE,
        &[0, 0],
    );
    let asked = host_apdus(&cam.lock().unwrap());
    let tail = &asked[asked.len() - 3..];
    assert_eq!(tail[0].0, CIPLUS_TAG_OPERATOR_INFO_REQ);
    assert_eq!(tail[1].0, CIPLUS_TAG_OPERATOR_SEARCH_START);
    assert_eq!(tail[1].1, [0x03, 0x01, 0x16, 0x19, 0x02, 0x43, 0x79, 0x00]);
    assert_eq!(tail[2].0, CIPLUS_TAG_OPERATOR_TUNE_STATUS);
    assert_eq!(tail[2].1, [0xFF, 0x00, 0x00, 0x30, 0x00]);

    deliver(
        &mut dev,
        &cam,
        CIPLUS_APP_UPGR_RESOURCEID,
        CIPLUS_TAG_FIRMWARE_UPGR_COMPLT,
        &[0, 0, 0, 0, 1],
    );
    assert!(dev.take_close_request(), "reset requested after upgrade");
}

// --- --cam-recovery: a CAM whose output stays scrambled after an MMI message.
// Plain CI (force_ci), so there are no CI+ keys: the host judges each 10 s window.

const WINDOW_MS: i64 = 10_000;

/// A slot with the watchdog on that has not seen any output yet.
fn cam_recovery_cold(timeout_ms: i64) -> (ManualClock, Arc<Mutex<FakeCam>>, CaDevice) {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    dev.cam_recovery_timeout_ms = timeout_ms;
    (clock, cam, dev)
}

/// A slot that was descrambling at time 0: a clean window precedes any MMI message.
fn cam_recovery_rig(timeout_ms: i64) -> (ManualClock, Arc<Mutex<FakeCam>>, CaDevice) {
    let (clock, cam, mut dev) = cam_recovery_cold(timeout_ms);
    dev.cam_recovery_window(WINDOW_MS, 0, 50_000, true);
    (clock, cam, dev)
}

/// Shows an MMI menu and lets the host read it.
fn show_menu(cam: &Arc<Mutex<FakeCam>>, dev: &mut CaDevice) {
    assert!(
        cam.lock()
            .unwrap()
            .send_mmi_menu(&["Title", "Subtitle", "Bottom", "Item"])
    );
    dev.timeout();
    assert!(dev.service());
    drain_events(dev);
}

/// Advances the clock to `at` and judges the 10 s window that ends there.
fn window_at(clock: &ManualClock, dev: &mut CaDevice, at: i64, scrambled: u64, clear: u64) {
    clock.advance(at - clock.tick_ms());
    dev.cam_recovery_window(WINDOW_MS, scrambled, clear, true);
}

#[test]
fn cam_recovery_fires_once_the_output_stays_scrambled() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 10_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "a window that starts at the anchor is not judged"
    );
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "the first judged stuck window starts a run"
    );
    window_at(&clock, &mut dev, 30_000, 50_000, 0);
    assert!(dev.take_close_request(), "stuck for the timeout");
    assert_eq!(dev.cam_recovery_since, -1, "the reset disarms");
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_close_request(), "one reset per episode");
}

#[test]
fn cam_recovery_one_stuck_window_after_quiet_windows_does_not_fire() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    // Near-silent windows are neutral: they make a window neither stuck nor clean.
    window_at(&clock, &mut dev, 20_000, 100, 0);
    window_at(&clock, &mut dev, 30_000, 0, 200);
    window_at(&clock, &mut dev, 40_000, 100, 0);
    assert!(
        !dev.take_close_request(),
        "neutral windows do not count as stuck"
    );
    // The stuck window ends 50 s after the anchor, but it is the only stuck window.
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "a single stuck window is not a sustained run"
    );
    assert_eq!(dev.cam_recovery_stuck_since, 40_000, "the run starts here");
    window_at(&clock, &mut dev, 60_000, 50_000, 0);
    assert!(
        dev.take_close_request(),
        "a run of stuck windows spanning the timeout fires"
    );
}

#[test]
fn cam_recovery_neutral_window_breaks_a_stuck_run() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert_eq!(dev.cam_recovery_stuck_since, 10_000, "the run starts");
    window_at(&clock, &mut dev, 30_000, 100, 0);
    assert_eq!(dev.cam_recovery_stuck_since, -1, "a neutral window ends it");
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert_eq!(dev.cam_recovery_stuck_since, 30_000, "a new run starts");
    assert!(
        !dev.take_close_request(),
        "the new run is only 10 s long at 40 s"
    );
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(dev.take_close_request(), "the new run spans the timeout");
}

#[test]
fn cam_recovery_clean_window_breaks_a_stuck_run() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 30_000, 0, 50_000);
    assert_eq!(dev.cam_recovery_stuck_since, -1, "a clean window ends it");
    assert_eq!(dev.cam_recovery_since, -1, "a judged clean window disarms");
    // Re-armed by an MMI message after the clean window: the old run does not count.
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(!dev.take_close_request(), "only 10 s of stuck windows");
    window_at(&clock, &mut dev, 60_000, 50_000, 0);
    assert!(dev.take_close_request(), "the new run spans the timeout");
}

#[test]
fn cam_recovery_needs_an_mmi_message() {
    let (clock, _cam, mut dev) = cam_recovery_rig(20_000);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_close_request(), "no MMI message, no reset");
}

#[test]
fn cam_recovery_is_off_by_default() {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    assert_eq!(dev.cam_recovery_timeout_ms, 0, "the knob defaults to off");
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, -1, "off means never armed");
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_close_request());
}

#[test]
fn cam_recovery_disarms_when_the_output_is_clear_again() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 0, 50_000);
    assert_eq!(dev.cam_recovery_since, -1, "a judged clean window disarms");
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    window_at(&clock, &mut dev, 60_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "not re-armed without an MMI message"
    );
}

#[test]
fn cam_recovery_threshold_and_share_boundaries() {
    // (scrambled, clear, fires): 90% exactly, one below, too few packets, and a low share.
    for (scrambled, clear, fires) in [
        (9_000, 1_000, true),
        (8_999, 1_001, false),
        (1_000, 0, true),
        (999, 0, false),
        (1_000, 200, false),
    ] {
        let (clock, cam, mut dev) = cam_recovery_rig(20_000);
        show_menu(&cam, &mut dev);
        window_at(&clock, &mut dev, 10_000, scrambled, clear);
        window_at(&clock, &mut dev, 20_000, scrambled, clear);
        window_at(&clock, &mut dev, 30_000, scrambled, clear);
        assert_eq!(
            dev.take_close_request(),
            fires,
            "scrambled {scrambled}, clear {clear}"
        );
    }
}

#[test]
fn cam_recovery_window_must_start_after_the_lag() {
    let (clock, cam, mut dev) = cam_recovery_rig(1);
    show_menu(&cam, &mut dev);
    // Starts at 1999 ms: inside the stats lag, not judged.
    window_at(&clock, &mut dev, 11_999, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "a window inside the lag is not judged"
    );
    // Starts at exactly the lag: judged.
    window_at(&clock, &mut dev, 12_000, 50_000, 0);
    assert!(dev.take_close_request(), "a window at the lag is judged");
}

#[test]
fn cam_recovery_keeps_the_anchor_through_a_straddling_clear_window() {
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    clock.advance(8_000);
    show_menu(&cam, &mut dev);
    // The zap MMI lands mid-window: this window is mostly clear, but starts before the anchor.
    window_at(&clock, &mut dev, 10_000, 0, 50_000);
    assert_eq!(
        dev.cam_recovery_since, 8_000,
        "a straddling window keeps the anchor"
    );
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(dev.take_close_request(), "the stuck window after it fires");
}

#[test]
fn cam_recovery_ignores_a_window_that_starts_before_the_anchor() {
    let (clock, cam, mut dev) = cam_recovery_rig(5_000);
    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 10_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "stuck, but the window starts before the anchor"
    );
}

#[test]
fn cam_recovery_needs_a_clean_window_before_the_mmi_message() {
    // Nothing was descrambling (no entitlement, or a fresh zap): the MMI must not arm.
    let (clock, cam, mut dev) = cam_recovery_cold(20_000);
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, -1, "no clean window before the MMI");
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_close_request(), "never descrambled, so no reset");
}

#[test]
fn cam_recovery_resets_when_it_was_descrambling_before_the_mmi() {
    let (clock, cam, mut dev) = cam_recovery_cold(20_000);
    window_at(&clock, &mut dev, 5_000, 0, 50_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 5_000,
        "a clean window before the MMI arms"
    );
    window_at(&clock, &mut dev, 15_000, 50_000, 0);
    window_at(&clock, &mut dev, 25_000, 50_000, 0);
    assert!(!dev.take_close_request(), "the run has only 10 s");
    window_at(&clock, &mut dev, 35_000, 50_000, 0);
    assert!(
        dev.take_close_request(),
        "it was descrambling, then stopped"
    );
}

#[test]
fn cam_recovery_waits_for_a_clean_window_after_a_reset() {
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(dev.take_close_request(), "the first episode resets");
    assert!(!dev.cam_recovery_ok, "the guard holds until a clean window");

    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, -1,
        "no re-arm before a clean window"
    );
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_close_request());

    window_at(&clock, &mut dev, 50_000, 0, 50_000);
    assert!(dev.cam_recovery_ok, "a clean window re-enables it");
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 50_000,
        "armed again after the clean window"
    );
}

#[test]
fn cam_recovery_keeps_ten_minutes_between_resets() {
    use super::device::CAM_RECOVERY_MIN_GAP_MS;
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(dev.take_close_request(), "the first reset");

    // A second episode 30 s later is refused: less than the gap since the first reset.
    window_at(&clock, &mut dev, 30_000, 0, 50_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "within ten minutes of the last reset"
    );
    assert_eq!(dev.cam_recovery_since, -1, "the refused episode disarms");

    // The gap is measured from the first reset: exactly ten minutes later it is allowed.
    let at = 20_000 + CAM_RECOVERY_MIN_GAP_MS;
    window_at(&clock, &mut dev, at - 30_000, 0, 50_000);
    clock.advance(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, at - 10_000, 50_000, 0);
    assert!(!dev.take_close_request(), "not judged yet");
    window_at(&clock, &mut dev, at, 50_000, 0);
    assert!(dev.take_close_request(), "the gap has passed");
}

#[test]
fn cam_recovery_teardown_clears_the_anchor_and_keeps_the_guard() {
    let (_clock, cam, mut dev) = cam_recovery_rig(10_000);
    show_menu(&cam, &mut dev);
    dev.cam_recovery_last = 12_345;
    dev.teardown();
    assert_eq!(dev.cam_recovery_since, -1, "a teardown disarms");
    assert!(dev.cam_recovery_ok, "the guard survives a teardown");
    assert_eq!(
        dev.cam_recovery_last, 12_345,
        "the reset time survives a teardown"
    );
}

#[test]
fn cam_recovery_repeated_mmi_keeps_the_first_anchor() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    show_menu(&cam, &mut dev);
    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, 0, "the first message is the anchor");
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(!dev.take_close_request(), "the run has only 10 s");
    window_at(&clock, &mut dev, 30_000, 50_000, 0);
    assert!(dev.take_close_request(), "timed from the first message");
}

#[test]
fn cam_recovery_skips_an_enquiry_the_pin_answers() {
    let (_clock, cam, mut dev) = cam_recovery_rig(20_000);
    dev.set_pin(b"1234");
    assert!(cam.lock().unwrap().send_to_resource(
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N']
    ));
    dev.timeout();
    assert!(dev.service());
    assert_eq!(dev.cam_recovery_since, -1, "the configured PIN answers it");

    assert!(cam.lock().unwrap().send_to_resource(
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x00, 2, b'O', b'K']
    ));
    dev.timeout();
    assert!(dev.service());
    assert_eq!(
        dev.cam_recovery_since, 0,
        "a prompt the PIN does not answer arms"
    );
}

#[test]
fn cam_recovery_setting_reaches_every_slot() {
    let mut reg = CaRegistry::new(Arc::new(ManualClock::new(0, 0)));
    reg.set_cam_recovery_ms(15_000);
    assert_eq!(
        reg.device(0).cam_recovery_timeout_ms,
        15_000,
        "a slot allocated later"
    );
    let dev = CaDevice::new(2, Arc::new(ManualClock::new(0, 0)));
    reg.insert(dev);
    assert_eq!(
        reg.get(2).unwrap().cam_recovery_timeout_ms,
        15_000,
        "an inserted slot"
    );
    reg.set_cam_recovery_ms(0);
    assert_eq!(
        reg.device(0).cam_recovery_timeout_ms,
        0,
        "existing slots follow"
    );
}

#[test]
fn cam_recovery_refuses_one_millisecond_before_the_gap() {
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    dev.cam_recovery_last = 0;
    clock.advance(579_999);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 589_999, 50_000, 0);
    window_at(&clock, &mut dev, 599_999, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "599999 ms after the last reset is refused"
    );
    assert_eq!(dev.cam_recovery_since, -1, "the refused episode disarms");
}

// --- Zap guard: a CAPMT change (a zap) clears the arming state.

/// A slot that was clean at time 0, then saw a CAPMT change at `zap_at`.
fn zap_rig(zap_at: i64) -> (ManualClock, Arc<Mutex<FakeCam>>, CaDevice) {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    clock.advance(zap_at);
    dev.cam_recovery_pmts_changed();
    (clock, cam, dev)
}

#[test]
fn cam_recovery_zap_guard_blocks_an_mmi_after_a_zap() {
    // Clean service, zap to one that never descrambles, MMI in the same window.
    let (clock, cam, mut dev) = zap_rig(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, -1, "the zap leaves nothing to arm");
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "never descrambled after the zap: no reset"
    );
}

#[test]
fn cam_recovery_zap_guard_rearms_after_a_clean_window_past_the_zap() {
    let (clock, cam, mut dev) = zap_rig(5_000);
    // Starts at 10 000, past the zap plus the lag: clean, so it re-arms.
    window_at(&clock, &mut dev, 20_000, 0, 50_000);
    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 25_000,
        "a clean window after the zap arms"
    );
    window_at(&clock, &mut dev, 35_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "starts inside the lag: not judged"
    );
    window_at(&clock, &mut dev, 45_000, 50_000, 0);
    assert!(!dev.take_close_request(), "the run has only 10 s");
    window_at(&clock, &mut dev, 55_000, 50_000, 0);
    assert!(
        dev.take_close_request(),
        "stuck for the timeout after the MMI"
    );
}

#[test]
fn cam_recovery_zap_guard_ignores_a_clean_window_that_starts_before_the_zap() {
    // Starts at 2 000, inside the lag after the zap at 1 000: it must not re-arm.
    let (clock, cam, mut dev) = zap_rig(1_000);
    window_at(&clock, &mut dev, 12_000, 0, 50_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, -1,
        "a window inside the lag does not re-arm"
    );
}

#[test]
fn cam_recovery_zap_disarms_an_armed_anchor() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, 5_000, "armed before the zap");
    dev.cam_recovery_pmts_changed();
    assert_eq!(dev.cam_recovery_since, -1, "the zap drops the anchor");
    window_at(&clock, &mut dev, 25_000, 50_000, 0);
    window_at(&clock, &mut dev, 45_000, 50_000, 0);
    assert!(
        !dev.take_close_request(),
        "the stuck windows after the zap do not reset"
    );
}

#[test]
fn cam_recovery_without_the_zap_guard_resets_once_after_a_zap() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    dev.cam_recovery_zap_guard = false;
    clock.advance(5_000);
    dev.cam_recovery_pmts_changed();
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 5_000,
        "the MMI arms on the old clean window"
    );
    window_at(&clock, &mut dev, 25_000, 50_000, 0);
    assert!(!dev.take_close_request(), "the run has only 10 s");
    window_at(&clock, &mut dev, 35_000, 50_000, 0);
    assert!(dev.take_close_request(), "the zap case resets once");
    window_at(&clock, &mut dev, 45_000, 50_000, 0);
    assert!(!dev.take_close_request(), "one reset per episode");
}
