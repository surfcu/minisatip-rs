//! The recorded APDU log: every exchange, stable text, multiple CAM instances.

mod common;

use ms_fakecam::host::HostSim;
use ms_fakecam::tags::*;
use ms_fakecam::{CamConfig, FakeCam, ListManagement, ManualClock, Recorder};
use std::sync::Arc;

fn scripted_session() -> (FakeCam, HostSim, ManualClock) {
    let config = CamConfig {
        resources: vec![RESOURCE_MANAGER, CA_SUPPORT],
        caids: vec![0x0B00, 0x0B01],
        ..CamConfig::default()
    };
    let (mut cam, mut host, clock) = common::setup(config);
    clock.advance(10);
    let only = common::capmt(
        ListManagement::Only,
        0x1234,
        1,
        &[0x501, 0x502],
        ms_fakecam::capmt::CMD_OK_DESCRAMBLING,
    );
    host.send_capmt(&mut cam, &only.encode());
    clock.advance(10);
    let query = common::capmt(
        ListManagement::Only,
        0x1234,
        1,
        &[0x501, 0x502],
        ms_fakecam::capmt::CMD_QUERY,
    );
    host.send_capmt(&mut cam, &query.encode());
    clock.advance(10);
    let release = common::capmt(
        ListManagement::Update,
        0x1234,
        2,
        &[0x501, 0x502],
        ms_fakecam::capmt::CMD_NOT_SELECTED,
    );
    host.send_capmt(&mut cam, &release.encode());
    (cam, host, clock)
}

// Stable snapshot of a whole session: setup, CA_INFO, three CA_PMTs and one
// CA_PMT_REPLY, timestamps from the injected clock.
#[test]
fn recorded_log_matches_snapshot() {
    let (cam, _, _) = scripted_session();
    let log = cam.recorder().to_text();
    let expected = concat!(
        "       0 cam0 H>C create_tc\n",
        "       0 cam0 C>H open_session 00010041\n",
        "       0 cam0 C>H ctc_reply\n",
        "       0 cam0 H>C open_session_rsp s1 00010041 st=00\n",
        "       0 cam0 H>C apdu s1 00010041 9F8010 profile_enq -\n",
        "       0 cam0 C>H apdu s1 00010041 9F8011 profile 0001004100030041\n",
        "       0 cam0 H>C apdu s1 00010041 9F8012 profile_change -\n",
        "       0 cam0 C>H apdu s1 00010041 9F8010 profile_enq -\n",
        "       0 cam0 H>C apdu s1 00010041 9F8011 profile 0001004100020041000300410020004100240041004000410001004200020042000200430020004200200043008C1001008C1002008C1004008D1001008E1001008F1001008F1002008F10030041004100410042\n",
        "       0 cam0 C>H open_session 00030041\n",
        "       0 cam0 H>C open_session_rsp s2 00030041 st=00\n",
        "       0 cam0 H>C apdu s2 00030041 9F8030 ca_info_enq -\n",
        "       0 cam0 C>H apdu s2 00030041 9F8031 ca_info 0B000B01\n",
        "      10 cam0 H>C apdu s2 00030041 9F8032 ca_pmt 03123403000002050100070109040B00057303050200070109040B000573 # lm=only pn=1234 v=1 cmd=01 es=0501/0B00:0573 es=0502/0B00:0573\n",
        "      20 cam0 H>C apdu s2 00030041 9F8032 ca_pmt 03123403000002050100070309040B00057303050200070309040B000573 # lm=only pn=1234 v=1 cmd=03 es=0501/0B00:0573 es=0502/0B00:0573\n",
        "      20 cam0 C>H apdu s2 00030041 9F8033 ca_pmt_reply 1234C381E50181E50281\n",
        "      30 cam0 H>C apdu s2 00030041 9F8032 ca_pmt 05123405000002050100070409040B00057303050200070409040B000573 # lm=update pn=1234 v=2 cmd=04 es=0501/0B00:0573 es=0502/0B00:0573\n",
    );
    assert_eq!(log, expected);
}

#[test]
fn every_apdu_is_recorded_in_both_directions() {
    let (cam, _, _) = scripted_session();
    let apdus = cam.recorder().apdus();
    let incoming: Vec<u32> = apdus
        .iter()
        .filter(|r| r.dir == ms_fakecam::Direction::HostToCam)
        .filter_map(|r| match &r.event {
            ms_fakecam::Event::Apdu { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect();
    let outgoing: Vec<u32> = apdus
        .iter()
        .filter(|r| r.dir == ms_fakecam::Direction::CamToHost)
        .filter_map(|r| match &r.event {
            ms_fakecam::Event::Apdu { tag, .. } => Some(*tag),
            _ => None,
        })
        .collect();
    // profile inquiry/change/reply, ca info enquiry, then three CA_PMTs
    assert_eq!(
        incoming,
        [
            TAG_PROFILE_ENQUIRY,
            TAG_PROFILE_CHANGE,
            TAG_PROFILE,
            TAG_CA_INFO_ENQUIRY,
            TAG_CA_PMT,
            TAG_CA_PMT,
            TAG_CA_PMT
        ]
    );
    // profile, the CAM's own enquiry, ca info, then one reply for the query
    assert_eq!(
        outgoing,
        [
            TAG_PROFILE,
            TAG_PROFILE_ENQUIRY,
            TAG_CA_INFO,
            TAG_CA_PMT_REPLY
        ]
    );
}

#[test]
fn multi_instance_tags_each_cam_in_a_shared_log() {
    let clock = ManualClock::new(0);
    let recorder = Recorder::new();
    let mut cam0 = FakeCam::with_recorder(
        CamConfig {
            id: 0,
            resources: vec![RESOURCE_MANAGER, CA_SUPPORT],
            ..CamConfig::default()
        },
        Arc::new(clock.clone()),
        recorder.clone(),
    );
    let mut cam1 = FakeCam::with_recorder(
        CamConfig {
            id: 1,
            resources: vec![RESOURCE_MANAGER, CA_SUPPORT],
            caids: vec![0x0B01],
            ..CamConfig::default()
        },
        Arc::new(clock.clone()),
        recorder.clone(),
    );
    let mut host0 = HostSim::new();
    let mut host1 = HostSim::new();
    host0.connect(&mut cam0);
    host1.connect(&mut cam1);

    let pmt0 = common::capmt(
        ListManagement::Only,
        0x0100,
        1,
        &[0x501],
        ms_fakecam::capmt::CMD_OK_DESCRAMBLING,
    );
    let pmt1 = common::capmt(
        ListManagement::Only,
        0x0200,
        1,
        &[0x601],
        ms_fakecam::capmt::CMD_OK_DESCRAMBLING,
    );
    host0.send_capmt(&mut cam0, &pmt0.encode());
    host1.send_capmt(&mut cam1, &pmt1.encode());

    assert_eq!(host0.caids, [0x0B00]);
    assert_eq!(host1.caids, [0x0B01]);
    assert_eq!(cam0.programs().descrambling(), [0x0100]);
    assert_eq!(cam1.programs().descrambling(), [0x0200]);

    let log = recorder.to_text();
    assert!(log.contains("cam0 H>C"), "{log}");
    assert!(log.contains("cam1 H>C"), "{log}");
    assert!(log.contains("# lm=only pn=0100"), "{log}");
    assert!(log.contains("# lm=only pn=0200"), "{log}");
    // each instance's CA_PMT stays on its own cam
    for line in log.lines() {
        if line.contains("pn=0100") {
            assert!(line.contains("cam0 "), "{line}");
        }
        if line.contains("pn=0200") {
            assert!(line.contains("cam1 "), "{line}");
        }
    }
}

#[test]
fn untimed_text_is_a_stable_diff_form() {
    let (cam, _, _) = scripted_session();
    let timed = cam.recorder().to_text();
    let untimed = cam.recorder().to_untimed_text();
    assert_eq!(timed.lines().count(), untimed.lines().count());
    // stripping the timestamp column yields the same records
    let stripped: String = timed
        .lines()
        .map(|l| {
            let rest = l.trim_start().split_once(' ').map_or(l, |(_, rest)| rest);
            format!("{rest}\n")
        })
        .collect();
    assert_eq!(stripped, untimed);
}
