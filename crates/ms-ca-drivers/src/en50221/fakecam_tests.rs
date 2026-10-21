//! The host stack driven by `ms-fakecam`, checked through its APDU log.

use super::clock::ManualClock;
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
}

impl CaOpener for CamOpener {
    fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
        Ok(Box::new(CamLink {
            cam: Arc::clone(&self.cam),
            rx: VecDeque::new(),
        }))
    }

    fn has_ciplus_root_cert(&self) -> bool {
        false
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
