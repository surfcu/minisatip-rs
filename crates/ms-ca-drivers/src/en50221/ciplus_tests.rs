//! CI+ content control driven by `ms-fakecam`'s module-side state machine.

use super::ciplus::authdata::RECORD_LEN;
use super::ciplus::cc::{CcAction, CcState};
use super::ciplus::test_pki::*;
use super::clock::ManualClock;
use super::consts::*;
use super::device::{CaDevice, CcKeyBytes, HostEvent};
use super::link::{
    AdapterInfo, CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY, CaLink, CaOpener, SlotInfo,
};
use super::registry::authdata_filename;
use ms_fakecam::ciplus::CiPlusCam;
use ms_fakecam::ciplus::test_pki::{TestPki, pki_with_device};
use ms_fakecam::record::{Direction, Event};
use ms_fakecam::tags::{APPLICATION_INFO, CIPLUS_CC, RESOURCE_MANAGER};
use ms_fakecam::{CamConfig, FakeCam};
use std::collections::VecDeque;
use std::io;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

const UNIX: u64 = 1_700_000_000;

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

/// Opens the fake CAM from an explicit certificate directory.
struct CertOpener {
    cam: Arc<Mutex<FakeCam>>,
    root_cert: bool,
    cert_dir: String,
}

impl CaOpener for CertOpener {
    fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
        Ok(Box::new(CamLink {
            cam: Arc::clone(&self.cam),
            rx: VecDeque::new(),
        }))
    }

    fn ciplus_cert_dir(&self) -> String {
        self.cert_dir.clone()
    }

    fn has_ciplus_root_cert(&self) -> bool {
        self.root_cert
    }
}

struct Rig {
    cam: Arc<Mutex<FakeCam>>,
    dev: CaDevice,
    cache_dir: PathBuf,
    /// Keeps the host certificates and the authdata directory alive.
    _pki: TestPki,
}

fn setup(
    order: [u8; 4],
    cc_resource: u32,
    with_device_key: bool,
    tune: impl FnOnce(&mut CiPlusCam),
) -> Rig {
    let pki = pki_with_device("ciplus-tests", with_device_key);
    let mut ciplus = CiPlusCam {
        element_order: order,
        ..pki.cam.clone()
    };
    tune(&mut ciplus);
    let cache_dir = pki.cache_dir.clone();
    let config = CamConfig {
        resources: vec![RESOURCE_MANAGER, APPLICATION_INFO, cc_resource],
        ciplus: Some(ciplus),
        ..CamConfig::default()
    };
    let cam = Arc::new(Mutex::new(FakeCam::new(
        config,
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let mut dev = CaDevice::new(0, Arc::new(ManualClock::new(0, UNIX)));
    dev.force_ci = false;
    dev.enabled = true;
    dev.set_ciplus_paths(path_arg(&pki.cert_dir), path_arg(&cache_dir));
    attach(&mut dev, &cam);
    assert!(dev.init_en50221(), "the fake slot resets ready");
    assert!(dev.service(), "the link stays up");
    Rig {
        cam,
        dev,
        cache_dir,
        _pki: pki,
    }
}

fn attach(dev: &mut CaDevice, cam: &Arc<Mutex<FakeCam>>) {
    dev.set_link(Some(Box::new(CamLink {
        cam: Arc::clone(cam),
        rx: VecDeque::new(),
    })));
}

fn pump(dev: &mut CaDevice) {
    dev.timeout();
    assert!(dev.service(), "the link stays up");
}

fn drain_events(dev: &mut CaDevice) -> Vec<HostEvent> {
    std::iter::from_fn(|| dev.next_event()).collect()
}

fn host_apdu_count(cam: &Arc<Mutex<FakeCam>>) -> usize {
    cam.lock()
        .unwrap()
        .recorder()
        .apdus()
        .into_iter()
        .filter(|r| r.dir == Direction::HostToCam && matches!(r.event, Event::Apdu { .. }))
        .count()
}

fn cckeys(events: &[HostEvent]) -> Vec<(u8, [u8; 16], [u8; 16])> {
    events
        .iter()
        .filter_map(|event| match event {
            HostEvent::CcKey { parity, key, iv } => Some((*parity, key.0, iv.0)),
            _ => None,
        })
        .collect()
}

/// Log lines captured for the whole unit-test binary.
fn captured_lines() -> &'static Arc<Mutex<Vec<String>>> {
    static LINES: OnceLock<Arc<Mutex<Vec<String>>>> = OnceLock::new();
    LINES.get_or_init(|| {
        let lines = Arc::new(Mutex::new(Vec::new()));
        ms_log::init(ms_log::Config {
            log: ms_log::ALL_LOG_MODULES | ms_log::LOG_GENERAL,
            debug: 0,
            slog: 0,
            file_line: false,
            backend: Box::new(Capture {
                lines: Arc::clone(&lines),
            }),
        });
        lines
    })
}

struct Capture {
    lines: Arc<Mutex<Vec<String>>>,
}

impl ms_log::Backend for Capture {
    fn timestamp(&self) -> ms_log::Timestamp {
        ms_log::Timestamp::new(1, 1, 0, 0, 0, 0)
    }

    fn syslog(&self, _: &str) {}

    fn stdout(&self, line: &str) {
        self.lines.lock().unwrap().push(line.to_owned());
    }
}

fn authdata_path(rig: &Rig) -> PathBuf {
    PathBuf::from(authdata_filename(path_arg(&rig.cache_dir), 0, b"Fake CAM"))
}

#[test]
fn full_handshake_delivers_key_and_authdata() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |_| {});
    let events = drain_events(&mut rig.dev);
    let disable = events
        .iter()
        .position(|event| *event == HostEvent::CcDisableCws);
    let key = events
        .iter()
        .position(|event| matches!(event, HostEvent::CcKey { .. }));
    assert!(
        matches!((disable, key), (Some(disable), Some(key)) if disable < key),
        "the SAC data request disables descrambling before the key: {events:?}"
    );
    let keys = cckeys(&events);
    assert_eq!(keys.len(), 1, "one key register update: {events:?}");

    let status = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert!(status.akh_match, "the host returned the CAM's AKH");
    assert!(status.key_ok, "the SAC key reply authenticated");
    assert!(
        status.sac_header_ok,
        "the SAC headers echo serial and length"
    );
    assert_eq!(
        status.steps, 7,
        "open, auth, akh, ns, key, sac sync, cc sync"
    );
    assert_eq!(status.expected_parity, Some(0));
    let expected_key = status.expected_key.expect("precursor processed");
    let expected_iv = status.expected_iv.expect("precursor processed");

    assert_eq!(keys[0].0, 0, "even parity");
    assert_eq!(keys[0].1, expected_key);
    assert_eq!(keys[0].2, expected_iv);
    assert_eq!(rig.dev.key[0], expected_key);
    assert_eq!(rig.dev.iv[0], expected_iv);
    assert_eq!(rig.dev.parity, 0);
    assert!(!rig.dev.force_ci, "a good handshake keeps CI+");

    let path = authdata_path(&rig);
    let meta = std::fs::metadata(&path).expect("authdata written");
    assert_eq!(meta.len(), RECORD_LEN as u64, "exactly one record");
    assert_eq!(
        std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o777,
        0o600,
        "authdata is private"
    );
    std::fs::remove_dir_all(path.parent().unwrap()).ok();
}

#[test]
fn odd_parity_key_goes_to_the_odd_register() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |ciplus| {
        ciplus.parity = 1;
    });
    let keys = cckeys(&drain_events(&mut rig.dev));
    assert_eq!(keys.len(), 1);
    assert_eq!(keys[0].0, 1);
    assert_eq!(rig.dev.key[1], keys[0].1);
    assert_eq!(rig.dev.iv[1], keys[0].2);
    assert_eq!(rig.dev.key[0], [0; 16]);
    assert_eq!(rig.dev.parity, 1);
    assert!(
        rig.cam
            .lock()
            .unwrap()
            .ciplus_status()
            .expect("CI+ CAM")
            .akh_match
    );
}

#[test]
fn reconnect_reads_the_cached_akh() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |ciplus| {
        ciplus.cached_akh_probe = true;
    });
    drain_events(&mut rig.dev);
    let first = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert!(first.akh_match && first.key_ok, "first handshake completes");
    assert!(authdata_path(&rig).exists());

    rig.dev.teardown();
    attach(&mut rig.dev, &rig.cam);
    assert!(rig.dev.init_en50221());
    assert!(rig.dev.service());
    let second = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert_eq!(second.steps, 2, "open confirmation and the cached AKH");
    assert!(second.akh_match, "the stored AKH matches after a reconnect");
    assert!(
        rig.dev.find_session_for_resource(CIPLUS_CC).is_some(),
        "the CC session was reopened"
    );
}

#[test]
fn brand_cert_before_the_device_cert_is_rejected() {
    let lines = captured_lines();
    let start = lines.lock().unwrap().len();
    let mut rig = setup([8, 14, 16, 18], CIPLUS_CC, true, |_| {});
    let events = drain_events(&mut rig.dev);
    let status = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert!(
        !status.akh_match,
        "no AKH was derived without a valid chain"
    );
    assert!(!status.key_ok);
    assert!(cckeys(&events).is_empty(), "no key without a valid chain");
    assert!(
        !authdata_path(&rig).exists(),
        "a failed certificate check writes no authdata"
    );
    let captured = lines.lock().unwrap();
    assert!(
        captured[start..]
            .iter()
            .any(|line| line.contains("CICAM device cert invalid")),
        "the host log names the missing device cert"
    );
}

#[test]
fn key_register_two_is_rejected_then_zero_is_accepted() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |_| {});
    drain_events(&mut rig.dev);
    let handshake_key = rig.dev.key;
    let handshake_iv = rig.dev.iv;

    assert!(rig.cam.lock().unwrap().send_sac_key_request(2, false));
    pump(&mut rig.dev);
    let events = drain_events(&mut rig.dev);
    assert!(cckeys(&events).is_empty(), "slot 2 is not a key register");
    assert_eq!(rig.dev.key, handshake_key);
    assert_eq!(rig.dev.iv, handshake_iv);

    assert!(rig.cam.lock().unwrap().send_sac_key_request(0, false));
    pump(&mut rig.dev);
    let events = drain_events(&mut rig.dev);
    let keys = cckeys(&events);
    assert_eq!(keys.len(), 1, "the valid register is applied: {events:?}");
    assert_eq!(keys[0].0, 0);
    let status = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert_eq!(Some(keys[0].1), status.expected_key);
    assert_eq!(rig.dev.key[0], keys[0].1);
}

#[test]
fn broken_mac_and_truncated_data_request_get_no_answer() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |_| {});
    drain_events(&mut rig.dev);
    let before = host_apdu_count(&rig.cam);

    assert!(rig.cam.lock().unwrap().send_sac_key_request(0, true));
    pump(&mut rig.dev);
    assert!(drain_events(&mut rig.dev).is_empty(), "no DisableCws");
    assert_eq!(host_apdu_count(&rig.cam), before, "the bad MAC is dropped");

    assert!(
        rig.cam
            .lock()
            .unwrap()
            .send_cc_raw(CIPLUS_TAG_CC_DATA_REQ, &[0, 1, 30, 0])
    );
    pump(&mut rig.dev);
    assert!(drain_events(&mut rig.dev).is_empty());
    assert_eq!(
        host_apdu_count(&rig.cam),
        before,
        "short elements are dropped"
    );
}

#[test]
fn zero_request_count_gets_no_data_cnf() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |_| {});
    drain_events(&mut rig.dev);
    let before = host_apdu_count(&rig.cam);

    assert!(
        rig.cam
            .lock()
            .unwrap()
            .send_cc_raw(CIPLUS_TAG_CC_DATA_REQ, &[0, 0, 0])
    );
    pump(&mut rig.dev);
    assert!(drain_events(&mut rig.dev).is_empty());
    assert_eq!(
        host_apdu_count(&rig.cam),
        before,
        "an empty request loop has no answer"
    );
}

#[test]
fn uri_confirmation_matches_the_host() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |ciplus| {
        ciplus.uri_step = true;
    });
    let events = drain_events(&mut rig.dev);
    assert!(!cckeys(&events).is_empty());
    let disables = events
        .iter()
        .filter(|event| **event == HostEvent::CcDisableCws)
        .count();
    assert_eq!(
        disables, 2,
        "the key and URI SAC requests both stop descrambling: {events:?}"
    );
    let status = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert!(status.uri_match, "element 27 is sha256(uri || sha256(SAK))");
    assert!(status.akh_match && status.key_ok);
    assert!(status.sac_header_ok);
    assert_eq!(status.steps, 8);
}

#[test]
fn missing_device_pem_forces_plain_ci() {
    let rig = setup([14, 16, 18, 8], CIPLUS_CC, false, |_| {});
    assert!(rig.dev.force_ci, "no host key means no CI+");
    let status = rig.cam.lock().unwrap().ciplus_status().expect("CI+ CAM");
    assert!(!status.akh_match && !status.key_ok);
    assert!(!authdata_path(&rig).exists());
}

#[test]
fn uri_mask_follows_the_content_control_resource() {
    let mut rig = setup([14, 16, 18, 8], CIPLUS_CC, true, |ciplus| {
        ciplus.probe_uri_mask = true;
    });
    drain_events(&mut rig.dev);
    assert_eq!(
        rig.cam
            .lock()
            .unwrap()
            .ciplus_status()
            .expect("CI+ CAM")
            .uri_mask_match,
        Some(true),
        "resource version 1 advertises bitmask 1"
    );

    let mut rig = setup(
        [14, 16, 18, 8],
        CIPLUS_APP_CC_RESOURCEID_TWO,
        true,
        |ciplus| {
            ciplus.probe_uri_mask = true;
        },
    );
    drain_events(&mut rig.dev);
    assert_eq!(
        rig.cam
            .lock()
            .unwrap()
            .ciplus_status()
            .expect("CI+ CAM")
            .uri_mask_match,
        Some(true),
        "the version 2 resource advertises bitmask 3"
    );

    let mut rig = setup(
        [14, 16, 18, 8],
        TS103205_APP_CC_RESOURCEID_THREE,
        true,
        |ciplus| {
            ciplus.probe_uri_mask = true;
        },
    );
    drain_events(&mut rig.dev);
    assert_eq!(
        rig.cam
            .lock()
            .unwrap()
            .ciplus_status()
            .expect("CI+ CAM")
            .uri_mask_match,
        Some(true),
        "the TS 103 205 resource advertises bitmask 3"
    );
}

#[test]
fn debug_output_hides_key_material() {
    let state = CcState::new(
        "/etc/ssl/certs".to_owned(),
        "/var/cache".to_owned(),
        0,
        b"cam".to_vec(),
        1,
    );
    let text = format!("{state:?}");
    assert!(text.contains("akh_index"));
    assert!(!text.contains("dhsk"));
    assert!(!text.contains("sak"));

    let action = CcAction::NewKey {
        parity: 1,
        key: [0xab; 16],
        iv: [0xcd; 16],
    };
    let text = format!("{action:?}");
    assert!(text.contains("redacted"));
    assert!(
        !text.contains("171"),
        "the key bytes are not printed: {text}"
    );
    assert!(
        !text.contains("205"),
        "the IV bytes are not printed: {text}"
    );

    let event = HostEvent::CcKey {
        parity: 0,
        key: CcKeyBytes([0xab; 16]),
        iv: CcKeyBytes([0xcd; 16]),
    };
    let text = format!("{event:?}");
    assert!(text.contains("CcKeyBytes(..)"));
    assert!(!text.contains("171"));

    let ciplus = CiPlusCam::default();
    let text = format!("{ciplus:?}");
    assert!(text.contains("[redacted]"), "the CAM private key is hidden");
}

fn registry_with(tmp: &str) -> (Arc<Mutex<FakeCam>>, super::registry::CaRegistry) {
    let cam = Arc::new(Mutex::new(FakeCam::new(
        CamConfig::default(),
        Arc::new(ms_fakecam::ManualClock::new(0)),
    )));
    let mut registry = super::registry::CaRegistry::new(Arc::new(ManualClock::new(0, UNIX)));
    registry.set_cache_dir(tmp);
    (cam, registry)
}

#[test]
fn registry_keeps_ciplus_when_the_root_cert_is_present() {
    let lines = captured_lines();
    let start = lines.lock().unwrap().len();
    let cache = temp_dir("registry-cache");
    let certs = temp_dir("registry-certs");
    let (cam, mut registry) = registry_with(path_arg(&cache));
    let mut opener = CertOpener {
        cam,
        root_cert: true,
        cert_dir: path_arg(&certs).to_owned(),
    };
    assert_eq!(
        registry.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        super::registry::InitResult::Ok
    );
    assert!(!registry.get(0).unwrap().force_ci, "root.pem keeps CI+");
    let captured = lines.lock().unwrap();
    assert!(
        captured[start..]
            .iter()
            .any(|line| line.contains("CA 0: CI+ content control enabled (experimental)")),
        "the init says CI+ is on: {captured:?}"
    );
}

#[test]
fn registry_forces_plain_ci_without_a_root_cert() {
    let cache = temp_dir("registry-cache");
    let certs = temp_dir("registry-certs");
    let (cam, mut registry) = registry_with(path_arg(&cache));
    let mut opener = CertOpener {
        cam,
        root_cert: false,
        cert_dir: path_arg(&certs).to_owned(),
    };
    assert_eq!(
        registry.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        super::registry::InitResult::Ok
    );
    assert!(registry.get(0).unwrap().force_ci, "no root.pem, plain CI");
}

#[test]
fn registry_keeps_a_forced_slot_on_plain_ci() {
    let cache = temp_dir("registry-cache");
    let certs = temp_dir("registry-certs");
    let (cam, mut registry) = registry_with(path_arg(&cache));
    registry.device(0).force_ci = true;
    let mut opener = CertOpener {
        cam,
        root_cert: true,
        cert_dir: path_arg(&certs).to_owned(),
    };
    assert_eq!(
        registry.init_dev(&AdapterInfo::dvb(0, 0, 0), &mut opener),
        super::registry::InitResult::Ok
    );
    assert!(
        registry.get(0).unwrap().force_ci,
        "-C keeps a slot on plain CI"
    );
}
