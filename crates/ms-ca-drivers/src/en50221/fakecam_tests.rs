//! The host stack driven by `ms-fakecam`, checked through its APDU log.

use super::clock::{Clock, ManualClock};
use super::consts::*;
use super::device::{CaDevice, CaState, HostEvent, MENU_IDLE_MS, MenuError, UnsolicitedMmi};
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
        solicited: false,
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
    assert!(events.contains(&HostEvent::MmiMenu {
        lines,
        solicited: false
    }));
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

/// An MMI menu body with `items` after title, subtitle and bottom (`choice_nb` = items - 1, as C++ reads it).
fn menu_body(texts: &[&str]) -> Vec<u8> {
    let mut body = vec![(texts.len() - 4) as u8];
    for text in texts {
        body.extend_from_slice(&[0x9F, 0x88, 0x03, text.len() as u8]);
        body.extend_from_slice(text.as_bytes());
    }
    body
}

/// The MMI APDUs the host sent, in order.
fn mmi_apdus(cam: &Arc<Mutex<FakeCam>>) -> Vec<(u32, Vec<u8>)> {
    host_apdus(&cam.lock().unwrap())
        .into_iter()
        .filter(|(tag, _)| {
            matches!(
                *tag,
                TAG_ENTER_MENU | TAG_MENU_ANSWER | TAG_CLOSE_MMI | TAG_ANSWER
            )
        })
        .collect()
}

#[test]
fn a_menu_request_walks_pages_without_auto_answers() {
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    drain_events(&mut dev);
    assert_eq!(
        dev.menu_answer(1),
        Err(MenuError::NotOpen),
        "no request yet"
    );
    assert!(dev.has_ai_session() && dev.has_mmi_session());
    dev.enter_menu().expect("AI session open");
    assert!(dev.menu_open());
    assert_eq!(mmi_apdus(&cam), [(TAG_ENTER_MENU, vec![])]);
    assert_eq!(dev.menu_answer(1), Err(MenuError::NotOpen), "no page yet");

    let texts = ["Main", "", "", "Status", "Info"];
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &menu_body(&texts),
    );
    let lines: Vec<String> = texts.iter().map(|s| (*s).to_owned()).collect();
    assert!(drain_events(&mut dev).contains(&HostEvent::MmiMenu {
        lines,
        solicited: true
    }));
    assert_eq!(mmi_apdus(&cam).len(), 1, "the page waits for the requester");
    assert_eq!(dev.menu_answer(3), Err(MenuError::BadChoice));
    dev.menu_answer(2).expect("item 2 is on the page");
    assert_eq!(mmi_apdus(&cam).last(), Some(&(TAG_MENU_ANSWER, vec![2])));
    assert_eq!(dev.menu_answer(1), Err(MenuError::NotOpen), "answered once");

    let list = ["Card", "", "", "Line"];
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_LIST_LAST,
        &menu_body(&list),
    );
    assert_eq!(
        dev.menu_answer(1),
        Err(MenuError::BadChoice),
        "a list takes 0"
    );
    dev.menu_answer(0).expect("back from the list");
    dev.close_mmi().expect("MMI session open");
    assert!(!dev.menu_open());
    assert_eq!(
        mmi_apdus(&cam)[2..],
        [
            (TAG_MENU_ANSWER, vec![0]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ]
    );
    #[cfg(feature = "diag-log")]
    {
        let events = crate::test_log::events("cam_menu");
        assert!(
            events.iter().any(|e| e.contains("\"kind\":\"list\"")
                && e.contains("\"answer\":\"request\"")
                && e.contains("\"solicited\":true")),
            "{events:?}"
        );
    }
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
        !dev.take_cam_recovery(),
        "a window that starts at the anchor is not judged"
    );
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(
        !dev.take_cam_recovery(),
        "the first judged stuck window starts a run"
    );
    window_at(&clock, &mut dev, 30_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "stuck for the timeout");
    assert_eq!(dev.cam_recovery_since, -1, "the recovery disarms");
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "one recovery per episode");
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
        !dev.take_cam_recovery(),
        "neutral windows do not count as stuck"
    );
    // The stuck window ends 50 s after the anchor, but it is the only stuck window.
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(
        !dev.take_cam_recovery(),
        "a single stuck window is not a sustained run"
    );
    assert_eq!(dev.cam_recovery_stuck_since, 40_000, "the run starts here");
    window_at(&clock, &mut dev, 60_000, 50_000, 0);
    assert!(
        dev.take_cam_recovery(),
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
        !dev.take_cam_recovery(),
        "the new run is only 10 s long at 40 s"
    );
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the new run spans the timeout");
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
    assert!(!dev.take_cam_recovery(), "only 10 s of stuck windows");
    window_at(&clock, &mut dev, 60_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the new run spans the timeout");
}

#[test]
fn cam_recovery_needs_an_mmi_message() {
    let (clock, _cam, mut dev) = cam_recovery_rig(20_000);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "no MMI message, no recovery");
}

#[test]
fn cam_recovery_is_off_by_default() {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    assert_eq!(dev.cam_recovery_timeout_ms, 0, "the knob defaults to off");
    show_menu(&cam, &mut dev);
    assert_eq!(dev.cam_recovery_since, -1, "off means never armed");
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_cam_recovery());
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
        !dev.take_cam_recovery(),
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
            dev.take_cam_recovery(),
            fires,
            "scrambled {scrambled}, clear {clear}"
        );
    }
}

#[test]
fn an_idle_menu_request_is_backed_out_and_closed() {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    dev.enter_menu().expect("AI session open");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &menu_body(&["Main", "", "", "A", "B"]),
    );
    clock.advance(MENU_IDLE_MS - 1);
    dev.timeout();
    assert!(dev.menu_open(), "not idle yet");
    clock.advance(1);
    dev.timeout();
    assert!(!dev.menu_open());
    assert_eq!(
        mmi_apdus(&cam)[1..],
        [
            (TAG_MENU_ANSWER, vec![0]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ]
    );
}

#[test]
fn cam_recovery_window_must_start_after_the_lag() {
    let (clock, cam, mut dev) = cam_recovery_rig(1);
    show_menu(&cam, &mut dev);
    // Starts at 1999 ms: inside the stats lag, not judged.
    window_at(&clock, &mut dev, 11_999, 50_000, 0);
    assert!(
        !dev.take_cam_recovery(),
        "a window inside the lag is not judged"
    );
    // Starts at exactly the lag: judged.
    window_at(&clock, &mut dev, 12_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "a window at the lag is judged");
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
    assert!(dev.take_cam_recovery(), "the stuck window after it fires");
}

#[test]
fn cam_recovery_ignores_a_window_that_starts_before_the_anchor() {
    let (clock, cam, mut dev) = cam_recovery_rig(5_000);
    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 10_000, 50_000, 0);
    assert!(
        !dev.take_cam_recovery(),
        "stuck, but the window starts before the anchor"
    );
}

/// Advances the clock to `at` and judges one service's 10 s window.
fn service_window_at(
    clock: &ManualClock,
    dev: &mut CaDevice,
    sid: u16,
    at: i64,
    scrambled: u64,
    clear: u64,
) {
    clock.advance(at - clock.tick_ms());
    dev.cam_recovery_service_window(sid, WINDOW_MS, scrambled, clear);
}

#[test]
fn silent_recovery_fires_once_after_a_30_s_run() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x100;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 10_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "a window inside the join lag is ignored"
    );
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 30_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "N = 10 s still needs the 30 s run"
    );
    service_window_at(&clock, &mut dev, sid, 32_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);
    service_window_at(&clock, &mut dev, sid, 40_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "one silent re-send per join"
    );
}

#[test]
fn silent_recovery_window_must_start_after_the_join_lag() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x101;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 10_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 31_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the run starts after the lag, not at the join"
    );
    service_window_at(&clock, &mut dev, sid, 51_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);
}

#[test]
fn silent_recovery_clean_window_restarts_the_run() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x102;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 20_000, 0, 50_000);
    service_window_at(&clock, &mut dev, sid, 32_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the clean window ended the first run"
    );
    service_window_at(&clock, &mut dev, sid, 52_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);
}

#[test]
fn silent_recovery_needs_a_new_join_for_another_re_send() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x103;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);

    // The same join does not get a second re-send.
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 72_000, 50_000, 0);
    assert!(dev.take_silent_recovery().is_empty());

    // A new CAPMT pmt id for the SID is a new join and gets one more.
    dev.cam_recovery_sync_services(&[(8, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 82_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 114_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the run restarts at the new join"
    );
    service_window_at(&clock, &mut dev, sid, 134_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![8]);
}

#[test]
fn silent_recovery_inactive_sync_restarts_the_join_time() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x104;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    clock.advance(100_000);
    dev.cam_recovery_sync_services(&[(7, sid, 1)], false);
    service_window_at(&clock, &mut dev, sid, 110_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 132_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "windows before the new join plus lag are ignored"
    );
    service_window_at(&clock, &mut dev, sid, 152_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);
}

#[test]
fn silent_recovery_ignores_the_mmi_gap() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x105;
    dev.cam_recovery_last = 0;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 32_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![7]);
    assert_eq!(
        dev.cam_recovery_last, 0,
        "the silent path does not touch the MMI gap"
    );
    assert!(
        !dev.take_cam_recovery(),
        "the silent path does not use the MMI due"
    );
}

#[test]
fn silent_recovery_is_off_with_the_knob() {
    let (clock, _cam, mut dev) = cam_recovery_cold(0);
    let sid = 0x106;
    dev.cam_recovery_sync_services(&[(7, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 62_000, 50_000, 0);
    assert!(dev.take_silent_recovery().is_empty());
}

#[test]
fn silent_recovery_warns_once_after_the_first_re_send() {
    let _ = crate::test_log::captured_lines();
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x107;
    let pmt = 0x7A07;
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![pmt]);
    for at in [52_000, 62_000, 72_000, 82_000, 92_000] {
        service_window_at(&clock, &mut dev, sid, at, 50_000, 0);
        // A different line between the windows keeps repeat suppression from
        // hiding a second warning.
        ms_log::log!("silent_recovery_warns marker {}", at);
    }
    assert!(
        dev.take_silent_recovery().is_empty(),
        "one silent re-send per join"
    );
    let needle =
        format!("CA 0: PMT {pmt} ES packets still scrambled; its CAPMT was already re-sent once");
    let count = crate::test_log::captured_lines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|line| line.contains(&needle))
        .count();
    assert_eq!(count, 1, "the already re-sent line is logged exactly once");
}

#[test]
fn silent_recovery_allows_another_re_send_after_the_sid_leaves() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x108;
    let pmt = 0x7A08;
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![pmt]);

    dev.cam_recovery_sync_services(&[], true);
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 50_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 60_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 72_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the new join needs its own 30 s run"
    );
    service_window_at(&clock, &mut dev, sid, 84_000, 50_000, 0);
    assert_eq!(
        dev.take_silent_recovery(),
        vec![pmt],
        "the re-added SID gets one more re-send"
    );
}

#[test]
fn silent_recovery_allows_another_re_send_after_a_new_generation() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x10C;
    let pmt = 0x7A0D;
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![pmt]);

    // A delete and re-add keeps the SID and PMT id but gets a new generation.
    dev.cam_recovery_sync_services(&[(pmt, sid, 2)], true);
    service_window_at(&clock, &mut dev, sid, 50_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 60_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 72_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the new join needs its own 30 s run"
    );
    service_window_at(&clock, &mut dev, sid, 84_000, 50_000, 0);
    assert_eq!(
        dev.take_silent_recovery(),
        vec![pmt],
        "the new generation gets one more re-send"
    );
}

#[test]
fn silent_recovery_inactive_sync_allows_another_re_send() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x109;
    let pmt = 0x7A09;
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![pmt]);

    service_window_at(&clock, &mut dev, sid, 52_000, 50_000, 0);
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], false);
    service_window_at(&clock, &mut dev, sid, 60_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 72_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the stuck run restarts at the inactive sync"
    );
    service_window_at(&clock, &mut dev, sid, 84_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the new run is still too short"
    );
    service_window_at(&clock, &mut dev, sid, 96_000, 50_000, 0);
    assert_eq!(
        dev.take_silent_recovery(),
        vec![pmt],
        "a CAM re-init counts as a new join"
    );
}

#[test]
fn silent_recovery_skips_a_sid_on_more_than_one_pmt() {
    let _ = crate::test_log::captured_lines();
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x10A;
    let pmts = [(0x7A0A, sid, 1), (0x7A0B, sid, 1)];
    dev.cam_recovery_sync_services(&pmts, true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 42_000, 50_000, 0);
    service_window_at(&clock, &mut dev, sid, 72_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "a duplicated SID gets no silent recovery"
    );
    dev.cam_recovery_sync_services(&pmts, true);
    ms_log::log!("silent_recovery_duplicate marker 1");
    dev.cam_recovery_sync_services(&pmts, true);
    ms_log::log!("silent_recovery_duplicate marker 2");
    let needle = format!("CA 0: SID {sid} is on more than one PMT, no silent recovery for it");
    let count = crate::test_log::captured_lines()
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .filter(|line| line.contains(&needle))
        .count();
    assert_eq!(count, 1, "logged once while the SID stays duplicated");
}

#[test]
fn silent_recovery_timeout_zero_clears_the_old_run() {
    let (clock, _cam, mut dev) = cam_recovery_cold(10_000);
    let sid = 0x10B;
    let pmt = 0x7A0C;
    dev.cam_recovery_sync_services(&[(pmt, sid, 1)], true);
    service_window_at(&clock, &mut dev, sid, 12_000, 50_000, 0);

    dev.cam_recovery_timeout_ms = 0;
    service_window_at(&clock, &mut dev, sid, 20_000, 50_000, 0);
    dev.cam_recovery_timeout_ms = 10_000;
    service_window_at(&clock, &mut dev, sid, 32_000, 50_000, 0);
    assert!(
        dev.take_silent_recovery().is_empty(),
        "the old run was cleared with the knob off"
    );
    service_window_at(&clock, &mut dev, sid, 52_000, 50_000, 0);
    assert_eq!(dev.take_silent_recovery(), vec![pmt]);
}

#[test]
fn a_requested_enquiry_is_cancelled_without_a_fitting_pin() {
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    dev.enter_menu().expect("AI session open");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N'],
    );
    assert!(dev.menu_open(), "the request stays open");
    assert_eq!(
        mmi_apdus(&cam)[1..],
        [(TAG_ANSWER, vec![MMI_ANSW_ID_CANCEL])],
        "cancelled, not closed"
    );
}

#[test]
fn unsolicited_menus_follow_the_configured_reply() {
    let body = menu_body(&["E", "", "", "One", "Two"]);
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    dev.unsolicited_mmi = UnsolicitedMmi::Answer0;
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &body,
    );
    assert_eq!(
        mmi_apdus(&cam),
        [
            (TAG_MENU_ANSWER, vec![0]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ]
    );

    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    dev.unsolicited_mmi = UnsolicitedMmi::Open;
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &body,
    );
    assert!(mmi_apdus(&cam).is_empty(), "left open");
    assert!(!dev.menu_open(), "a held page is not a request");
    clock.advance(MENU_IDLE_MS / 2);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &body,
    );
    clock.advance(MENU_IDLE_MS / 2);
    dev.timeout();
    assert_eq!(
        mmi_apdus(&cam),
        [
            (TAG_MENU_ANSWER, vec![0]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ],
        "a repeated page does not extend the hold"
    );
}

#[test]
fn the_apdu_sink_sees_headers_only() {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    let log = Arc::clone(&seen);
    dev.apdu_sink = Some(Arc::new(move |slot, dir, session, tag, len| {
        log.lock().unwrap().push((slot, dir, session, tag, len));
    }));
    let body = menu_body(&["E", "", "", "One", "Two"]);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &body,
    );
    let seen = seen.lock().unwrap();
    assert!(seen.iter().any(|e| e.0 == 0
        && e.1 == super::device::ApduDir::CamToHost
        && e.3 == TAG_MENU_LAST
        && e.4 == body.len()));
    assert!(
        seen.iter().any(|e| e.1 == super::device::ApduDir::HostToCam
            && e.3 == TAG_MENU_ANSWER
            && e.4 == 1)
    );
}

#[test]
fn the_registry_hands_the_apdu_sink_to_later_slots() {
    let mut registry = CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    registry.set_apdu_sink(Some(Arc::new(|_, _, _, _, _| {})));
    assert!(registry.device(4).apdu_sink.is_some());
    registry.set_apdu_sink(None);
    assert!(registry.device(4).apdu_sink.is_none());
}

#[test]
fn menu_commands_need_their_sessions() {
    let clock = ManualClock::new(0, UNIX);
    let mut dev = CaDevice::new(0, Arc::new(clock));
    assert_eq!(dev.enter_menu(), Err(MenuError::NoSession));
    assert_eq!(dev.close_mmi(), Err(MenuError::NoSession));
    assert!(!dev.menu_open());
}

#[test]
fn a_cam_side_close_ends_the_request() {
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    dev.enter_menu().expect("AI session open");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_CLOSE_MMI,
        &[0],
    );
    assert!(!dev.menu_open(), "close_mmi from the CAM");
    let body = menu_body(&["E", "", "", "One", "Two"]);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &body,
    );
    assert_eq!(
        mmi_apdus(&cam)[1..],
        [
            (TAG_MENU_ANSWER, vec![1]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ],
        "the next page is unsolicited again"
    );

    dev.enter_menu().expect("AI session open");
    assert!(
        cam.lock()
            .unwrap()
            .request_close(EN50221_APP_MMI_RESOURCEID)
    );
    dev.timeout();
    assert!(dev.service());
    assert!(!dev.menu_open(), "the MMI session closed");
}

#[test]
fn a_requested_pin_enquiry_is_answered_without_a_close() {
    let (_, cam, mut dev) = setup(CamConfig::default(), true);
    dev.set_pin(b"1234");
    dev.enter_menu().expect("AI session open");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N'],
    );
    assert!(dev.menu_open());
    assert_eq!(
        mmi_apdus(&cam)[1..],
        [(TAG_ANSWER, vec![MMI_ANSW_ID_ANSWER, b'1', b'2', b'3', b'4'])]
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
    assert!(
        !dev.take_cam_recovery(),
        "never descrambled, so no recovery"
    );
}

#[test]
fn cam_recovery_fires_when_it_was_descrambling_before_the_mmi() {
    let (clock, cam, mut dev) = cam_recovery_cold(20_000);
    window_at(&clock, &mut dev, 5_000, 0, 50_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 5_000,
        "a clean window before the MMI arms"
    );
    window_at(&clock, &mut dev, 15_000, 50_000, 0);
    window_at(&clock, &mut dev, 25_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "the run has only 10 s");
    window_at(&clock, &mut dev, 35_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "it was descrambling, then stopped");
}

#[test]
fn cam_recovery_waits_for_a_clean_window_after_a_recovery() {
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the first episode recovers");
    assert!(!dev.cam_recovery_ok, "the guard holds until a clean window");

    clock.advance(5_000);
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, -1,
        "no re-arm before a clean window"
    );
    window_at(&clock, &mut dev, 40_000, 50_000, 0);
    assert!(!dev.take_cam_recovery());

    window_at(&clock, &mut dev, 50_000, 0, 50_000);
    assert!(dev.cam_recovery_ok, "a clean window re-enables it");
    show_menu(&cam, &mut dev);
    assert_eq!(
        dev.cam_recovery_since, 50_000,
        "armed again after the clean window"
    );
}

#[test]
fn cam_recovery_keeps_ten_minutes_between_recoveries() {
    use super::device::CAM_RECOVERY_MIN_GAP_MS;
    let (clock, cam, mut dev) = cam_recovery_rig(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 20_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the first recovery");

    // A second episode 30 s later is refused: less than the gap since the first recovery.
    window_at(&clock, &mut dev, 30_000, 0, 50_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, 50_000, 50_000, 0);
    assert!(
        !dev.take_cam_recovery(),
        "within ten minutes of the last recovery"
    );
    assert_eq!(dev.cam_recovery_since, -1, "the refused episode disarms");

    // The gap is measured from the first recovery: exactly ten minutes later it is allowed.
    let at = 20_000 + CAM_RECOVERY_MIN_GAP_MS;
    window_at(&clock, &mut dev, at - 30_000, 0, 50_000);
    clock.advance(10_000);
    show_menu(&cam, &mut dev);
    window_at(&clock, &mut dev, at - 10_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "not judged yet");
    window_at(&clock, &mut dev, at, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the gap has passed");
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
        "the recovery time survives a teardown"
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
    assert!(!dev.take_cam_recovery(), "the run has only 10 s");
    window_at(&clock, &mut dev, 30_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "timed from the first message");
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
        !dev.take_cam_recovery(),
        "599999 ms after the last recovery is refused"
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
        !dev.take_cam_recovery(),
        "never descrambled after the zap: no recovery"
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
        !dev.take_cam_recovery(),
        "starts inside the lag: not judged"
    );
    window_at(&clock, &mut dev, 45_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "the run has only 10 s");
    window_at(&clock, &mut dev, 55_000, 50_000, 0);
    assert!(
        dev.take_cam_recovery(),
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
        !dev.take_cam_recovery(),
        "the stuck windows after the zap do not recover"
    );
}

#[test]
fn cam_recovery_without_the_zap_guard_fires_once_after_a_zap() {
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
    assert!(!dev.take_cam_recovery(), "the run has only 10 s");
    window_at(&clock, &mut dev, 35_000, 50_000, 0);
    assert!(dev.take_cam_recovery(), "the zap case recovers once");
    window_at(&clock, &mut dev, 45_000, 50_000, 0);
    assert!(!dev.take_cam_recovery(), "one recovery per episode");
}

// --- --cam-recovery with --cam-menu: a requested page is no CAM error.

#[test]
fn cam_recovery_ignores_requested_menus_and_enquiries() {
    let (_clock, cam, mut dev) = cam_recovery_rig(20_000);
    dev.enter_menu().expect("AI session open");
    let page = menu_body(&["Main", "", "", "Status", "Info"]);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &page,
    );
    assert_eq!(dev.cam_recovery_since, -1, "a requested menu does not arm");
    // No PIN fits this prompt: cancelled inside the request, still not armed.
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x00, 4, b'P', b'I', b'N'],
    );
    assert_eq!(
        dev.cam_recovery_since, -1,
        "a requested enquiry does not arm"
    );

    dev.close_mmi().expect("MMI session open");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_MENU_LAST,
        &page,
    );
    assert!(
        dev.cam_recovery_since >= 0,
        "an unsolicited menu arms again"
    );
}

// --- --cam-menu-snapshot: hash-pinned menu walks after a CAM page of its own.

const MAIN: [&str; 5] = ["Main", "", "", "Status", "Other"];
const SUB: [&str; 5] = ["Status", "", "", "Card", "Service"];
const TARGET: [&str; 4] = ["Card", "", "", "Line"];

fn page_hash(texts: &[&str]) -> u64 {
    super::apps::text_hash(texts.iter().copied())
}

/// A slot with the paths `MAIN:1/SUB:choice` for each of `choices`.
fn snapshot_rig(choices: &[u8]) -> (ManualClock, Arc<Mutex<FakeCam>>, CaDevice) {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    dev.snapshot_paths = choices
        .iter()
        .map(|&c| vec![(page_hash(&MAIN), 1), (page_hash(&SUB), c)])
        .collect();
    (clock, cam, dev)
}

fn show(dev: &mut CaDevice, cam: &Arc<Mutex<FakeCam>>, tag: u32, texts: &[&str]) {
    deliver(dev, cam, EN50221_APP_MMI_RESOURCEID, tag, &menu_body(texts));
}

/// The CAM's own error page, answered as today, then the 5 s wait.
fn error_then_start(clock: &ManualClock, dev: &mut CaDevice, cam: &Arc<Mutex<FakeCam>>) {
    show(dev, cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    clock.advance(5_000);
    dev.timeout();
}

#[test]
fn a_snapshot_walks_its_pinned_path_and_closes_on_the_target() {
    let (clock, cam, mut dev) = snapshot_rig(&[1]);
    error_then_start(&clock, &mut dev, &cam);
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    show(&mut dev, &cam, TAG_MENU_LAST, &SUB);
    show(&mut dev, &cam, TAG_LIST_LAST, &TARGET);
    clock.advance(4_000);
    dev.timeout();
    let close = (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
    assert_eq!(
        mmi_apdus(&cam),
        [
            (TAG_MENU_ANSWER, vec![1]),
            close.clone(),
            (TAG_ENTER_MENU, vec![]),
            (TAG_MENU_ANSWER, vec![1]),
            (TAG_MENU_ANSWER, vec![1]),
            close
        ]
    );
    assert!(
        !dev.menu_open() && dev.snapshot.is_none(),
        "done after the last path"
    );
}

#[test]
fn a_page_that_is_not_the_pinned_one_ends_the_path_unanswered() {
    // A repeat of the parent page (same count, other step) and a changed menu both stop it.
    for second in [&MAIN, &["Status", "", "", "Card", "Reset"]] {
        let (clock, cam, mut dev) = snapshot_rig(&[1]);
        error_then_start(&clock, &mut dev, &cam);
        show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
        show(&mut dev, &cam, TAG_MENU_LAST, second);
        assert_eq!(
            mmi_apdus(&cam)[2..],
            [
                (TAG_ENTER_MENU, vec![]),
                (TAG_MENU_ANSWER, vec![1]),
                (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
            ],
            "{second:?}"
        );
    }
}

#[test]
fn a_snapshot_never_sends_the_pin() {
    let (clock, cam, mut dev) = snapshot_rig(&[1]);
    dev.set_pin(b"1234");
    error_then_start(&clock, &mut dev, &cam);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N'],
    );
    assert_eq!(
        mmi_apdus(&cam)[3..],
        [
            (TAG_ANSWER, vec![MMI_ANSW_ID_CANCEL]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ]
    );
}

#[test]
fn between_paths_the_cam_s_own_pages_keep_their_pin_and_arm_recovery() {
    let (clock, cam, mut dev) = cam_recovery_rig(20_000);
    dev.snapshot_paths = vec![vec![(page_hash(&MAIN), 1)], vec![(page_hash(&MAIN), 2)]];
    dev.set_pin(b"1234");
    error_then_start(&clock, &mut dev, &cam);
    dev.cam_recovery_since = -1;
    dev.cam_recovery_ok = true;
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    show(&mut dev, &cam, TAG_LIST_LAST, &TARGET);
    let sent = mmi_apdus(&cam).len();
    // In the gap: a menu of the CAM's own is closed with no choice, and still arms recovery.
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    assert_eq!(
        mmi_apdus(&cam)[sent..],
        [(TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])]
    );
    assert!(dev.cam_recovery_since >= 0, "recovery armed");
    // A PIN prompt of its own still gets the PIN.
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N'],
    );
    assert_eq!(
        mmi_apdus(&cam)[sent + 1],
        (TAG_ANSWER, vec![MMI_ANSW_ID_ANSWER, b'1', b'2', b'3', b'4'])
    );
}

#[test]
fn while_waiting_the_cam_s_pages_keep_today_s_handling() {
    let (_clock, cam, mut dev) = snapshot_rig(&[1]);
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    let close = (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
    assert_eq!(
        mmi_apdus(&cam),
        [
            (TAG_MENU_ANSWER, vec![1]),
            close.clone(),
            (TAG_MENU_ANSWER, vec![1]),
            close
        ]
    );
}

#[test]
fn a_held_page_or_a_busy_mmi_delays_the_start_until_the_deadline() {
    let (clock, cam, mut dev) = snapshot_rig(&[1]);
    dev.unsolicited_mmi = UnsolicitedMmi::Open;
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    clock.advance(5_000);
    dev.timeout();
    assert!(mmi_apdus(&cam).is_empty(), "the held page is kept");
    // Keep the MMI busy past the 5 min start limit.
    for _ in 0..30 {
        clock.advance(10_000);
        dev.mmi_hold_ms = clock.tick_ms();
        dev.timeout();
    }
    assert!(dev.snapshot.is_none(), "dropped after 5 min");
    assert!(!mmi_apdus(&cam).contains(&(TAG_ENTER_MENU, vec![])));
}

#[test]
fn a_cam_close_or_a_page_timeout_moves_to_the_next_path() {
    let (clock, cam, mut dev) = snapshot_rig(&[1, 2]);
    error_then_start(&clock, &mut dev, &cam);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_CLOSE_MMI,
        &[0],
    );
    assert!(!dev.menu_open(), "the CAM closed the first path");
    clock.advance(4_000);
    dev.timeout();
    assert_eq!(
        mmi_apdus(&cam).last(),
        Some(&(TAG_ENTER_MENU, vec![])),
        "path 2"
    );
    clock.advance(10_000);
    dev.timeout();
    assert_eq!(
        mmi_apdus(&cam).last(),
        Some(&(TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])),
        "no page for 10 s"
    );
    clock.advance(4_000);
    dev.timeout();
    assert!(dev.snapshot.is_none());
}

#[test]
fn a_menu_request_cancels_the_snapshot_and_the_window_stays_used() {
    let (clock, cam, mut dev) = snapshot_rig(&[1]);
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    dev.enter_menu().expect("AI session open");
    assert!(dev.snapshot.is_none(), "the request took over");
    dev.close_mmi().expect("MMI session open");
    clock.advance(super::snapshot::SNAPSHOT_MIN_GAP_MS - 1);
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    assert!(dev.snapshot.is_none(), "within 10 min of the cancelled one");
    clock.advance(1);
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    assert!(dev.snapshot.is_some(), "10 min later");
}

#[test]
fn a_requested_page_never_starts_a_snapshot() {
    let (_clock, cam, mut dev) = snapshot_rig(&[1]);
    dev.enter_menu().expect("AI session open");
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    assert!(dev.snapshot.is_none());
}

#[test]
fn the_cam_s_late_close_of_a_path_lands_in_the_gap() {
    let (clock, cam, mut dev) = snapshot_rig(&[1, 2]);
    error_then_start(&clock, &mut dev, &cam);
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    show(&mut dev, &cam, TAG_MENU_LAST, &SUB);
    show(&mut dev, &cam, TAG_LIST_LAST, &TARGET);
    clock.advance(2_000);
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_CLOSE_MMI,
        &[0],
    );
    clock.advance(2_000);
    dev.timeout();
    assert_eq!(
        mmi_apdus(&cam).last(),
        Some(&(TAG_ENTER_MENU, vec![])),
        "path 2 opens"
    );
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    assert_eq!(
        mmi_apdus(&cam).last(),
        Some(&(TAG_MENU_ANSWER, vec![1])),
        "path 2 is walked, not cut by the late close"
    );
}

#[test]
fn a_teardown_or_a_closed_mmi_session_ends_the_walk() {
    let (clock, cam, mut dev) = snapshot_rig(&[1, 2]);
    error_then_start(&clock, &mut dev, &cam);
    assert!(
        cam.lock()
            .unwrap()
            .request_close(EN50221_APP_MMI_RESOURCEID)
    );
    dev.timeout();
    assert!(dev.service());
    assert!(
        !dev.menu_open() && !dev.snapshot_walking(),
        "the session close ends path 1"
    );
    assert!(dev.snapshot.is_some(), "path 2 is still due");
    let last = dev.snapshot_last;
    dev.teardown();
    assert!(dev.snapshot.is_none(), "a teardown cancels it");
    assert_eq!(dev.snapshot_last, last, "the 10 min window stays used");
}

#[test]
fn an_enquiry_the_pin_answers_starts_no_snapshot() {
    let (_clock, cam, mut dev) = snapshot_rig(&[1]);
    dev.set_pin(b"1234");
    deliver(
        &mut dev,
        &cam,
        EN50221_APP_MMI_RESOURCEID,
        TAG_ENQUIRY,
        &[0x01, 4, b'P', b'I', b'N'],
    );
    assert!(dev.snapshot.is_none());
}

#[test]
fn a_target_page_that_is_a_menu_is_closed_not_answered() {
    let (clock, cam, mut dev) = setup(CamConfig::default(), true);
    dev.snapshot_paths = vec![vec![(page_hash(&MAIN), 1)]];
    error_then_start(&clock, &mut dev, &cam);
    show(&mut dev, &cam, TAG_MENU_LAST, &MAIN);
    show(&mut dev, &cam, TAG_MENU_LAST, &SUB);
    assert_eq!(
        mmi_apdus(&cam)[3..],
        [
            (TAG_MENU_ANSWER, vec![1]),
            (TAG_CLOSE_MMI, vec![MMI_CLOSE_MMI_CMD_ID_IMMEDIATE])
        ]
    );
}

#[test]
fn the_registry_hands_the_snapshot_paths_to_later_slots() {
    let mut registry = CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    registry.set_menu_snapshot(vec![vec![(1, 1)]]);
    assert_eq!(registry.device(5).snapshot_paths, [vec![(1, 1)]]);
}

#[test]
fn closing_a_held_page_keeps_a_waiting_snapshot() {
    let (_clock, cam, mut dev) = snapshot_rig(&[1]);
    dev.unsolicited_mmi = UnsolicitedMmi::Open;
    show(&mut dev, &cam, TAG_MENU_LAST, &["E", "", "", "OK"]);
    dev.close_mmi().expect("MMI session open");
    assert!(dev.snapshot.is_some(), "not a takeover");
    assert_eq!(dev.menu_answer(1), Err(MenuError::NotOpen));
    assert!(
        dev.snapshot.is_some(),
        "a refused answer does not cancel it"
    );
}
