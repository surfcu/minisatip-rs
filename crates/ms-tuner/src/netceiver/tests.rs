use super::fake::{FakeMcli, McliCall};
use super::*;
use crate::tuner::{DeliverySystem, PidSet, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner};

fn dvbs(freq: i32) -> TuneParams {
    TuneParams {
        freq,
        msys: DeliverySystem::DvbS2,
        sr: 27_500_000,
        ..TuneParams::default()
    }
}

fn rx(n: usize) -> ReceiverId {
    ReceiverId(n)
}

fn packets(n: usize, tag: u8) -> Vec<u8> {
    let mut data = vec![tag; n * TS_PACKET_SIZE];
    for p in data.chunks_mut(TS_PACKET_SIZE) {
        p[0] = 0x47;
    }
    data
}

#[test]
fn tune_then_pids_commits_add_tune_and_pid_list() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 3);
    t.tune(&dvbs(11_494_000)).unwrap();
    assert!(t.api().calls.is_empty(), "tune alone only arms want_tune");
    t.set_pids(&PidSet::from_pids([0, 100, 101])).unwrap();
    assert_eq!(
        t.api().calls,
        vec![
            McliCall::RecvAdd(Some(rx(1))),
            McliCall::RecvTune(rx(1), mcli_tune(&dvbs(11_494_000))),
            McliCall::Sleep(10),
            McliCall::RecvPids(rx(1), vec![0, 100, 101]),
        ]
    );
    assert_eq!(t.active_pids(), [0, 100, 101]);
}

#[test]
fn unchanged_pids_do_not_commit_and_empty_set_stops() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    t.tune(&dvbs(10_000_000)).unwrap();
    t.set_pids(&PidSet::from_pids([0, 17])).unwrap();
    let n = t.api().calls.len();
    t.set_pids(&PidSet::from_pids([0, 17])).unwrap();
    assert_eq!(t.api().calls.len(), n);
    t.set_pids(&PidSet::from_pids([17, 18])).unwrap();
    assert_eq!(
        t.api().calls[n..],
        [McliCall::Sleep(10), McliCall::RecvPids(rx(1), vec![17, 18])]
    );
    t.set_pids(&PidSet::new()).unwrap();
    assert_eq!(t.api().calls.last(), Some(&McliCall::RecvStop(rx(1))));
    assert!(t.active_pids().is_empty());
}

#[test]
fn set_pid_returns_the_cpp_fd_and_caps_at_max_pids() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 5);
    for pid in 0..MAX_PIDS as u16 {
        assert_eq!(t.set_pid(pid), 105);
    }
    assert_eq!(t.set_pid(999), -1);
    assert_eq!(t.active_pids().len(), MAX_PIDS);
}

#[test]
fn del_pid_shifts_like_cpp_including_stale_entries() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    for pid in [10, 20, 30] {
        t.set_pid(pid);
    }
    assert_eq!(t.del_pid(10), 0);
    assert_eq!(t.active_pids(), [20, 30]);
    // Absent PID: nothing moves, lp stays.
    t.del_pid(99);
    assert_eq!(t.active_pids(), [20, 30]);
    // A retune zeroes lp but keeps npid; deleting a stale entry never underflows lp.
    t.netcv_tune(&dvbs(10_000_000));
    t.del_pid(30);
    assert!(t.active_pids().is_empty());
    t.set_pid(40);
    assert_eq!(t.active_pids(), [40]);
}

#[test]
fn retune_readds_current_pids_and_reuses_the_receiver() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    t.tune(&dvbs(11_000_000)).unwrap();
    t.set_pids(&PidSet::from_pids([0, 256])).unwrap();
    let n = t.api().calls.len();
    t.tune(&dvbs(12_000_000)).unwrap();
    assert_eq!(
        t.api().calls[n..],
        [
            McliCall::RecvTune(rx(1), mcli_tune(&dvbs(12_000_000))),
            McliCall::Sleep(10),
            McliCall::RecvPids(rx(1), vec![0, 256]),
        ]
    );
}

#[test]
fn failed_recv_add_sets_err_and_is_retried() {
    let mut fake = FakeMcli::new();
    fake.fail_add = true;
    let mut t = NetceiverTuner::new(fake, 0);
    t.tune(&dvbs(11_000_000)).unwrap();
    t.set_pids(&PidSet::from_pids([0])).unwrap();
    assert_eq!(t.api().calls, vec![McliCall::RecvAdd(None)]);
    // err makes set_pid return 0 and skip the PID, like netcv_set_pid.
    assert_eq!(t.set_pid(1), 0);
    t.api_mut().fail_add = false;
    t.set_pids(&PidSet::from_pids([0, 1])).unwrap();
    assert_eq!(t.api().calls.last(), Some(&McliCall::RecvAdd(Some(rx(1)))));
    t.set_pids(&PidSet::from_pids([0, 1, 2])).unwrap();
    assert_eq!(
        t.api().receiver_calls().last(),
        Some(&McliCall::RecvPids(rx(1), vec![0, 1, 2]))
    );
}

#[test]
fn ts_handler_queues_only_active_well_formed_data() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    let shared = std::sync::Arc::clone(t.shared());
    // No PIDs yet: consumed and dropped.
    assert_eq!(shared.handle_ts(&packets(2, 1)), 2 * TS_PACKET_SIZE);
    t.tune(&dvbs(11_000_000)).unwrap();
    t.set_pids(&PidSet::from_pids([0])).unwrap();
    assert_eq!(
        t.api().deliver_ts(rx(1), &packets(3, 2)),
        3 * TS_PACKET_SIZE
    );
    let mut bad = packets(1, 3);
    bad[0] = 0x00;
    shared.handle_ts(&bad);
    shared.handle_ts(&packets(1, 4)[..100]);
    let mut buf = TsBuf::with_packets(2);
    assert_eq!(t.read(&mut buf).unwrap(), 2 * TS_PACKET_SIZE);
    let mut buf = TsBuf::with_packets(8);
    assert_eq!(t.read(&mut buf).unwrap(), TS_PACKET_SIZE);
    assert!(buf.packets().all(|p| p[1] == 2));
    assert_eq!(t.read(&mut buf).unwrap(), 0);
}

#[test]
fn pipe_overflow_drops_whole_packets() {
    let t = NetceiverTuner::new(FakeMcli::new(), 0);
    let shared = t.shared();
    shared.set_active_pids(1);
    let packets_in_pipe = NETCV_PIPE_SIZE / TS_PACKET_SIZE;
    shared.handle_ts(&packets(packets_in_pipe - 1, 0));
    shared.handle_ts(&packets(3, 0));
    assert_eq!(shared.dropped_bytes(), 2 * TS_PACKET_SIZE as u64);
}

#[test]
fn ten_handler_maps_signal_and_tune_resets_it() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    t.shared().handle_ten(Some(&FeStatus {
        st: 0x1f,
        ber: 7,
        strength: 0xC812,
        snr: 0x6400,
    }));
    let status = t.status();
    assert!(status.locked);
    assert_eq!((status.strength, status.snr), (0xC8, 0x64));
    assert_eq!(t.shared().signal().ber, 7);
    t.shared().handle_ten(Some(&FeStatus {
        st: 0x0f,
        ..FeStatus::default()
    }));
    assert!(!t.status().locked);
    t.shared().handle_ten(None);
    t.tune(&dvbs(11_000_000)).unwrap();
    assert_eq!(t.shared().signal(), NetcvSignal::default());
}

#[test]
fn drop_deletes_the_receiver() {
    let mut t = NetceiverTuner::new(FakeMcli::new(), 0);
    t.tune(&dvbs(11_000_000)).unwrap();
    t.set_pids(&PidSet::from_pids([0])).unwrap();
    t.close();
    assert_eq!(t.api().calls.last(), Some(&McliCall::RecvDel(rx(1))));
    assert!(t.api().handlers.is_empty());
    let n = t.api().calls.len();
    t.close();
    assert_eq!(t.api().calls.len(), n);
}

fn nc(uuid: &str, types: &[i32]) -> NetceiverInfo {
    NetceiverInfo {
        uuid: uuid.into(),
        tuners: types
            .iter()
            .map(|&fe_type| NetcvTunerInfo {
                name: format!("tuner{fe_type}"),
                fe_type,
            })
            .collect(),
    }
}

fn cfg(count: usize) -> NetcvConfig {
    NetcvConfig {
        ifname: "vlan4".into(),
        count,
    }
}

#[test]
fn discovery_without_option_does_nothing() {
    let mut fake = FakeMcli::new();
    let d = find_netcv_adapters(&mut fake, None, 2, 100, |_| false).unwrap();
    assert_eq!(d.a_count, 2);
    assert!(fake.calls.is_empty());
}

#[test]
fn discovery_fails_when_interface_is_down() {
    let mut fake = FakeMcli::new();
    fake.interface_up = false;
    let err = find_netcv_adapters(&mut fake, Some(&cfg(1)), 0, 100, |_| false).unwrap_err();
    assert_eq!(err, NetcvError::InterfaceUnavailable("vlan4".into()));
    assert_eq!(fake.calls, vec![McliCall::InterfaceReady("vlan4".into())]);
}

#[test]
fn discovery_polls_until_count_then_assigns_s2_first() {
    let mut fake = FakeMcli::new();
    let first = nc("a", &[FE_QAM, FE_DVBS2]);
    let second = nc("b", &[FE_OFDM, FE_QPSK, FE_DVBS2]);
    fake.lists = vec![vec![], vec![first.clone()], vec![first, second]];
    let d = find_netcv_adapters(&mut fake, Some(&cfg(2)), 1, 100, |_| false).unwrap();
    assert_eq!(
        fake.calls[..5],
        [
            McliCall::InterfaceReady("vlan4".into()),
            McliCall::RecvInit("vlan4".into(), MCLI_PORT),
            McliCall::ApiSockInit(API_SOCK_NAMESPACE.into()),
            McliCall::Sleep(250),
            McliCall::GetList,
        ]
    );
    let gets = fake
        .calls
        .iter()
        .filter(|c| **c == McliCall::GetList)
        .count();
    assert_eq!(gets, 3);
    assert_eq!(
        fake.calls[fake.calls.len() - 2..],
        [McliCall::LockList, McliCall::UnlockList]
    );
    let types: Vec<(usize, i32)> = d.adapters.iter().map(|a| (a.id, a.fe_type)).collect();
    assert_eq!(
        types,
        [
            (1, FE_DVBS2),
            (2, FE_DVBS2),
            (3, FE_QPSK),
            (4, FE_QAM),
            (5, FE_OFDM)
        ]
    );
    assert_eq!(
        d.adapters[0].sys,
        [DeliverySystem::DvbS2, DeliverySystem::DvbS]
    );
    assert_eq!(d.adapters[4].sys, [DeliverySystem::DvbT]);
    assert_eq!(d.a_count, 6);
    assert_eq!(d.netceivers.len(), 2);
}

#[test]
fn discovery_gives_up_after_twenty_polls() {
    let mut fake = FakeMcli::new();
    fake.lists = vec![vec![nc("a", &[FE_QPSK])]];
    let d = find_netcv_adapters(&mut fake, Some(&cfg(3)), 0, 100, |_| false).unwrap();
    let gets = fake
        .calls
        .iter()
        .filter(|c| **c == McliCall::GetList)
        .count();
    assert_eq!(gets, 20);
    assert_eq!(d.adapters.len(), 1);
}

#[test]
fn discovery_skips_disabled_ids_and_stops_at_max_adapters() {
    let mut fake = FakeMcli::new();
    fake.lists = vec![vec![nc("a", &[FE_QPSK, FE_QPSK, FE_QAM, FE_OFDM])]];
    let d = find_netcv_adapters(&mut fake, Some(&cfg(1)), 0, 3, |id| id == 1).unwrap();
    // Adapter 1 is disabled and consumes the second QPSK tuner; id 3 hits max_adapters.
    let types: Vec<(usize, i32)> = d.adapters.iter().map(|a| (a.id, a.fe_type)).collect();
    assert_eq!(types, [(0, FE_QPSK), (2, FE_QAM)]);
    assert_eq!(d.a_count, 3);
}

#[test]
fn discovery_atsc_and_unknown_types() {
    let mut fake = FakeMcli::new();
    fake.lists = vec![vec![nc("a", &[FE_ATSC, 9, -1])]];
    let d = find_netcv_adapters(&mut fake, Some(&cfg(1)), 0, 100, |_| false).unwrap();
    // ATSC gets an adapter with no delivery systems; out-of-range types are ignored.
    assert_eq!(d.adapters.len(), 1);
    assert_eq!(d.adapters[0].fe_type, FE_ATSC);
    assert!(d.adapters[0].sys.is_empty());
}
