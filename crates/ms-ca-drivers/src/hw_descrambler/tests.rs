use std::collections::BTreeSet;
use std::io;

use super::fake::{CaCall, RecordingCaDevice};
use super::*;
use crate::algo::{Algo, AlgoOp, AlgoRegistry, Backend};

type Hw = HwDescrambler<RecordingCaDevice>;

/// The C++ globals `a[]` and `pmts` the tests mutate.
#[derive(Default)]
struct Env {
    reg: HwRegistry,
}

impl Env {
    // setup_mock_adapter (test_hw_descrambler.cpp:26)
    fn setup_mock_adapter(&mut self, id: usize, pa: i32, fe: i32) -> HwAdapter {
        *self.reg.adapters.entry(id).or_insert(HwAdapter {
            id,
            pa,
            fe,
            enabled: true,
        })
    }

    fn cleanup_mock_adapter(&mut self, id: usize) {
        self.reg.adapters.remove(&id);
    }

    // pmt_add: the first free slot, enabled, no streams yet.
    fn pmt_add(&mut self, adapter: usize, _sid: u16, _pmt_pid: u16) -> usize {
        let id = (0..).find(|i| !self.reg.pmts.contains_key(i)).unwrap();
        self.reg.pmts.insert(
            id,
            HwPmt {
                id,
                adapter: Some(adapter),
                enabled: true,
                stream_pids: Vec::new(),
            },
        );
        id
    }

    fn pmt(&self, id: usize) -> HwPmt {
        self.reg.pmts[&id].clone()
    }

    fn pmt_add_stream_pid(&mut self, id: usize, pid: u16) {
        self.reg.pmts.get_mut(&id).unwrap().stream_pids.push(pid);
    }

    fn pmt_del(&mut self, id: usize) {
        self.reg.pmts.remove(&id);
    }
}

fn cw(id: i32, algo: Algo, parity: u8) -> HwCw {
    HwCw {
        id,
        algo,
        parity,
        ..HwCw::default()
    }
}

/// The C++ test binary runs without CA nodes; `open` fails with `ENOENT` like there.
fn no_hardware(enabled: bool) -> Hw {
    HwDescrambler::new(enabled, RecordingCaDevice::empty())
}

// Ported from tests/test_hw_descrambler.cpp:47
#[test]
fn test_hw_key_lifecycle() {
    let mut cw_csa = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut cw_csa);
    assert!(cw_csa.key.is_some(), "hw_create_key failed for DVBCSA");

    let mut cw_aes_ecb = cw(2, Algo::Aes128Ecb, 0);
    Hw::create_key(&mut cw_aes_ecb);
    assert!(
        cw_aes_ecb.key.is_some(),
        "hw_create_key failed for AES128_ECB"
    );

    let mut cw_aes_cbc = cw(3, Algo::Aes128Cbc, 0);
    Hw::create_key(&mut cw_aes_cbc);
    assert!(
        cw_aes_cbc.key.is_some(),
        "hw_create_key failed for AES128_CBC"
    );

    Hw::delete_key(&mut cw_csa);
    assert!(cw_csa.key.is_none(), "hw_delete_key failed for DVBCSA");
    Hw::delete_key(&mut cw_aes_ecb);
    assert!(
        cw_aes_ecb.key.is_none(),
        "hw_delete_key failed for AES128_ECB"
    );
    Hw::delete_key(&mut cw_aes_cbc);
    assert!(
        cw_aes_cbc.key.is_none(),
        "hw_delete_key failed for AES128_CBC"
    );
}

// Ported from tests/test_hw_descrambler.cpp:80
#[test]
fn test_hw_descrambler_disabled_when_opts_enigma_zero() {
    let mut hw = no_hardware(false);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);

    let pmt_id = env.pmt_add(0, 100, 1000);
    env.pmt_add_stream_pid(pmt_id, 1001);
    let pmt = env.pmt(pmt_id);

    let mut cw = cw(10, Algo::Csa, 0);
    cw.cw[..8].fill(0xAA);
    Hw::create_key(&mut cw);

    hw.set_cw(&mut cw, &pmt, &env.reg);

    let res = hw.del_pmt(&ad, &pmt, &env.reg);
    assert_eq!(res, 0, "hw_ca_del_pmt should return 0");

    Hw::delete_key(&mut cw);
    env.pmt_del(pmt_id);
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:112
#[test]
fn test_multi_channel_slot_allocation() {
    let mut hw = no_hardware(true);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);

    let pmt_id1 = env.pmt_add(0, 100, 1000);
    env.pmt_add_stream_pid(pmt_id1, 1001);
    env.pmt_add_stream_pid(pmt_id1, 1002);
    let pmt_id2 = env.pmt_add(0, 200, 2000);
    env.pmt_add_stream_pid(pmt_id2, 2001);
    env.pmt_add_stream_pid(pmt_id2, 2002);
    let pmt_id3 = env.pmt_add(0, 300, 3000);
    env.pmt_add_stream_pid(pmt_id3, 3001);
    let (pmt1, pmt2, pmt3) = (env.pmt(pmt_id1), env.pmt(pmt_id2), env.pmt(pmt_id3));

    let mut cw1_even = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut cw1_even);
    let mut cw1_odd = cw(2, Algo::Csa, 1);
    Hw::create_key(&mut cw1_odd);
    let mut cw2 = cw(3, Algo::Csa, 0);
    Hw::create_key(&mut cw2);
    let mut cw3 = cw(4, Algo::Aes128Cbc, 0);
    Hw::create_key(&mut cw3);

    hw.set_cw(&mut cw1_even, &pmt1, &env.reg);
    hw.set_cw(&mut cw1_odd, &pmt1, &env.reg);
    hw.set_cw(&mut cw2, &pmt2, &env.reg);
    hw.set_cw(&mut cw3, &pmt3, &env.reg);

    hw.del_pmt(&ad, &pmt2, &env.reg);

    let pmt_id4 = env.pmt_add(0, 400, 4000);
    env.pmt_add_stream_pid(pmt_id4, 4001);
    let pmt4 = env.pmt(pmt_id4);
    let mut cw4 = cw(5, Algo::Csa, 0);
    Hw::create_key(&mut cw4);
    hw.set_cw(&mut cw4, &pmt4, &env.reg);

    for cw in [&mut cw1_even, &mut cw1_odd, &mut cw2, &mut cw3, &mut cw4] {
        Hw::delete_key(cw);
    }
    hw.del_pmt(&ad, &pmt1, &env.reg);
    hw.del_pmt(&ad, &pmt3, &env.reg);
    hw.del_pmt(&ad, &pmt4, &env.reg);
    for id in [pmt_id1, pmt_id2, pmt_id3, pmt_id4] {
        env.pmt_del(id);
    }
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:206
#[test]
fn test_aes128_key_programming() {
    let mut hw = no_hardware(true);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);

    let pmt_id = env.pmt_add(0, 680, 6800);
    env.pmt_add_stream_pid(pmt_id, 6801);
    env.pmt_add_stream_pid(pmt_id, 6802);
    let pmt = env.pmt(pmt_id);

    let aes_key: [u8; 16] = [
        0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD, 0xEE,
        0xFF,
    ];
    let aes_iv: [u8; 16] = [
        0xFF, 0xEE, 0xDD, 0xCC, 0xBB, 0xAA, 0x99, 0x88, 0x77, 0x66, 0x55, 0x44, 0x33, 0x22, 0x11,
        0x00,
    ];

    let mut cw_ecb = cw(100, Algo::Aes128Ecb, 0);
    cw_ecb.cw = aes_key;
    Hw::create_key(&mut cw_ecb);
    hw.set_cw(&mut cw_ecb, &pmt, &env.reg);

    let mut cw_cbc = cw(101, Algo::Aes128Cbc, 1);
    cw_cbc.cw = aes_key;
    cw_cbc.iv = aes_iv;
    Hw::create_key(&mut cw_cbc);
    hw.set_cw(&mut cw_cbc, &pmt, &env.reg);

    Hw::delete_key(&mut cw_ecb);
    Hw::delete_key(&mut cw_cbc);
    hw.del_pmt(&ad, &pmt, &env.reg);
    env.pmt_del(pmt_id);
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:253
#[test]
fn test_hw_ca_close_dev_and_null_guards() {
    // Null SCW/SPMT pointers are unrepresentable; the nearest guards are a CW without a key
    // and deleting a key twice.
    let mut hw = no_hardware(false);
    let mut keyless = HwCw::default();
    Hw::delete_key(&mut keyless);
    hw.set_cw(&mut keyless, &HwPmt::default(), &HwRegistry::default());

    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);
    let res = hw.close_dev(&ad);
    assert_eq!(res, 0, "hw_ca_close_dev failed");
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:270
#[test]
fn test_csa_vs_csa_icam_routing() {
    // main() runs init_hw_descrambler with opts.hw_descrambler == 0, then init_algo_csa.
    let mut registry = AlgoRegistry::new();
    assert!(!no_hardware(false).register(&mut registry));
    registry.register(AlgoOp {
        algo: Algo::Csa,
        backend: Backend::Software,
    });
    let mut hw = no_hardware(true);

    let csa_op_res = registry.op_for_algo(Algo::Csa);
    assert!(
        csa_op_res.is_some(),
        "get_op_for_algo failed for CA_ALGO_DVBCSA"
    );
    assert_eq!(
        csa_op_res.unwrap().algo,
        Algo::Csa,
        "csa_op_res algo should be CA_ALGO_DVBCSA"
    );

    // ICAM algo is only registered when libdvbcsa has dvbcsa_bs_key_set_ecm
    if let Some(icam_op_res) = registry.op_for_algo(Algo::CsaIcam) {
        assert_eq!(
            icam_op_res.algo,
            Algo::CsaIcam,
            "icam_op_res algo should be CA_ALGO_DVBCSA_ICAM"
        );
        assert_ne!(
            Some(icam_op_res),
            csa_op_res,
            "CSA_ICAM op must be different from CSA op"
        );
    }

    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt_id = env.pmt_add(0, 500, 5000);
    let pmt = env.pmt(pmt_id);

    let mut cw_icam = cw(50, Algo::CsaIcam, 0);
    Hw::create_key(&mut cw_icam);
    hw.set_cw(&mut cw_icam, &pmt, &env.reg);

    Hw::delete_key(&mut cw_icam);
    env.pmt_del(pmt_id);
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:314
#[test]
fn test_hw_ca_update_retarget() {
    let mut hw = no_hardware(false);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);
    let saved = hw.enabled();
    hw.set_enabled(true);

    let pmt_id = env.pmt_add(0, 500, 5000);
    env.pmt_add_stream_pid(pmt_id, 1001);
    env.pmt_add_stream_pid(pmt_id, 1002);
    let pmt = env.pmt(pmt_id);
    assert_eq!(
        hw.add_pmt(&ad, &pmt, false, &env.reg),
        TABLES_RESULT_OK,
        "add must succeed"
    );
    assert_eq!(
        hw.bound_pids(0, pmt_id),
        BTreeSet::from([1001, 1002]),
        "add must track both pids"
    );

    env.reg
        .pmts
        .get_mut(&pmt_id)
        .unwrap()
        .stream_pids
        .retain(|&p| p != 1002);
    let pmt = env.pmt(pmt_id);
    assert_eq!(
        hw.add_pmt(&ad, &pmt, true, &env.reg),
        TABLES_RESULT_OK,
        "re-send must succeed"
    );
    assert_eq!(
        hw.bound_pids(0, pmt_id),
        BTreeSet::from([1001]),
        "re-send must drop 1002 from tracking"
    );
    assert_eq!(hw.del_pmt(&ad, &pmt, &env.reg), 0, "del must succeed");
    assert!(
        hw.bound_pids(0, pmt_id).is_empty(),
        "del must forget tracking"
    );

    hw.set_enabled(saved);
    env.pmt_del(pmt_id);
    env.cleanup_mock_adapter(0);
}

// Ported from tests/test_hw_descrambler.cpp:350
#[test]
fn test_hw_ca_hooks_succeed() {
    // Runs after test_csa_vs_csa_icam_routing in C++, so opts.hw_descrambler is still 1.
    let mut hw = no_hardware(true);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);

    let pmt_id = env.pmt_add(0, 500, 5000);
    let pmt = env.pmt(pmt_id);
    assert_eq!(
        hw.add_pmt(&ad, &pmt, false, &env.reg),
        TABLES_RESULT_OK,
        "hw_ca_add_pmt must succeed"
    );
    assert_eq!(
        hw.init_dev(&ad),
        TABLES_RESULT_OK,
        "hw_ca_init_dev must succeed"
    );

    env.pmt_del(pmt_id);
    env.cleanup_mock_adapter(0);
}

// ---- Beyond the C++ file: ioctl sequences through the recording fake. ----

const CA0: &str = "/dev/dvb/adapter0/ca0";

fn with_node(num: u32) -> Hw {
    HwDescrambler::new(true, RecordingCaDevice::with_node(CA0, num))
}

fn one_pmt(env: &mut Env, pids: &[u16]) -> HwPmt {
    let id = env.pmt_add(0, 1, 100);
    for &pid in pids {
        env.pmt_add_stream_pid(id, pid);
    }
    env.pmt(id)
}

fn fd(hw: &Hw) -> CaFd {
    hw.device()
        .calls
        .iter()
        .find_map(|c| match c {
            CaCall::Open(_, Ok(fd)) => Some(*fd),
            _ => None,
        })
        .unwrap()
}

#[test]
fn csa_sequence_is_mode_pids_then_cw() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x101, 0x102]);
    let mut even = cw(1, Algo::Csa, 0);
    even.cw[..8].copy_from_slice(&[1, 2, 3, 4, 5, 6, 7, 8]);
    Hw::create_key(&mut even);
    hw.set_cw(&mut even, &pmt, &env.reg);

    let fd = fd(&hw);
    assert_eq!(
        hw.device().calls,
        vec![
            CaCall::Open(CA0.into(), Ok(fd)),
            CaCall::GetDescrInfo(fd),
            CaCall::SetDescrMode(
                fd,
                CaDescrMode {
                    index: 0,
                    algo: 0,
                    cipher_mode: 0
                }
            ),
            CaCall::SetPid(
                fd,
                CaPid {
                    pid: 0x101,
                    index: 0
                }
            ),
            CaCall::SetPid(
                fd,
                CaPid {
                    pid: 0x102,
                    index: 0
                }
            ),
            CaCall::SetDescr(
                fd,
                CaDescr {
                    index: 0,
                    parity: 0,
                    cw: [1, 2, 3, 4, 5, 6, 7, 8]
                }
            ),
        ]
    );
    let key = even.key.unwrap();
    assert_eq!(key.ca_fd, Some(fd));
    assert_eq!(
        (key.adapter_id, key.pmt_id, key.slot_index),
        (0, Some(pmt.id), 0)
    );
    assert_eq!(key.num_descramblers, 4);
}

#[test]
fn aes_ecb_programs_key_only_and_cbc_adds_iv() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[]);
    let mut ecb = cw(1, Algo::Aes128Ecb, 1);
    ecb.cw = [0x11; 16];
    Hw::create_key(&mut ecb);
    hw.set_cw(&mut ecb, &pmt, &env.reg);
    let mut cbc = cw(2, Algo::Aes128Cbc, 0);
    cbc.cw = [0x22; 16];
    cbc.iv = [0x33; 16];
    Hw::create_key(&mut cbc);
    hw.set_cw(&mut cbc, &pmt, &env.reg);

    let fd = fd(&hw);
    let data = |parity, data_type, byte| CaDescrData {
        index: 0,
        parity,
        data_type,
        data_len: 16,
        data: [byte; 16],
    };
    assert_eq!(
        hw.device().set_calls(),
        vec![
            CaCall::SetDescrMode(
                fd,
                CaDescrMode {
                    index: 0,
                    algo: 2,
                    cipher_mode: 0
                }
            ),
            CaCall::SetDescrData(fd, data(1, DATA_TYPE_CW, 0x11)),
            CaCall::SetDescrMode(
                fd,
                CaDescrMode {
                    index: 0,
                    algo: 3,
                    cipher_mode: 1
                }
            ),
            CaCall::SetDescrData(fd, data(0, DATA_TYPE_CW, 0x22)),
            CaCall::SetDescrData(fd, data(0, DATA_TYPE_IV, 0x33)),
        ]
    );
}

#[test]
fn des_goes_through_the_16_byte_data_path() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[]);
    let mut des = cw(1, Algo::Des, 0);
    Hw::create_key(&mut des);
    hw.set_cw(&mut des, &pmt, &env.reg);
    let calls = hw.device().set_calls();
    assert_eq!(calls.len(), 2);
    assert!(matches!(calls[1], CaCall::SetDescrData(_, d) if d.data_type == DATA_TYPE_CW));
}

#[test]
fn missing_node_falls_back_to_dev_ci_only_on_enoent() {
    let mut dev = RecordingCaDevice::with_node("/dev/ci2", 8);
    dev.descr_info = None;
    let mut hw = HwDescrambler::new(true, dev);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 2, 1);
    let pmt = one_pmt(&mut env, &[]);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    let calls = &hw.device().calls;
    assert_eq!(
        calls[0],
        CaCall::Open("/dev/dvb/adapter2/ca1".into(), Err(io::ErrorKind::NotFound))
    );
    assert!(matches!(&calls[1], CaCall::Open(p, Ok(_)) if p == "/dev/ci2"));
    // CA_GET_DESCR_INFO failed: the default of 16 slots applies.
    assert_eq!(c.key.unwrap().num_descramblers, DEFAULT_MAX_DESCRAMBLERS);

    let mut dev = RecordingCaDevice::with_node("/dev/ci0", 8);
    dev.open_errors
        .insert(CA0.into(), io::ErrorKind::PermissionDenied);
    let mut hw = HwDescrambler::new(true, dev);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x100]);
    hw.set_cw(&mut c, &pmt, &env.reg);
    assert_eq!(
        hw.device().calls,
        vec![CaCall::Open(
            CA0.into(),
            Err(io::ErrorKind::PermissionDenied)
        )]
    );
    assert_eq!(c.key.unwrap().slot_index, -1);
}

#[test]
fn failed_open_is_retried_on_the_next_cw() {
    let mut hw = no_hardware(true);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x100]);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    hw.set_cw(&mut c, &pmt, &env.reg);
    // ca0 then /dev/ci0, twice, and nothing else.
    assert_eq!(hw.device().calls.len(), 4);
    assert!(hw.device().set_calls().is_empty());
}

#[test]
fn slot_exhaustion_skips_programming() {
    let mut hw = with_node(2);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmts: Vec<HwPmt> = (0..3).map(|_| one_pmt(&mut env, &[0x200])).collect();
    let mut keys: Vec<HwCw> = (0..3).map(|i| cw(i, Algo::Csa, 0)).collect();
    for (c, pmt) in keys.iter_mut().zip(&pmts) {
        Hw::create_key(c);
        hw.set_cw(c, pmt, &env.reg);
    }
    let slots: Vec<i32> = keys
        .iter()
        .map(|c| c.key.as_ref().unwrap().slot_index)
        .collect();
    assert_eq!(slots, [0, 1, -1]);
    // Two PMTs programmed (mode, pid, cw each); the third issued nothing.
    assert_eq!(hw.device().set_calls().len(), 6);

    // Freeing PMT 0 lets the third take slot 0.
    let ad = env.reg.adapters[&0];
    hw.del_pmt(&ad, &pmts[0], &env.reg);
    hw.set_cw(&mut keys[2], &pmts[2], &env.reg);
    assert_eq!(keys[2].key.as_ref().unwrap().slot_index, 0);
    assert_eq!(hw.slot_of(0, pmts[2].id), Some(0));
}

#[test]
fn slot_count_defaults_and_caps() {
    for (num, expected) in [(0, 16), (u32::MAX, 16), (1000, 64)] {
        let mut hw = with_node(num);
        let mut env = Env::default();
        env.setup_mock_adapter(0, 0, 0);
        let mut last = -1;
        for i in 0..70 {
            let pmt = one_pmt(&mut env, &[]);
            let mut c = cw(i, Algo::Csa, 0);
            Hw::create_key(&mut c);
            hw.set_cw(&mut c, &pmt, &env.reg);
            let slot = c.key.unwrap().slot_index;
            if slot >= 0 {
                last = slot;
            }
        }
        assert_eq!(last, expected - 1, "num={num}");
    }
}

#[test]
fn even_and_odd_updates_reuse_the_slot_in_order() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let other = one_pmt(&mut env, &[0x300]);
    let pmt = one_pmt(&mut env, &[0x400]);
    let mut first = cw(9, Algo::Csa, 0);
    Hw::create_key(&mut first);
    hw.set_cw(&mut first, &other, &env.reg);

    let mut even = cw(1, Algo::Csa, 0);
    let mut odd = cw(2, Algo::Csa, 1);
    Hw::create_key(&mut even);
    Hw::create_key(&mut odd);
    let before = hw.device().set_calls().len();
    for round in 0..2u8 {
        even.cw[0] = round;
        odd.cw[0] = 0x80 | round;
        hw.set_cw(&mut even, &pmt, &env.reg);
        hw.set_cw(&mut odd, &pmt, &env.reg);
    }
    let descrs: Vec<(u32, u32, u8)> = hw.device().set_calls()[before..]
        .iter()
        .filter_map(|c| match c {
            CaCall::SetDescr(_, d) => Some((d.index, d.parity, d.cw[0])),
            _ => None,
        })
        .collect();
    assert_eq!(descrs, [(1, 0, 0), (1, 1, 0x80), (1, 0, 1), (1, 1, 0x81)]);
    // The device is opened once and shared by every PMT.
    let opens = hw
        .device()
        .calls
        .iter()
        .filter(|c| matches!(c, CaCall::Open(..)))
        .count();
    assert_eq!(opens, 1);
}

#[test]
fn icam_and_disabled_issue_no_ioctls() {
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x100]);
    let mut hw = with_node(4);
    let mut icam = cw(1, Algo::CsaIcam, 0);
    Hw::create_key(&mut icam);
    hw.set_cw(&mut icam, &pmt, &env.reg);
    assert!(hw.device().calls.is_empty());

    let mut hw = HwDescrambler::new(false, RecordingCaDevice::with_node(CA0, 4));
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    let ad = env.reg.adapters[&0];
    hw.add_pmt(&ad, &pmt, false, &env.reg);
    hw.del_pmt(&ad, &pmt, &env.reg);
    assert!(hw.device().calls.is_empty());
    assert!(hw.bound_pids(0, pmt.id).is_empty());
}

#[test]
fn disabled_adapter_or_missing_adapter_skips_set_cw() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let mut pmt = one_pmt(&mut env, &[0x100]);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    env.reg.adapters.get_mut(&0).unwrap().enabled = false;
    hw.set_cw(&mut c, &pmt, &env.reg);
    pmt.adapter = None;
    hw.set_cw(&mut c, &pmt, &env.reg);
    assert!(hw.device().calls.is_empty());
}

#[test]
fn update_unbinds_dropped_pids_on_the_device() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);
    let mut pmt = one_pmt(&mut env, &[0x100, 0x101, 0x102]);
    hw.add_pmt(&ad, &pmt, false, &env.reg);
    assert!(
        hw.device().calls.is_empty(),
        "nothing removed, nothing opened"
    );
    pmt.stream_pids = vec![0x100];
    hw.add_pmt(&ad, &pmt, true, &env.reg);
    let fd = fd(&hw);
    assert_eq!(
        hw.device().set_calls(),
        vec![
            CaCall::SetPid(
                fd,
                CaPid {
                    pid: 0x101,
                    index: -1
                }
            ),
            CaCall::SetPid(
                fd,
                CaPid {
                    pid: 0x102,
                    index: -1
                }
            ),
        ]
    );
}

#[test]
fn del_keeps_pids_held_by_other_pmts_on_the_same_hardware() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    let ad0 = env.setup_mock_adapter(0, 0, 0);
    // Logical adapter 1 shares physical adapter 0; adapter 2 is another card.
    env.setup_mock_adapter(1, 0, 1);
    env.setup_mock_adapter(2, 1, 0);
    let pmt = one_pmt(&mut env, &[0x100, 0x101, 0x102, 0x103]);
    let shared = env.pmt_add(1, 2, 200);
    env.pmt_add_stream_pid(shared, 0x101);
    let elsewhere = env.pmt_add(2, 3, 300);
    env.pmt_add_stream_pid(elsewhere, 0x102);
    let disabled = env.pmt_add(1, 4, 400);
    env.pmt_add_stream_pid(disabled, 0x103);
    env.reg.pmts.get_mut(&disabled).unwrap().enabled = false;

    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    assert_eq!(hw.slot_of(0, pmt.id), Some(0));
    let before = hw.device().set_calls().len();
    assert_eq!(hw.del_pmt(&ad0, &pmt, &env.reg), 0);
    let fd = fd(&hw);
    let unbinds: Vec<CaCall> = hw.device().set_calls()[before..].to_vec();
    let unbind = |pid| CaCall::SetPid(fd, CaPid { pid, index: -1 });
    assert_eq!(unbinds, vec![unbind(0x100), unbind(0x102), unbind(0x103)]);
    assert_eq!(hw.slot_of(0, pmt.id), None);
}

#[test]
fn del_unbinds_pids_dropped_by_earlier_updates() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);
    let mut pmt = one_pmt(&mut env, &[0x100]);
    hw.add_pmt(&ad, &pmt, false, &env.reg);
    pmt.stream_pids = vec![0x200];
    // An update the tracker sees, then a PMT whose live list no longer has 0x200.
    hw.add_pmt(&ad, &pmt, true, &env.reg);
    pmt.stream_pids.clear();
    let before = hw.device().set_calls().len();
    hw.del_pmt(&ad, &pmt, &env.reg);
    let fd = fd(&hw);
    assert_eq!(
        hw.device().set_calls()[before..],
        [CaCall::SetPid(
            fd,
            CaPid {
                pid: 0x200,
                index: -1
            }
        )]
    );
}

#[test]
fn close_dev_closes_and_resets_then_reopens() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    let ad = env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x100]);
    hw.add_pmt(&ad, &pmt, false, &env.reg);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    let first = fd(&hw);
    assert_eq!(hw.close_dev(&ad), 0);
    assert_eq!(hw.device().calls.last(), Some(&CaCall::Close(first)));
    assert_eq!(hw.slot_of(0, pmt.id), None);
    assert!(hw.bound_pids(0, pmt.id).is_empty());
    // A second close has no fd left to close.
    let n = hw.device().calls.len();
    hw.close_dev(&ad);
    assert_eq!(hw.device().calls.len(), n);

    hw.set_cw(&mut c, &pmt, &env.reg);
    let opens = hw
        .device()
        .calls
        .iter()
        .filter(|c| matches!(c, CaCall::Open(..)))
        .count();
    assert_eq!(opens, 2);
}

#[test]
fn failing_ioctls_do_not_stop_the_sequence() {
    let mut dev = RecordingCaDevice::with_node(CA0, 4);
    dev.fail_set = true;
    let mut hw = HwDescrambler::new(true, dev);
    let mut env = Env::default();
    env.setup_mock_adapter(0, 0, 0);
    let pmt = one_pmt(&mut env, &[0x100, 0x101]);
    let mut c = cw(1, Algo::Aes128Cbc, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    // mode, two pids, key, iv
    assert_eq!(hw.device().set_calls().len(), 5);
}

#[test]
fn out_of_range_physical_adapter_is_ignored() {
    let mut hw = with_node(4);
    let mut env = Env::default();
    env.setup_mock_adapter(0, MAX_ADAPTERS, 0);
    let pmt = one_pmt(&mut env, &[0x100]);
    let mut c = cw(1, Algo::Csa, 0);
    Hw::create_key(&mut c);
    hw.set_cw(&mut c, &pmt, &env.reg);
    assert!(hw.device().calls.is_empty());
}

#[test]
fn register_adds_hardware_ops_before_software() {
    let mut registry = AlgoRegistry::new();
    assert!(with_node(4).register(&mut registry));
    registry.register(AlgoOp {
        algo: Algo::Csa,
        backend: Backend::Software,
    });
    assert_eq!(
        registry.op_for_algo(Algo::Csa).unwrap().backend,
        Backend::Hardware
    );
    assert!(registry.op_for_algo(Algo::Aes128Ecb).is_some());
    assert!(registry.op_for_algo(Algo::Aes128Cbc).is_some());
    assert!(registry.op_for_algo(Algo::Des).is_none());
    assert!(registry.op_for_algo(Algo::CsaIcam).is_none());
}
