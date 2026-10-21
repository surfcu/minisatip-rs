//! Transport and session establishment against the ca.cpp host byte formats.

mod common;

use ms_fakecam::host::{
    HOST_RESOURCES, HostSim, apdu_tpdu, date_time_body, parse_r_tpdu, spdu, tpdu,
};
use ms_fakecam::tags::*;
use ms_fakecam::{AppInfo, CamConfig, FakeCam, ManualClock};
use std::sync::Arc;

fn cam(config: CamConfig) -> FakeCam {
    FakeCam::new(config, Arc::new(ManualClock::new(0)))
}

#[test]
fn transport_and_resource_manager_bytes() {
    let mut cam = cam(CamConfig::default());
    // Create TC: CTC reply plus a status byte saying data is waiting
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_CREATE_TC, &[])).unwrap(),
        [0, 1, 0x83, 1, 1, 0x80, 2, 1, 0x80]
    );
    // An empty poll (ca_timeout) only gets the status
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_DATA_LAST, &[])).unwrap(),
        [0, 1, 0x80, 2, 1, 0x80]
    );
    // T_RCV returns the open session request for the resource manager
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap(),
        [0, 1, 0xA0, 7, 1, 0x91, 4, 0, 1, 0, 0x41, 0x80, 2, 1, 0]
    );
    let mut rsp = vec![0];
    rsp.extend_from_slice(&RESOURCE_MANAGER.to_be_bytes());
    let open = spdu(1, ST_OPEN_SESSION_RESPONSE, &rsp, &[]);
    assert_eq!(open, [0x92, 7, 0, 0, 1, 0, 0x41, 0, 1]);
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_DATA_LAST, &open)).unwrap(),
        [0, 1, 0x80, 2, 1, 0]
    );
    // APP_RM_create: profile enquiry, answered with the CAM's resources
    let enq = apdu_tpdu(1, TAG_PROFILE_ENQUIRY, &[]);
    assert_eq!(enq, [0, 1, 0xA0, 9, 1, 0x90, 2, 0, 1, 0x9F, 0x80, 0x10, 0]);
    assert_eq!(cam.handle_tpdu(&enq).unwrap(), [0, 1, 0x80, 2, 1, 0x80]);
    let mut profile = vec![0, 1, 0xA0, 29, 1, 0x90, 2, 0, 1, 0x9F, 0x80, 0x11, 20];
    for r in [
        0x0001_0041u32,
        0x0002_0041,
        0x0003_0041,
        0x0024_0041,
        0x0040_0041,
    ] {
        profile.extend_from_slice(&r.to_be_bytes());
    }
    profile.extend_from_slice(&[0x80, 2, 1, 0]);
    assert_eq!(cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap(), profile);
    // Host profile_change: the CAM asks for the host's profile
    cam.handle_tpdu(&apdu_tpdu(1, TAG_PROFILE_CHANGE, &[]));
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap(),
        [
            0, 1, 0xA0, 9, 1, 0x90, 2, 0, 1, 0x9F, 0x80, 0x10, 0, 0x80, 2, 1, 0
        ]
    );
    // Once the host replies, the CAM opens Application Information
    cam.handle_tpdu(&apdu_tpdu(1, TAG_PROFILE, &[0, 1, 0, 0x41]));
    assert_eq!(cam.host_resources(), [RESOURCE_MANAGER]);
    let r = cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap();
    // From tests/test_ca.cpp:1758 (ST_OPEN_SESSION_REQUEST for AI)
    assert_eq!(&r[5..11], &[0x91, 0x04, 0x00, 0x02, 0x00, 0x41]);
}

#[test]
fn full_setup_with_the_host_simulator() {
    let config = CamConfig {
        caids: vec![0x0B00, 0x0B01],
        datetime_interval_s: 30,
        ..CamConfig::default()
    };
    let (cam, host, _) = common::setup(config);
    let opened: Vec<u32> = host.sessions.values().copied().collect();
    assert_eq!(
        opened,
        [
            RESOURCE_MANAGER,
            APPLICATION_INFO,
            CA_SUPPORT,
            DATE_TIME,
            MMI
        ]
    );
    assert_eq!(
        cam.sessions(),
        host.sessions.clone().into_iter().collect::<Vec<_>>()
    );
    assert_eq!(host.caids, [0x0B00, 0x0B01]);
    assert_eq!(host.cam_name, b"Fake CAM");
    assert_eq!(cam.host_resources(), HOST_RESOURCES);
    assert!(cam.ca_ready());
    let dt = date_time_body(host.unix_time);
    assert_eq!(cam.last_date_time(), Some(&dt[..]));
    // DATE_TIME_ENQUIRY carries the response interval
    assert!(
        host.received
            .iter()
            .any(|(_, t, b)| *t == TAG_DATE_TIME_ENQUIRY && b == &[30])
    );
}

#[test]
fn force_ci_host_profile_stops_before_ci_plus() {
    let mut cam = cam(CamConfig::default());
    let mut host = HostSim::new();
    host.force_ci = true;
    host.connect(&mut cam);
    assert_eq!(cam.host_resources(), &HOST_RESOURCES[..6]);
}

#[test]
fn refused_resource_is_skipped() {
    // From tests/test_ca.cpp:1844 (TS 103 205 MMI has no host handler)
    let ts103205_mmi = mkrid(64, 2, 1);
    let config = CamConfig {
        resources: vec![RESOURCE_MANAGER, ts103205_mmi, CA_SUPPORT],
        ..CamConfig::default()
    };
    let (cam, host, _) = common::setup(config);
    assert_eq!(
        host.sessions.values().copied().collect::<Vec<_>>(),
        [RESOURCE_MANAGER, CA_SUPPORT]
    );
    let log = cam.recorder().to_untimed_text();
    assert!(log.contains("cam0 C>H open_session 00400081\n"), "{log}");
    assert!(
        log.contains("open_session_rsp s65535 00400081 st=F0"),
        "{log}"
    );
}

// From tests/test_ca.cpp:1767 (APP_INFO with a 200-byte name, session 1)
#[test]
fn application_info_matches_the_long_name_fixture() {
    let config = CamConfig {
        resources: vec![APPLICATION_INFO],
        app_info: AppInfo {
            application_type: 1,
            manufacturer: 0,
            manufacturer_code: 0,
            menu: "A".repeat(200),
        },
        ..CamConfig::default()
    };
    let mut cam = cam(config);
    cam.start();
    assert_eq!(cam.take_spdus(), [vec![0x91, 0x04, 0x00, 0x02, 0x00, 0x41]]);
    let mut rsp = vec![0];
    rsp.extend_from_slice(&APPLICATION_INFO.to_be_bytes());
    assert!(
        cam.handle_spdu(&spdu(1, ST_OPEN_SESSION_RESPONSE, &rsp, &[]))
            .is_empty()
    );
    let out = cam.handle_spdu(&spdu(1, ST_SESSION_NUMBER, &[], &[0x9F, 0x80, 0x20, 0]));
    let mut expected = vec![
        0x90, 0x02, 0x00, 0x01, 0x9F, 0x80, 0x21, 0x81, 0xCE, 0x01, 0x00, 0x00, 0x00, 0x00, 0xC8,
    ];
    expected.extend_from_slice(&[b'A'; 200]);
    assert_eq!(out, [expected]);
}

// From tests/test_ca.cpp:1858 (MMI display control APDU on session 1)
#[test]
fn mmi_display_control_matches_fixture() {
    let config = CamConfig {
        resources: vec![MMI],
        ..CamConfig::default()
    };
    let mut cam = cam(config);
    cam.start();
    cam.take_spdus();
    let mut rsp = vec![0];
    rsp.extend_from_slice(&MMI.to_be_bytes());
    cam.handle_spdu(&spdu(1, ST_OPEN_SESSION_RESPONSE, &rsp, &[]));
    assert!(cam.send_to_resource(MMI, TAG_DISPLAY_CONTROL, &[]));
    assert_eq!(
        cam.take_spdus(),
        [vec![0x90, 0x02, 0x00, 0x01, 0x9F, 0x88, 0x01, 0x00]]
    );
    assert!(!cam.send_to_resource(CA_SUPPORT, TAG_CA_INFO, &[]));
}

#[test]
fn mmi_enquiry_is_closed_by_the_host() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    assert!(cam.send_to_resource(MMI, TAG_DISPLAY_CONTROL, &[1, 1]));
    assert!(cam.send_to_resource(MMI, TAG_ENQUIRY, &[0x01, 4, b'P', b'I', b'N']));
    host.pump(&mut cam);
    let log = cam.recorder().to_untimed_text();
    assert!(
        log.contains("H>C apdu s5 00400041 9F8802 display_reply 0101"),
        "{log}"
    );
    assert!(
        log.contains("H>C apdu s5 00400041 9F8800 close_mmi 00"),
        "{log}"
    );
}

#[test]
fn close_and_delete_reset_the_cam() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    // Host-initiated close of the CA session (session 3)
    let r = cam
        .handle_tpdu(&tpdu(T_DATA_LAST, &[0x95, 2, 0, 3]))
        .unwrap();
    assert_eq!(r, [0, 1, 0x80, 2, 1, 0x80]);
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap(),
        [0, 1, 0xA0, 6, 1, 0x96, 3, 0, 0, 3, 0x80, 2, 1, 0]
    );
    assert!(!cam.ca_ready());
    // CAM-initiated close of the MMI session
    assert!(cam.request_close(MMI));
    host.pump(&mut cam);
    assert!(!host.sessions.values().any(|r| *r == MMI));
    assert!(!cam.sessions().iter().any(|(_, r)| *r == MMI));
    // Delete TC drops every session
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_DELETE_TC, &[])).unwrap(),
        [0, 1, 0x85, 1, 1, 0x80, 2, 1, 0]
    );
    assert!(cam.sessions().is_empty());
    // Without a transport connection nothing is delivered
    assert_eq!(
        cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap(),
        [0, 1, 0x80, 2, 1, 0]
    );
}

#[test]
fn fragmented_spdus_reassemble() {
    let config = CamConfig {
        max_tpdu_data: Some(8),
        ..CamConfig::default()
    };
    let (cam, host, _) = common::setup(config);
    assert_eq!(host.caids, [0x0B00]);
    // Host to CAM: an SPDU split into T_DATA_MORE + T_DATA_LAST
    let mut cam = cam;
    cam.recorder().clear();
    let full = spdu(3, ST_SESSION_NUMBER, &[], &[0x9F, 0x80, 0x30, 0]);
    cam.handle_tpdu(&tpdu(T_DATA_MORE, &full[..3]));
    cam.handle_tpdu(&tpdu(T_DATA_LAST, &full[3..]));
    let log = cam.recorder().to_untimed_text();
    assert_eq!(
        log.matches("H>C apdu s3 00030041 9F8030 ca_info_enq")
            .count(),
        1
    );
}

// The module splits its own SPDUs when the transport limits the chunk size.
#[test]
fn module_spdus_are_split_into_t_data_chunks() {
    let config = CamConfig {
        resources: vec![RESOURCE_MANAGER],
        max_tpdu_data: Some(5),
        ..CamConfig::default()
    };
    let mut cam = cam(config);
    cam.handle_tpdu(&tpdu(T_CREATE_TC, &[])).unwrap();
    assert_eq!(cam.take_spdus(), [vec![0x91, 0x04, 0x00, 0x01, 0x00, 0x41]]);
    let mut rsp = vec![0];
    rsp.extend_from_slice(&RESOURCE_MANAGER.to_be_bytes());
    cam.handle_spdu(&spdu(1, ST_OPEN_SESSION_RESPONSE, &rsp, &[]));
    // Host asks for the CAM profile; the reply is larger than one chunk.
    cam.handle_tpdu(&apdu_tpdu(1, TAG_PROFILE_ENQUIRY, &[]));

    let mut payload = Vec::new();
    let mut chunks = Vec::new();
    let mut sb = None;
    for _ in 0..100 {
        let r = parse_r_tpdu(&cam.handle_tpdu(&tpdu(T_RCV, &[])).unwrap()).unwrap();
        sb = r.sb;
        let (tag, data) = r.objects.into_iter().next().expect("data chunk");
        chunks.push(tag);
        payload.extend_from_slice(&data);
        if tag == T_DATA_LAST {
            break;
        }
    }
    let expected = spdu(
        1,
        ST_SESSION_NUMBER,
        &[],
        &ms_fakecam::cam::apdu(TAG_PROFILE, &RESOURCE_MANAGER.to_be_bytes()),
    );
    assert_eq!(payload, expected);
    assert_eq!(chunks.first(), Some(&T_DATA_MORE));
    assert_eq!(chunks.last(), Some(&T_DATA_LAST));
    assert!(chunks.len() > 1);
    // last status byte reports that nothing is left
    assert_eq!(sb, Some(0));
}

#[test]
fn malformed_tpdus_are_ignored() {
    let mut cam = cam(CamConfig::default());
    assert_eq!(cam.handle_tpdu(&[]), None);
    assert_eq!(cam.handle_tpdu(&[0, 1]), None);
    assert_eq!(cam.handle_tpdu(&[0, 1, 0xA0, 0x83]), None);
    assert_eq!(cam.handle_tpdu(&[0, 1, 0xA0, 5, 1]), None);
    // Unknown tags still get a status reply
    assert_eq!(
        cam.handle_tpdu(&[0, 1, 0x86, 1, 1]).unwrap(),
        [0, 1, 0x80, 2, 1, 0]
    );
    // Garbage SPDUs on an open connection do not panic
    cam.handle_tpdu(&tpdu(T_CREATE_TC, &[]));
    for bad in [
        &[0x90u8][..],
        &[0x90, 0x83],
        &[0x92, 2, 0, 0],
        &[0x90, 2, 0, 9, 0x9F],
    ] {
        cam.handle_tpdu(&tpdu(T_DATA_LAST, bad));
    }
}
