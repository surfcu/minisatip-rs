//! CA_PMT list-management semantics, CAM quirks, service limits and replies,
//! driven through the same TPDU framing the C++ host uses.

mod common;

use common::{CAID, ECM};
use ms_fakecam::capmt::{
    CA_ENABLE_NOT_POSSIBLE_TECHNICAL, CA_ENABLE_POSSIBLE, CMD_NOT_SELECTED, CMD_OK_DESCRAMBLING,
    CMD_OK_MMI, CMD_QUERY, CaPmtStream,
};
use ms_fakecam::host::HostSim;
use ms_fakecam::tags::TAG_CA_PMT_REPLY;
use ms_fakecam::{
    CaPmt, CaPmtReply, CamConfig, FakeCam, ListManagement, PmtReplyMode, ProgramState, Quirks,
};

fn send(host: &mut HostSim, cam: &mut FakeCam, pmt: &CaPmt) {
    host.send_capmt(cam, &pmt.encode());
}

fn numbers(cam: &FakeCam) -> Vec<u16> {
    cam.programs()
        .programs()
        .iter()
        .map(|p| p.number())
        .collect()
}

fn states(cam: &FakeCam) -> Vec<ProgramState> {
    cam.programs().programs().iter().map(|p| p.state).collect()
}

fn reply(cam: &FakeCam, pn: u16) -> &CaPmt {
    &cam.programs()
        .programs()
        .iter()
        .find(|p| p.number() == pn)
        .expect("program listed")
        .capmt
}

fn pids(pmt: &CaPmt) -> Vec<u16> {
    pmt.streams.iter().map(|s| s.pid).collect()
}

fn replies(host: &HostSim) -> Vec<CaPmtReply> {
    host.received
        .iter()
        .filter(|(_, tag, _)| *tag == TAG_CA_PMT_REPLY)
        .map(|(_, _, body)| CaPmtReply::parse(body).expect("CA_PMT_REPLY parses"))
        .collect()
}

/// A CA_PMT whose streams carry no command at all (free to air).
fn no_cmd(lm: ListManagement, pn: u16, version: u8, stream_pids: &[u16]) -> CaPmt {
    CaPmt {
        list_management: lm,
        program_number: pn,
        version,
        current_next: true,
        cmd_id: None,
        descriptors: Vec::new(),
        streams: stream_pids
            .iter()
            .map(|&pid| CaPmtStream {
                stream_type: 2,
                pid,
                cmd_id: None,
                descriptors: Vec::new(),
            })
            .collect(),
    }
}

// From tests/test_ca.cpp:468 (test_create_capmt_different_listmgmt): every
// list-management byte reaches the CAM and acts on the program list.
#[test]
fn only_first_more_last_add_update() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 8,
        ..CamConfig::default()
    });

    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x100,
            1,
            &[0x501, 0x502],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(numbers(&cam), [0x100]);
    assert_eq!(pids(reply(&cam, 0x100)), [0x501, 0x502]);
    assert_eq!(cam.programs().descrambling(), [0x100]);

    // add keeps the programs already listed
    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Add, 0x200, 1, &[0x511], CMD_OK_DESCRAMBLING),
    );
    assert_eq!(numbers(&cam), [0x100, 0x200]);
    assert_eq!(cam.programs().descrambling(), [0x100, 0x200]);

    // update replaces the streams of one program and keeps its place
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Update,
            0x100,
            2,
            &[0x601, 0x602],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(numbers(&cam), [0x100, 0x200]);
    assert_eq!(reply(&cam, 0x100).version, 2);
    assert_eq!(pids(reply(&cam, 0x100)), [0x601, 0x602]);

    // first/more/last describe one complete replacement list
    for (lm, pn, pid) in [
        (ListManagement::First, 0x300, 0x701),
        (ListManagement::More, 0x301, 0x702),
        (ListManagement::Last, 0x302, 0x703),
    ] {
        send(
            &mut host,
            &mut cam,
            &common::capmt(lm, pn, 1, &[pid], CMD_OK_DESCRAMBLING),
        );
    }
    assert_eq!(numbers(&cam), [0x300, 0x301, 0x302]);

    // update inside the sequence keeps its place
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Update,
            0x301,
            5,
            &[0x704],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(numbers(&cam), [0x300, 0x301, 0x302]);
    assert_eq!(pids(reply(&cam, 0x301)), [0x704]);

    // only drops every program not in the message
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x400,
            1,
            &[0x801],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(numbers(&cam), [0x400]);
}

// From tests/test_ca.cpp:103 (test_create_capmt_not_selected): releasing a
// service is an update for the same program with not_selected.
#[test]
fn not_selected_releases_the_streams() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 4,
        ..CamConfig::default()
    });
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x100,
            1,
            &[0x501],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(states(&cam), [ProgramState::Descrambling]);

    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Update, 0x100, 2, &[0x502], CMD_NOT_SELECTED),
    );
    assert_eq!(states(&cam), [ProgramState::NotSelected]);
    assert!(cam.programs().descrambling().is_empty());
    assert!(cam.programs().clear_pids(0, 0).is_empty());

    // the same program can be brought back
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Update,
            0x100,
            3,
            &[0x503],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(states(&cam), [ProgramState::Descrambling]);
    assert_eq!(
        cam.programs()
            .clear_pids(0, 0)
            .into_iter()
            .collect::<Vec<_>>(),
        [0x503]
    );
}

// From tests/test_ca.cpp:514 (test_create_capmt_different_cmd_ids).
#[test]
fn command_ids_control_descrambling() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 4,
        ..CamConfig::default()
    });

    // ok_mmi also asks the CAM to descramble
    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Only, 0x100, 1, &[0x501], CMD_OK_MMI),
    );
    assert_eq!(states(&cam), [ProgramState::Descrambling]);

    // a query does not change the list
    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Only, 0x200, 1, &[0x601], CMD_QUERY),
    );
    assert_eq!(numbers(&cam), [0x100]);

    // a CA_PMT without any command means nothing to descramble
    send(
        &mut host,
        &mut cam,
        &no_cmd(ListManagement::Only, 0x300, 1, &[0x701]),
    );
    assert_eq!(states(&cam), [ProgramState::Clear]);
    assert!(cam.programs().descrambling().is_empty());
}

#[test]
fn query_gets_a_ca_pmt_reply() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 1,
        ..CamConfig::default()
    });
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x100,
            1,
            &[0x501],
            CMD_OK_DESCRAMBLING,
        ),
    );
    // OnQuery (the default) stays silent for a command that changes the list
    assert!(replies(&host).is_empty());

    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Only, 0x100, 1, &[0x501], CMD_QUERY),
    );
    let r = replies(&host);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].program_number, 0x100);
    assert_eq!(r[0].version, 1);
    assert!(r[0].current_next);
    assert_eq!(r[0].ca_enable, CA_ENABLE_POSSIBLE);
    assert_eq!(r[0].streams, [(0x501, CA_ENABLE_POSSIBLE)]);

    // a program over the service limit reports not possible
    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Add, 0x200, 1, &[0x601], CMD_OK_DESCRAMBLING),
    );
    send(
        &mut host,
        &mut cam,
        &common::capmt(ListManagement::Add, 0x200, 1, &[0x601], CMD_QUERY),
    );
    let r = replies(&host);
    assert_eq!(r.len(), 2);
    assert_eq!(r[1].program_number, 0x200);
    assert_eq!(r[1].ca_enable, CA_ENABLE_NOT_POSSIBLE_TECHNICAL);

    // a program-level query command works as well
    let mut program_level = common::capmt(
        ListManagement::Only,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );
    program_level.cmd_id = Some(CMD_QUERY);
    for s in &mut program_level.streams {
        s.cmd_id = None;
    }
    send(&mut host, &mut cam, &program_level);
    let r = replies(&host);
    assert_eq!(r.len(), 3);
    assert_eq!(r[2].ca_enable, CA_ENABLE_POSSIBLE);
}

#[test]
fn reply_modes_select_when_the_cam_answers() {
    let query = common::capmt(ListManagement::Only, 0x100, 1, &[0x501], CMD_QUERY);
    let only = common::capmt(
        ListManagement::Only,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );

    let (mut cam, mut host, _) = common::setup(CamConfig {
        pmt_reply: PmtReplyMode::Never,
        ..CamConfig::default()
    });
    send(&mut host, &mut cam, &only);
    send(&mut host, &mut cam, &query);
    assert!(replies(&host).is_empty());

    let (mut cam, mut host, _) = common::setup(CamConfig {
        pmt_reply: PmtReplyMode::Always,
        ..CamConfig::default()
    });
    send(&mut host, &mut cam, &only);
    assert_eq!(replies(&host).len(), 1);
    send(&mut host, &mut cam, &query);
    assert_eq!(replies(&host).len(), 2);
}

// A CAM that ignores update keeps the programs it already selected.
#[test]
fn ignore_update_quirk_keeps_the_old_selection() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        quirks: Quirks {
            ignore_update: true,
            ..Quirks::default()
        },
        pmt_reply: PmtReplyMode::Always,
        ..CamConfig::default()
    });
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x100,
            1,
            &[0x501],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(replies(&host).len(), 1);

    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Update,
            0x100,
            2,
            &[0x601],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(numbers(&cam), [0x100]);
    assert_eq!(reply(&cam, 0x100).version, 1);
    assert_eq!(pids(reply(&cam, 0x100)), [0x501]);
    assert_eq!(cam.programs().descrambling(), [0x100]);
    // ignored, so no reply; the APDU itself is still recorded
    assert_eq!(replies(&host).len(), 1);
    let incoming = cam.recorder().apdus();
    assert!(
        incoming
            .iter()
            .any(|r| matches!(&r.event, ms_fakecam::Event::Apdu { tag, .. } if *tag == ms_fakecam::tags::TAG_CA_PMT))
    );
}

// A CAM that restarts on `only` recovers the start latency for unchanged programs.
#[test]
fn reset_on_only_quirk_restarts_descrambling() {
    let config = CamConfig {
        quirks: Quirks {
            reset_on_only: true,
            ..Quirks::default()
        },
        descramble_latency_ms: 100,
        ..CamConfig::default()
    };
    let (mut cam, mut host, clock) = common::setup(config);
    let only = common::capmt(
        ListManagement::Only,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );
    send(&mut host, &mut cam, &only);
    assert_eq!(cam.programs().programs()[0].active_since, Some(0));

    clock.advance(50);
    send(&mut host, &mut cam, &only);
    assert_eq!(cam.programs().programs()[0].active_since, Some(50));

    // the latency counts from the restart
    cam.write_ts(&common::scrambled(0x501, 0));
    assert_eq!(common::scrambling(&cam.read_ts()), [(0x501, 3)]);
    clock.advance(100);
    cam.write_ts(&common::scrambled(0x501, 1));
    assert_eq!(common::scrambling(&cam.read_ts()), [(0x501, 0)]);
}

#[test]
fn without_the_quirk_only_keeps_unchanged_programs_running() {
    let config = CamConfig {
        descramble_latency_ms: 100,
        ..CamConfig::default()
    };
    let (mut cam, mut host, clock) = common::setup(config);
    let only = common::capmt(
        ListManagement::Only,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );
    send(&mut host, &mut cam, &only);
    clock.advance(50);
    send(&mut host, &mut cam, &only);
    assert_eq!(cam.programs().programs()[0].active_since, Some(0));
}

#[test]
fn restart_on_update_quirk_restarts_the_program() {
    let config = CamConfig {
        quirks: Quirks {
            restart_on_update: true,
            ..Quirks::default()
        },
        ..CamConfig::default()
    };
    let (mut cam, mut host, clock) = common::setup(config);
    let only = common::capmt(
        ListManagement::Only,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );
    let update = common::capmt(
        ListManagement::Update,
        0x100,
        1,
        &[0x501],
        CMD_OK_DESCRAMBLING,
    );
    send(&mut host, &mut cam, &only);
    clock.advance(50);
    send(&mut host, &mut cam, &update);
    assert_eq!(cam.programs().programs()[0].active_since, Some(50));

    let (mut cam, mut host, clock) = common::setup(CamConfig::default());
    send(&mut host, &mut cam, &only);
    clock.advance(50);
    send(&mut host, &mut cam, &update);
    assert_eq!(cam.programs().programs()[0].active_since, Some(0));
}

#[test]
fn service_limit_marks_programs_over_limit() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 1,
        ..CamConfig::default()
    });
    for (lm, pn, pid) in [
        (ListManagement::First, 0x100, 0x501),
        (ListManagement::More, 0x200, 0x502),
        (ListManagement::Last, 0x300, 0x503),
    ] {
        send(
            &mut host,
            &mut cam,
            &common::capmt(lm, pn, 1, &[pid], CMD_OK_DESCRAMBLING),
        );
    }
    assert_eq!(
        states(&cam),
        [
            ProgramState::Descrambling,
            ProgramState::OverLimit,
            ProgramState::OverLimit
        ]
    );
    assert_eq!(cam.programs().descrambling(), [0x100]);
    assert_eq!(
        cam.programs()
            .clear_pids(0, 0)
            .into_iter()
            .collect::<Vec<_>>(),
        [0x501]
    );

    // replacing the list gives the place to another program
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x200,
            1,
            &[0x502],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(states(&cam), [ProgramState::Descrambling]);
    assert_eq!(cam.programs().descrambling(), [0x200]);
}

#[test]
fn a_cam_with_no_services_descrambles_nothing() {
    let (mut cam, mut host, _) = common::setup(CamConfig {
        max_services: 0,
        ..CamConfig::default()
    });
    send(
        &mut host,
        &mut cam,
        &common::capmt(
            ListManagement::Only,
            0x100,
            1,
            &[0x501],
            CMD_OK_DESCRAMBLING,
        ),
    );
    assert_eq!(states(&cam), [ProgramState::OverLimit]);
    assert!(cam.programs().descrambling().is_empty());
    assert!(cam.programs().clear_pids(0, 0).is_empty());
}

// The CA descriptor contents do not decide descrambling; the host asserts it
// with cmd ids (tests/test_ca.cpp:207, :294 carry CAID 0x0B00 on ECM 0x573).
#[test]
fn ca_descriptors_are_kept_for_the_recorded_log() {
    let (mut cam, mut host, _) = common::setup(CamConfig::default());
    let pmt = common::capmt(
        ListManagement::Only,
        0x1234,
        1,
        &[0x501, 0x502],
        CMD_OK_DESCRAMBLING,
    );
    send(&mut host, &mut cam, &pmt);
    let listed = reply(&cam, 0x1234);
    assert_eq!(
        ms_fakecam::capmt::ca_descriptors(&listed.streams[0].descriptors),
        [(CAID, ECM)]
    );
    let log = cam.recorder().to_untimed_text();
    assert!(log.contains("/0B00:0573"), "{log}");
}
