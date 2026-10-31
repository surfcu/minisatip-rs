//! Ports of the PMT claim/handover tests in `tests/test_pmt.cpp`.
//!
//! The scheduler stands in for the C++ `adapter` and the fake CA stands in
//! for `SCA_op`; section builders mirror the upstream test helpers byte for
//! byte, except that the CRC is left zeroed because the parser under test
//! uses `CrcCheck::Skip` (see `docs/deviations.md`).

use std::cell::RefCell;
use std::rc::Rc;

use ms_ca::CaService;
use ms_ca::TablesResult;
use ms_tuner::{DeliverySystem, TuneParams};

use super::*;
use crate::{
    AdapterConfig, AdapterTuner, MAX_PMT_FOR_ADAPTER, PID_STREAM_ID_UNDEFINED, PidState, TuneError,
};

const PAT_TSID: u16 = 40901;
const SID_KEPT: i32 = 100;
const PID_KEPT: i32 = 1000;
const SID_GONE: i32 = 200;
const PID_GONE: i32 = 2000;

struct NoTune;

impl AdapterTuner for NoTune {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

fn test_sched(kind: AdapterType) -> Scheduler {
    let mut config = AdapterConfig::new(DeliverySystem::DvbS2);
    config.adapter_type = kind;
    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(0, config, Box::new(NoTune));
    scheduler
}

/// Fills one PID slot the way the C++ tests write `ad.pids[i]` directly.
fn set_pid_slot(sched: &mut Scheduler, slot: usize, pid: i32, sids: &[StreamId], order: u32) {
    let entry = &mut sched.adapter_mut(0).unwrap().pids[slot];
    entry.pid = pid;
    entry.pmt = -1;
    entry.filter = -1;
    entry.state = PidState::Active;
    entry.order = order;
    entry.sids = sids.iter().copied().collect();
}

fn pmt_state(sched: &Scheduler, id: i32) -> PmtState {
    sched.pmt_slot(id).unwrap().state
}

fn pmt_ca_mask(sched: &Scheduler, id: i32) -> u64 {
    sched.pmt_slot(id).unwrap().ca_mask
}

fn pmt_registered_mask(sched: &Scheduler, id: i32) -> u64 {
    sched.pmt_slot(id).unwrap().ca_registered_mask
}

fn pmt_version(sched: &Scheduler, id: i32) -> i32 {
    sched.pmt_slot(id).unwrap().version
}

fn pmt_best(sched: &Scheduler, id: i32) -> bool {
    sched.pmt_slot(id).unwrap().best
}

/// Claim owner of `pid`, or `-1` (`find_pid(0, pid)->pmt`).
fn claim(sched: &Scheduler, pid: i32) -> i32 {
    sched.find_pid(0, pid).map_or(-1, |entry| entry.pmt)
}

fn pid_order(sched: &Scheduler, pid: i32) -> u32 {
    sched.find_pid(0, pid).unwrap().order
}

#[derive(Debug)]
struct CaLog {
    add_calls: i32,
    del_calls: i32,
    last_add_pmt: i32,
    last_del_pmt: i32,
    last_add_update: i32,
    fail_next: bool,
    single_slot: bool,
    holder: i32,
    seq: i32,
    add_seq: i32,
    del_seq: i32,
    failed_adds: i32,
}

impl Default for CaLog {
    fn default() -> Self {
        Self {
            add_calls: 0,
            del_calls: 0,
            last_add_pmt: -1,
            last_del_pmt: -1,
            last_add_update: -1,
            fail_next: false,
            single_slot: false,
            holder: -1,
            seq: 0,
            add_seq: 0,
            del_seq: 0,
            failed_adds: 0,
        }
    }
}

impl CaLog {
    fn reset(&mut self) {
        self.add_calls = 0;
        self.del_calls = 0;
        self.last_add_pmt = -1;
        self.last_del_pmt = -1;
        self.last_add_update = -1;
    }
}

struct FakeCa(Rc<RefCell<CaLog>>);

impl CaSink for FakeCa {
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let mut log = self.0.borrow_mut();
        log.add_calls += 1;
        log.last_add_pmt = service.pmt_id as i32;
        log.last_add_update = i32::from(update);
        if log.fail_next {
            log.fail_next = false;
            return TablesResult::NoRetry;
        }
        let id = service.pmt_id as i32;
        if log.single_slot {
            if log.holder != -1 && log.holder != id {
                log.failed_adds += 1;
                return TablesResult::Retry;
            }
            log.holder = id;
            log.seq += 1;
            log.add_seq = log.seq;
        }
        TablesResult::Ok
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        let mut log = self.0.borrow_mut();
        log.del_calls += 1;
        log.last_del_pmt = service.pmt_id as i32;
        let id = service.pmt_id as i32;
        if log.single_slot && log.holder == id {
            log.holder = -1;
            log.seq += 1;
            log.del_seq = log.seq;
        }
        TablesResult::Ok
    }
}

fn register_ca(sched: &mut Scheduler) -> (i32, Rc<RefCell<CaLog>>) {
    let log = Rc::new(RefCell::new(CaLog::default()));
    let id = sched.add_ca(Box::new(FakeCa(log.clone())));
    sched.adapter_mut(0).unwrap().ca_mask = 1u64 << id;
    (id, log)
}

/// `build_pat`: PAT with `entries` (sid, PMT pid) and a zeroed CRC.
fn build_pat(tsid: u16, version: u8, entries: &[(i32, i32)]) -> Vec<u8> {
    let len = 8 + 4 * entries.len() + 4;
    let mut b = vec![0u8; len];
    b[0] = 0;
    let body = (len - 3) as u16;
    b[1] = 0xb0 | ((body >> 8) as u8 & 0x0f);
    b[2] = body as u8;
    b[3] = (tsid >> 8) as u8;
    b[4] = tsid as u8;
    b[5] = 0xc1 | ((version & 0x1f) << 1);
    for (index, &(sid, pid)) in entries.iter().enumerate() {
        let offset = 8 + 4 * index;
        b[offset] = (sid >> 8) as u8;
        b[offset + 1] = sid as u8;
        b[offset + 2] = 0xe0 | ((pid >> 8) as u8 & 0x1f);
        b[offset + 3] = pid as u8;
    }
    b
}

/// `build_pmt`: optional program CA descriptor plus one ES per pair.
fn build_pmt(
    sid: i32,
    version: u8,
    pcr: i32,
    caid: u16,
    ecm: u16,
    types: &[i32],
    pids: &[i32],
) -> Vec<u8> {
    let pi_len = if caid != 0 { 6 } else { 0 };
    let len = 12 + pi_len + 5 * types.len() + 4;
    let mut b = vec![0u8; len];
    b[0] = 2;
    let body = (len - 3) as u16;
    b[1] = 0xb0 | ((body >> 8) as u8 & 0x0f);
    b[2] = body as u8;
    b[3] = (sid >> 8) as u8;
    b[4] = sid as u8;
    b[5] = 0xc1 | ((version & 0x1f) << 1);
    b[8] = 0xe0 | ((pcr >> 8) as u8 & 0x1f);
    b[9] = pcr as u8;
    b[10] = 0xf0 | ((pi_len >> 8) as u8 & 0x0f);
    b[11] = pi_len as u8;
    if pi_len > 0 {
        b[12] = 9;
        b[13] = 4;
        b[14] = (caid >> 8) as u8;
        b[15] = caid as u8;
        b[16] = 0xe0 | ((ecm >> 8) as u8 & 0x1f);
        b[17] = ecm as u8;
    }
    for (index, (&stream_type, &pid)) in types.iter().zip(pids).enumerate() {
        let offset = 12 + pi_len + 5 * index;
        b[offset] = stream_type as u8;
        b[offset + 1] = 0xe0 | ((pid >> 8) as u8 & 0x1f);
        b[offset + 2] = pid as u8;
        b[offset + 3] = 0xf0;
        b[offset + 4] = 0;
    }
    b
}

fn carries_ecm(pmt: &Pmt, ecm: u16) -> bool {
    let has_ecm = |descriptors: &[Descriptor]| {
        descriptors
            .iter()
            .any(|descriptor| descriptor.tag == 9 && descriptor.ca_pid() == Some(ecm))
    };
    has_ecm(&pmt.descriptors)
        || pmt
            .stream_pids
            .iter()
            .any(|stream| has_ecm(&stream.descriptors))
}

// Ported from tests/test_pmt.cpp:566
fn check_pat_drop_releases_ca(adapter_type: AdapterType) {
    let mut sched = test_sched(adapter_type);
    let (_ica, log) = register_ca(&mut sched);
    let pat_filter = sched.add_filter(0, 0, FilterTarget::Pat, FILTER_PERMANENT | FILTER_CRC);
    assert!(pat_filter >= 0, "could not add the PAT filter");

    let kept = sched.pmt_add(0, SID_KEPT, PID_KEPT);
    let gone = sched.pmt_add(0, SID_GONE, PID_GONE);
    assert!(kept >= 0 && gone >= 0, "could not create the PMTs");
    sched.pmt_add_caid(kept, 0x0664, 0x1F06, &[]);
    sched.pmt_add_caid(gone, 0x0664, 0x1F08, &[]);
    sched.send_pmt_to_cas(0, kept);
    sched.send_pmt_to_cas(0, gone);
    assert_ne!(
        pmt_ca_mask(&sched, kept),
        0,
        "kept PMT was not registered with the CA"
    );
    assert_ne!(
        pmt_ca_mask(&sched, gone),
        0,
        "gone PMT was not registered with the CA"
    );

    sched.pmt_slot_mut(kept).unwrap().state = PmtState::Running;
    sched.pmt_slot_mut(gone).unwrap().state = PmtState::Running;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = kept;
        adapter.active_pmt[1] = gone;
    }
    sched.pmt_add_stream_pid(kept, 1101, 2, false, true);
    sched.pmt_add_stream_pid(kept, 1201, 3, true, false);
    assert_eq!(sched.mark_pid_add(0, 0, PID_KEPT), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 1101), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 1201), 0);

    log.borrow_mut().reset();
    let pat = build_pat(PAT_TSID, 1, &[(SID_KEPT, PID_KEPT)]);
    sched.process_pat(pat_filter, &pat);

    assert_eq!(
        log.borrow().del_calls,
        1,
        "the PMT missing from the PAT was not closed on the CA"
    );
    assert_eq!(
        log.borrow().last_del_pmt,
        gone,
        "the wrong PMT was closed on the CA"
    );
    assert_eq!(
        pmt_ca_mask(&sched, gone),
        0,
        "the CA registration of the dropped PMT was not released"
    );
    assert!(
        sched.pmt_slot(kept).unwrap().enabled,
        "the PMT still present in the PAT was retired"
    );
    assert_ne!(
        pmt_ca_mask(&sched, kept),
        0,
        "the PMT still present in the PAT lost its CA registration"
    );
    if adapter_type == AdapterType::Ci {
        assert!(
            !sched.pmt_slot(gone).unwrap().enabled,
            "the dropped PMT was not deleted on a CI adapter"
        );
    } else {
        assert_eq!(
            pmt_state(&sched, gone),
            PmtState::Cached,
            "the dropped PMT was not cached"
        );
    }
    sched.free_all_pmts();
}

#[test]
fn pat_drop_releases_ca() {
    check_pat_drop_releases_ca(AdapterType::Dvb);
    check_pat_drop_releases_ca(AdapterType::Ci);
}

// Ported from tests/test_pmt.cpp:574
#[test]
fn revived_cached_pmt_gets_its_filter() {
    let mut sched = test_sched(AdapterType::Dvb);
    let pat_filter = sched.add_filter(0, 0, FilterTarget::Pat, FILTER_PERMANENT | FILTER_CRC);

    let id = sched.pmt_add(0, SID_KEPT, PID_KEPT);
    assert!(id >= 0, "could not create the PMT");
    {
        let pmt = sched.pmt_slot_mut(id).unwrap();
        pmt.version = 7;
        pmt.state = PmtState::Running;
        pmt.stream_pids.push(StreamPid {
            stream_type: 2,
            pid: 300,
            is_audio: false,
            is_video: true,
            descriptors: Vec::new(),
        });
    }

    set_pid_slot(&mut sched, 0, 300, &[0], 2);
    set_pid_slot(&mut sched, 1, PID_KEPT, &[0], 1);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }

    sched.cache_pmt_for_adapter(0, id);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Cached,
        "PMT should be cached"
    );
    assert_eq!(
        sched.pmt_slot(id).unwrap().filter,
        -1,
        "the cached PMT should have no filter"
    );

    let pat = build_pat(PAT_TSID, 1, &[(SID_KEPT, PID_KEPT)]);
    sched.process_pat(pat_filter, &pat);

    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "the revived PMT should start straight from the cache"
    );
    let filter_id = sched.pmt_slot(id).unwrap().filter;
    assert!(
        filter_id >= 0,
        "process_pat() should hand the new filter to the revived PMT"
    );
    let filter = sched.filter_slot(filter_id).unwrap();
    assert_eq!(
        filter.pid, PID_KEPT,
        "the filter should be the one for the PMT pid"
    );
    assert_eq!(
        filter.target,
        FilterTarget::Pmt(Some(id as usize)),
        "the filter should belong to this PMT"
    );

    let before = filter_id;
    let pat = build_pat(PAT_TSID, 2, &[(SID_KEPT, PID_KEPT)]);
    sched.process_pat(pat_filter, &pat);
    assert_eq!(
        sched.pmt_slot(id).unwrap().filter,
        before,
        "a second PAT should reuse the filter of the revived PMT"
    );

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "the revived PMT should stay up"
    );
    assert_ne!(
        sched.filter_slot(filter_id).unwrap().flags,
        0,
        "starting the PMT should have enabled its filter"
    );

    sched.del_filter(pat_filter);
    sched.free_all_pmts();
    sched.free_filters();
}

// Ported from tests/test_pmt.cpp:656
#[test]
fn pmt_starts_only_with_pmt_pid() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    assert!(aid >= 0 && bid >= 0, "could not create the PMTs");
    for id in [aid, bid] {
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }
    set_pid_slot(&mut sched, 0, 52, &[0], 1);
    set_pid_slot(&mut sched, 1, 3301, &[0], 2);
    set_pid_slot(&mut sched, 2, 3401, &[0], 3);

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "wanted PMT should run"
    );
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "sibling PMT should wait"
    );
    assert_eq!(claim(&sched, 3301), bid, "video pid should be claimed by B");
    assert_eq!(claim(&sched, 3401), bid, "audio pid should be claimed by B");

    {
        let entry = sched.find_pid_mut(0, 52).unwrap();
        entry.sids.clear();
        entry.order = 0;
    }
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, bid), PmtState::Running, "PMT should stay");
    assert_eq!(claim(&sched, 3301), bid, "video claim should be kept");
    assert_eq!(claim(&sched, 3401), bid, "audio claim should be kept");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:733
#[test]
fn retune_handover_same_loop() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    for id in [aid, bid] {
        sched.pmt_add_caid(id, 0x0664, 0x1F06, &[]);
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }
    set_pid_slot(&mut sched, 0, 48, &[0], 1);
    set_pid_slot(&mut sched, 1, 3301, &[0], 2);
    set_pid_slot(&mut sched, 2, 3401, &[0], 3);

    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run first"
    );
    assert_eq!(log.borrow().add_calls, 1);
    assert_eq!(log.borrow().last_add_pmt, aid, "A should be sent to the CA");

    {
        let entry = sched.find_pid_mut(0, 48).unwrap();
        entry.sids.clear();
        entry.order = 0;
    }
    set_pid_slot(&mut sched, 3, 52, &[0], 4);
    log.borrow_mut().reset();
    sched.start_active_pmts(0);

    assert_eq!(pmt_state(&sched, aid), PmtState::Stopped, "A should stop");
    assert_eq!(pmt_ca_mask(&sched, aid), 0, "A should release its CA slot");
    assert_eq!(pmt_state(&sched, bid), PmtState::Running, "B should run");
    assert_ne!(pmt_ca_mask(&sched, bid), 0, "B should hold a CA slot");
    assert_eq!(log.borrow().del_calls, 1);
    assert_eq!(
        log.borrow().last_del_pmt,
        aid,
        "A should be closed on the CA"
    );
    assert_eq!(log.borrow().add_calls, 1);
    assert_eq!(log.borrow().last_add_pmt, bid, "B should be sent to the CA");
    assert_eq!(claim(&sched, 3301), bid, "video pid should move to B");

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:837
#[test]
fn single_slot_retune_releases_first() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    for id in [aid, bid] {
        sched.pmt_add_caid(id, 0x0664, 0x1F06, &[]);
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }
    set_pid_slot(&mut sched, 0, 48, &[0], 1);
    set_pid_slot(&mut sched, 1, 3301, &[0], 2);
    set_pid_slot(&mut sched, 2, 3401, &[0], 3);

    {
        let mut state = log.borrow_mut();
        state.single_slot = true;
        state.holder = -1;
        state.seq = 0;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run first"
    );
    assert_eq!(log.borrow().holder, aid, "A should hold the slot");

    {
        let entry = sched.find_pid_mut(0, 48).unwrap();
        entry.sids.clear();
        entry.order = 0;
    }
    set_pid_slot(&mut sched, 3, 52, &[0], 4);
    {
        let mut state = log.borrow_mut();
        state.seq = 0;
        state.add_seq = 0;
        state.del_seq = 0;
        state.failed_adds = 0;
    }
    sched.start_active_pmts(0);

    assert_eq!(pmt_state(&sched, aid), PmtState::Stopped, "A should stop");
    assert_eq!(pmt_state(&sched, bid), PmtState::Running, "B should run");
    assert_ne!(pmt_ca_mask(&sched, bid), 0, "B should hold a CA slot");
    assert_eq!(log.borrow().holder, bid, "B should hold the slot");
    assert_eq!(log.borrow().failed_adds, 0, "B must not see a full slot");
    let state = log.borrow();
    assert!(
        state.del_seq > 0 && state.add_seq > 0 && state.del_seq < state.add_seq,
        "the release must come before the acquire"
    );
    drop(state);

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:915
#[test]
fn scan_pmt_only_starts_nothing() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    assert!(aid >= 0 && bid >= 0, "could not create the PMTs");
    for id in [aid, bid] {
        sched.pmt_add_caid(id, 0x0664, 0x1F06, &[]);
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }
    set_pid_slot(&mut sched, 0, 48, &[0], 1);
    set_pid_slot(&mut sched, 1, 52, &[0], 2);

    log.borrow_mut().add_calls = 0;
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "A must not run on scan"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Stopped,
        "B must not run on scan"
    );
    assert_eq!(log.borrow().add_calls, 0, "nothing must be sent to the CA");

    set_pid_slot(&mut sched, 2, 3301, &[0], 3);
    set_pid_slot(&mut sched, 3, 3401, &[0], 4);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "earlier PMT should run"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Stopped,
        "later PMT should wait"
    );

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:987
#[test]
fn active_pmt_list_capped_at_max() {
    let mut sched = test_sched(AdapterType::Dvb);
    let mut ids = Vec::new();
    for index in 0..=MAX_PMT_FOR_ADAPTER {
        ids.push(sched.pmt_add(0, 1000 + index as i32, 4000 + index as i32));
    }
    for id in ids.iter().take(MAX_PMT_FOR_ADAPTER) {
        sched.pmt_add_active_pmt(0, *id);
    }
    assert_eq!(
        sched.adapter(0).unwrap().active_pmts,
        MAX_PMT_FOR_ADAPTER,
        "active list must fill up"
    );

    sched.pmt_add_active_pmt(0, ids[MAX_PMT_FOR_ADAPTER]);
    assert_eq!(
        sched.adapter(0).unwrap().active_pmts,
        MAX_PMT_FOR_ADAPTER,
        "full active list must refuse"
    );

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1012
#[test]
fn test_19e_11493h_zap_flows() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 10303, 5120);
    let bid = sched.pmt_add(0, 10304, 5130);
    assert!(aid >= 0 && bid >= 0, "could not create the PMTs");
    for id in [aid, bid] {
        sched.pmt_add_stream_pid(id, 5121, 27, false, true);
        sched.pmt_add_stream_pid(id, 5122, 3, true, false);
    }
    sched.pmt_add_stream_pid(aid, 5124, 6, false, false);
    sched.pmt_add_stream_pid(bid, 5134, 6, false, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 5120), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5121), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5122), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run first"
    );
    assert_eq!(claim(&sched, 5121), aid, "A should own video");

    sched.mark_pid_deleted(0, 0, Some(5120));
    assert_eq!(sched.mark_pid_add(0, 0, 5130), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "zap should run B"
    );
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "zap should stop A"
    );
    assert_eq!(claim(&sched, 5121), bid, "video should move to B");

    sched.mark_pid_deleted(0, 0, Some(5130));
    assert_eq!(sched.mark_pid_add(0, 0, 5120), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "zap back should run A"
    );
    assert_eq!(claim(&sched, 5121), aid, "video should move back to A");

    sched.mark_pid_deleted(0, 0, Some(5120));
    sched.mark_pid_deleted(0, 0, Some(5121));
    sched.mark_pid_deleted(0, 0, Some(5122));
    assert_eq!(sched.mark_pid_add(0, 0, 5124), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "data-only runs nothing"
    );

    assert_eq!(sched.mark_pid_add(0, 0, 5121), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5122), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "AV-only runs nothing"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Stopped,
        "AV-only runs nothing"
    );

    assert_eq!(sched.mark_pid_add(0, 0, 5120), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "PMT-after-AV runs A"
    );
    assert_eq!(claim(&sched, 5124), aid, "data is marked with its owner");

    assert_eq!(sched.mark_pid_add(0, 0, 5130), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "first keeps running"
    );
    assert_eq!(pmt_state(&sched, bid), PmtState::Stopped, "second waits");
    assert!(pmt_best(&sched, bid), "waiter stays in the set");
    assert_eq!(claim(&sched, 5121), aid, "video stays with A");

    sched.mark_pid_deleted(0, 0, Some(5120));
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "del hands over to B"
    );
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "deleted PMT stops"
    );
    assert_eq!(claim(&sched, 5121), bid, "video moves to B");
    assert_eq!(claim(&sched, 5124), -1, "released data has no owner");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1102
#[test]
fn test_19e_11582h_group_and_disjoint() {
    let mut sched = test_sched(AdapterType::Dvb);
    let cid = sched.pmt_add(0, 10325, 5200);
    let did = sched.pmt_add(0, 10326, 5210);
    assert!(cid >= 0 && did >= 0, "could not create the PMTs");
    for id in [cid, did] {
        sched.pmt_add_stream_pid(id, 5201, 27, false, true);
        sched.pmt_add_stream_pid(id, 5202, 3, true, false);
        sched.pmt_add_stream_pid(id, 5204, 6, false, false);
    }
    let sids = [10327, 10328, 10329, 10330];
    let ppids = [5220, 5230, 5240, 5250];
    let dpids = [5225, 5235, 5245, 5255];
    let mut efgh = Vec::new();
    for ((&sid, &ppid), &dpid) in sids.iter().zip(&ppids).zip(&dpids) {
        let id = sched.pmt_add(0, sid, ppid);
        assert!(id >= 0, "could not create the group PMT");
        sched.pmt_add_stream_pid(id, 5221, 27, false, true);
        sched.pmt_add_stream_pid(id, 5222, 3, true, false);
        sched.pmt_add_stream_pid(id, dpid, 6, false, false);
        efgh.push(id);
    }
    let iid = sched.pmt_add(0, 10331, 5260);
    assert!(iid >= 0, "could not create the disjoint PMT");
    sched.pmt_add_stream_pid(iid, 5261, 27, false, true);
    sched.pmt_add_stream_pid(iid, 5262, 3, true, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 7;
        adapter.active_pmt[0] = cid;
        adapter.active_pmt[1] = did;
        for (index, id) in efgh.iter().enumerate() {
            adapter.active_pmt[2 + index] = *id;
        }
        adapter.active_pmt[6] = iid;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 5201), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5202), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5200), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, cid),
        PmtState::Running,
        "C should run first"
    );

    assert_eq!(sched.mark_pid_add(0, 0, 5210), 0);
    sched.update_pids(0);
    assert_eq!(pmt_state(&sched, cid), PmtState::Running, "C keeps running");
    assert_eq!(
        pmt_state(&sched, did),
        PmtState::Stopped,
        "dup never starts"
    );
    assert!(!pmt_best(&sched, did), "dup drops out of the set");

    sched.mark_pid_deleted(0, 0, Some(5200));
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, did),
        PmtState::Running,
        "del hands over to D"
    );
    assert_eq!(claim(&sched, 5201), did, "video moves to D");

    sched.mark_pid_deleted(0, 0, Some(5201));
    sched.mark_pid_deleted(0, 0, Some(5202));
    sched.mark_pid_deleted(0, 0, Some(5210));

    assert_eq!(sched.mark_pid_add(0, 0, 5221), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5222), 0);
    for ppid in ppids {
        assert_eq!(sched.mark_pid_add(0, 0, ppid), 0, "group pid added");
        sched.update_pids(0);
    }
    assert_eq!(
        pmt_state(&sched, efgh[0]),
        PmtState::Running,
        "E should run"
    );
    for id in efgh.iter().skip(1) {
        assert_eq!(pmt_state(&sched, *id), PmtState::Stopped, "group waits");
        assert!(pmt_best(&sched, *id), "waiter stays in the set");
    }

    assert_eq!(sched.mark_pid_add(0, 0, 5260), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5261), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 5262), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, iid),
        PmtState::Running,
        "disjoint runs too"
    );
    assert_eq!(
        pmt_state(&sched, efgh[0]),
        PmtState::Running,
        "E keeps running"
    );
    assert_eq!(claim(&sched, 5261), iid, "I owns its video");

    sched.mark_pid_deleted(0, 0, Some(5221));
    sched.mark_pid_deleted(0, 0, Some(5222));
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, efgh[0]),
        PmtState::Stopped,
        "AV-drop stops E"
    );
    assert_eq!(
        pmt_state(&sched, iid),
        PmtState::Running,
        "disjoint survives"
    );
    assert_eq!(claim(&sched, 5261), iid, "I keeps its video");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1206
#[test]
fn test_19e_11914h_ca_send_close() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    log.borrow_mut().reset();

    let jid = sched.pmt_add(0, 13, 101);
    let kid = sched.pmt_add(0, 17, 102);
    assert!(jid >= 0 && kid >= 0, "could not create the PMTs");
    sched.pmt_add_caid(jid, 0x098C, 6786, &[]);
    sched.pmt_add_caid(jid, 0x09F0, 8066, &[]);
    sched.pmt_add_stream_pid(jid, 1535, 27, false, true);
    sched.pmt_add_caid(kid, 0x098C, 6843, &[]);
    sched.pmt_add_caid(kid, 0x09F0, 8123, &[]);
    sched.pmt_add_stream_pid(kid, 1791, 27, false, true);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = jid;
        adapter.active_pmt[1] = kid;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 101), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 1535), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, jid), PmtState::Running, "J should run");
    assert_eq!(log.borrow().add_calls, 1, "J should be sent once");
    assert_eq!(log.borrow().last_add_pmt, jid, "J should be the sent PMT");

    sched.mark_pid_deleted(0, 0, Some(101));
    sched.mark_pid_deleted(0, 0, Some(1535));
    assert_eq!(sched.mark_pid_add(0, 0, 102), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 1791), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, kid),
        PmtState::Running,
        "zap should run K"
    );
    assert_eq!(
        pmt_state(&sched, jid),
        PmtState::Stopped,
        "zap should stop J"
    );
    assert_eq!(log.borrow().last_add_pmt, kid, "K should be sent on zap");
    assert_eq!(log.borrow().del_calls, 1, "J should be closed on zap");
    assert_eq!(log.borrow().last_del_pmt, jid, "closed PMT should be J");

    sched.mark_pid_deleted(0, 0, Some(102));
    sched.mark_pid_deleted(0, 0, Some(1791));
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, kid),
        PmtState::Stopped,
        "teardown stops K"
    );
    assert_eq!(log.borrow().del_calls, 2, "teardown closes K");
    assert_eq!(log.borrow().last_del_pmt, kid, "closed PMT should be K");

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1280
#[test]
fn ci_adapter_pmt_starts() {
    let mut sched = test_sched(AdapterType::Ci);
    let id = sched.pmt_add(0, 16542, 67);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 4942, 2, false, true);
    sched.pmt_add_stream_pid(id, 5042, 3, true, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }
    set_pid_slot(&mut sched, 0, 67, &[DDCI_SID], 0);
    set_pid_slot(&mut sched, 1, 4942, &[DDCI_SID], 0);
    set_pid_slot(&mut sched, 2, 5042, &[DDCI_SID], 0);

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "the CI adapter's PMT should start"
    );

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1316
#[test]
fn sticky_claims_without_parse() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    for id in [aid, bid] {
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    set_pid_slot(&mut sched, 0, 48, &[1], 5);
    set_pid_slot(&mut sched, 1, 52, &[0], 3);
    set_pid_slot(&mut sched, 2, 3301, &[0, 1], 6);
    set_pid_slot(&mut sched, 3, 3401, &[0, 1], 7);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = bid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "B should run alone"
    );

    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[1] = aid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "newcomer should wait"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "holder should keep running"
    );
    assert_eq!(claim(&sched, 3301), bid, "video pid should stay with B");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "newcomer should keep waiting"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "holder should keep running"
    );

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1374
#[test]
fn earlier_newcomer_steals_by_order() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    sched.pmt_add_stream_pid(aid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(bid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(bid, 3401, 3, true, false);
    set_pid_slot(&mut sched, 0, 48, &[1], 5);
    set_pid_slot(&mut sched, 1, 52, &[0], 3);
    set_pid_slot(&mut sched, 2, 3301, &[0, 1], 6);
    set_pid_slot(&mut sched, 3, 3401, &[0, 1], 7);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = aid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run alone"
    );
    assert_eq!(claim(&sched, 3301), aid, "A should own the video pid");

    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[1] = bid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "earlier should take over"
    );
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "holder should step aside"
    );
    assert_eq!(claim(&sched, 3301), bid, "video pid should move to B");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1427
#[test]
fn steal_splits_partial_overlap() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    sched.pmt_add_stream_pid(aid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(aid, 3401, 3, true, false);
    sched.pmt_add_stream_pid(bid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(bid, 3402, 3, true, false);
    set_pid_slot(&mut sched, 0, 48, &[1], 5);
    set_pid_slot(&mut sched, 1, 52, &[0], 3);
    set_pid_slot(&mut sched, 2, 3301, &[0, 1], 6);
    set_pid_slot(&mut sched, 3, 3401, &[1], 7);
    set_pid_slot(&mut sched, 4, 3402, &[0], 8);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = aid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run alone"
    );

    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[1] = bid;
    }
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "earlier should run"
    );
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "holder keeps own audio"
    );
    assert_eq!(claim(&sched, 3301), bid, "shared video moves to B");
    assert_eq!(claim(&sched, 3401), aid, "own audio stays with A");
    assert_eq!(claim(&sched, 3402), bid, "own audio goes to B");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1479
#[test]
fn pid_order_reset_on_remove() {
    let mut sched = test_sched(AdapterType::Dvb);
    assert_eq!(sched.mark_pid_add(0, 0, 100), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 101), 0);
    sched.update_pids(0);
    let first_order = pid_order(&sched, 100);
    assert!(
        first_order > 0 && pid_order(&sched, 101) > first_order,
        "later add should have larger order"
    );

    sched.mark_pid_deleted(0, 0, Some(100));
    sched.update_pids(0);
    assert!(sched.find_pid(0, 100).is_none(), "pid 100 should be gone");
    assert_eq!(sched.mark_pid_add(0, 0, 100), 0);
    sched.update_pids(0);
    let reorder = pid_order(&sched, 100);
    assert!(
        reorder > 0 && reorder != first_order,
        "re-added pid should get a fresh order"
    );

    assert_eq!(sched.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, 200), 0);
    assert_eq!(
        pid_order(&sched, 200),
        0,
        "scanner pid should have no order"
    );
    assert_eq!(sched.mark_pid_add(0, 0, 200), 0);
    assert!(
        pid_order(&sched, 200) > 0,
        "subscribed pid should gain an order"
    );

    sched.mark_pid_deleted(0, PID_STREAM_ID_UNDEFINED, None);
    sched.update_pids(0);
}

// Ported from tests/test_pmt.cpp:1521
#[test]
fn d8_shared_pid() {
    let mut sched = test_sched(AdapterType::Dvb);
    let m1 = sched.pmt_add(0, 100, 60);
    let m2 = sched.pmt_add(0, 200, 60);
    sched.pmt_add_stream_pid(m1, 701, 2, false, true);
    sched.pmt_add_stream_pid(m1, 711, 3, true, false);
    sched.pmt_add_stream_pid(m2, 701, 2, false, true);
    sched.pmt_add_stream_pid(m2, 712, 3, true, false);
    let m3 = sched.pmt_add(0, 300, 61);
    let m4 = sched.pmt_add(0, 400, 61);
    for id in [m3, m4] {
        sched.pmt_add_stream_pid(id, 801, 2, false, true);
        sched.pmt_add_stream_pid(id, 811, 3, true, false);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 4;
        adapter.active_pmt[0] = m1;
        adapter.active_pmt[1] = m2;
        adapter.active_pmt[2] = m3;
        adapter.active_pmt[3] = m4;
    }
    for (slot, pid) in [60, 701, 712, 61, 801, 811].into_iter().enumerate() {
        set_pid_slot(&mut sched, slot, pid, &[0], slot as u32 + 1);
    }

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, m1),
        PmtState::Running,
        "first set should run"
    );
    assert_eq!(
        pmt_state(&sched, m2),
        PmtState::Running,
        "second set should run"
    );
    assert_eq!(
        pmt_state(&sched, m3),
        PmtState::Running,
        "lowest sid should run"
    );
    assert_eq!(
        pmt_state(&sched, m4),
        PmtState::Stopped,
        "duplicate should wait"
    );
    assert_eq!(claim(&sched, 701), m1, "shared video goes to the first");
    assert_eq!(claim(&sched, 712), m2, "own audio goes to the second");
    assert!(
        pmt_best(&sched, m1) && pmt_best(&sched, m2),
        "different sets stay in"
    );
    assert!(
        pmt_best(&sched, m3) && !pmt_best(&sched, m4),
        "only the lowest sid is best"
    );

    {
        let entry = sched.find_pid_mut(0, 712).unwrap();
        entry.sids.clear();
        entry.order = 0;
    }
    set_pid_slot(&mut sched, 6, 711, &[0], 7);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, m1),
        PmtState::Running,
        "m1 should keep running"
    );
    assert_eq!(
        pmt_state(&sched, m2),
        PmtState::Stopped,
        "m2 should wait claimless"
    );
    assert!(
        pmt_best(&sched, m1) && pmt_best(&sched, m2),
        "waiter stays in the set"
    );
    assert_eq!(claim(&sched, 701), m1, "shared video should not move");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1602
#[test]
fn shared_pid_split() {
    let mut sched = test_sched(AdapterType::Dvb);
    let m1 = sched.pmt_add(0, 100, 60);
    let m2 = sched.pmt_add(0, 200, 60);
    sched.pmt_add_stream_pid(m1, 701, 2, false, true);
    sched.pmt_add_stream_pid(m1, 711, 3, true, false);
    sched.pmt_add_stream_pid(m2, 701, 2, false, true);
    sched.pmt_add_stream_pid(m2, 712, 3, true, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = m1;
        adapter.active_pmt[1] = m2;
    }
    for (slot, pid) in [60, 701, 711, 712].into_iter().enumerate() {
        set_pid_slot(&mut sched, slot, pid, &[0], slot as u32 + 1);
    }

    sched.start_active_pmts(0);
    assert!(
        pmt_best(&sched, m1) && pmt_best(&sched, m2),
        "both should stay in the set"
    );
    assert_eq!(
        pmt_state(&sched, m1),
        PmtState::Running,
        "first contender should run"
    );
    assert_eq!(
        pmt_state(&sched, m2),
        PmtState::Running,
        "second contender should run"
    );
    assert_eq!(claim(&sched, 701), m1, "shared video goes to the first");
    assert_eq!(claim(&sched, 712), m2, "own audio goes to the second");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, m1),
        PmtState::Running,
        "first should stay up"
    );
    assert_eq!(
        pmt_state(&sched, m2),
        PmtState::Running,
        "second should stay up"
    );
    assert_eq!(claim(&sched, 701), m1, "shared video should not move");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1657
#[test]
fn test_30w_shared_pmt_pid() {
    let mut sched = test_sched(AdapterType::Dvb);
    set_pid_slot(&mut sched, 0, 817, &[0], 1);
    let (ica, _log) = register_ca(&mut sched);

    let sec6043: [u8; 45] = [
        0x02, 0xB0, 0x2A, 0x17, 0x9B, 0xE9, 0x00, 0x00, 0xFD, 0x30, 0xF0, 0x09, 0x09, 0x07, 0x18,
        0x14, 0xE6, 0x43, 0x02, 0x52, 0x11, 0x1B, 0xFD, 0x30, 0xF0, 0x00, 0x0F, 0xFD, 0x31, 0xF0,
        0x0A, 0x0A, 0x04, 0x70, 0x6F, 0x72, 0x00, 0x7C, 0x02, 0x00, 0x00, 0x43, 0x9A, 0x7E, 0xD6,
    ];
    let sec807: [u8; 48] = [
        0x02, 0xB0, 0x2D, 0x03, 0x27, 0xE9, 0x00, 0x00, 0xFF, 0x70, 0xF0, 0x00, 0x0F, 0xFF, 0x71,
        0xF0, 0x10, 0x09, 0x04, 0x18, 0x02, 0xE4, 0x06, 0x0A, 0x04, 0x70, 0x6F, 0x72, 0x00, 0x7C,
        0x02, 0x00, 0x00, 0x1B, 0xFF, 0x70, 0xF0, 0x06, 0x09, 0x04, 0x18, 0x02, 0xE4, 0x06, 0xA0,
        0x11, 0x76, 0x82,
    ];
    let sec1338: [u8; 48] = [
        0x02, 0xB0, 0x2D, 0x05, 0x3A, 0xE9, 0x00, 0x00, 0xFD, 0x30, 0xF0, 0x00, 0x0F, 0xFD, 0x31,
        0xF0, 0x10, 0x09, 0x04, 0x18, 0x02, 0xE4, 0xCF, 0x0A, 0x04, 0x70, 0x6F, 0x72, 0x00, 0x7C,
        0x02, 0x00, 0x00, 0x1B, 0xFD, 0x30, 0xF0, 0x06, 0x09, 0x04, 0x18, 0x02, 0xE4, 0xCF, 0xDD,
        0x41, 0xAD, 0x2C,
    ];

    let f1 = sched.add_filter(0, 817, FilterTarget::Pmt(None), 0);
    let f2 = sched.add_filter(0, 817, FilterTarget::Pmt(None), 0);
    let f3 = sched.add_filter(0, 817, FilterTarget::Pmt(None), 0);
    assert!(
        f1 >= 0 && f2 >= 0 && f3 >= 0,
        "need three filters on pid 817"
    );

    assert_eq!(sched.process_pmt(f1, &sec6043), 0, "6043 to parse");
    assert_eq!(sched.process_pmt(f2, &sec1338), 0, "1338 to parse");
    assert_eq!(sched.process_pmt(f3, &sec807), 0, "807 to parse");

    let p1338 = sched.get_pmt_for_sid_pid(0, 1338, 817).unwrap().id as i32;
    let p6043 = sched.get_pmt_for_sid_pid(0, 6043, 817).unwrap().id as i32;
    let p807 = sched.get_pmt_for_sid_pid(0, 807, 817).unwrap().id as i32;
    assert_eq!(
        sched.pmt_slot(p1338).unwrap().stream_pids.len(),
        2,
        "1338 should hold two streams"
    );
    assert_eq!(
        sched.pmt_slot(p6043).unwrap().stream_pids.len(),
        2,
        "6043 should hold two streams"
    );
    assert_eq!(
        sched.pmt_slot(p807).unwrap().stream_pids.len(),
        2,
        "807 should hold two streams"
    );
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 3;
        adapter.active_pmt[0] = p1338;
        adapter.active_pmt[1] = p6043;
        adapter.active_pmt[2] = p807;
    }
    assert!(
        carries_ecm(sched.pmt_slot(p1338).unwrap(), 1231),
        "1338 should carry ECM 1231"
    );
    assert!(
        carries_ecm(sched.pmt_slot(p6043).unwrap(), 1603),
        "6043 should carry ECM 1603"
    );
    assert!(
        carries_ecm(sched.pmt_slot(p807).unwrap(), 1030),
        "807 should carry ECM 1030"
    );

    assert_eq!(sched.mark_pid_add(0, 0, 7472), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 7473), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 8048), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 8049), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, p1338),
        PmtState::Running,
        "lowest sid should run"
    );
    assert_eq!(
        pmt_state(&sched, p6043),
        PmtState::Stopped,
        "higher sid should wait"
    );
    assert_eq!(
        pmt_state(&sched, p807),
        PmtState::Running,
        "disjoint service should run"
    );
    assert!(pmt_best(&sched, p6043), "waiter stays in the set");
    assert_eq!(claim(&sched, 7472), p1338, "1338 should own video");
    assert_eq!(claim(&sched, 7473), p1338, "1338 should own audio");
    assert_eq!(claim(&sched, 8048), p807, "807 should own video");
    sched.start_active_pmts(0);
    assert_ne!(pmt_ca_mask(&sched, p1338), 0, "loop should send the winner");
    assert_ne!(pmt_ca_mask(&sched, p807), 0, "loop should send 807");
    assert_eq!(pmt_ca_mask(&sched, p6043), 0, "waiter should never be sent");

    sched.mark_pid_deleted(0, 0, Some(7472));
    sched.mark_pid_deleted(0, 0, Some(7473));
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, p1338),
        PmtState::Stopped,
        "1338 should stop"
    );
    assert_eq!(
        pmt_state(&sched, p807),
        PmtState::Running,
        "807 should keep running"
    );
    assert!(
        sched.find_pid(0, 7472).is_none(),
        "video pid should leave the demux"
    );
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, p807),
        PmtState::Running,
        "807 should stay up"
    );
    assert_eq!(
        pmt_state(&sched, p1338),
        PmtState::Stopped,
        "1338 should stay down"
    );

    sched.del_ca(ica);
    sched.free_all_pmts();
    sched.free_filters();
}

// Ported from tests/test_pmt.cpp:1770
#[test]
fn test_1129_bein_shared_es() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, _log) = register_ca(&mut sched);
    let ticari = sched.pmt_add(0, 14104, 52);
    let bein = sched.pmt_add(0, 14101, 48);
    assert!(ticari >= 0 && bein >= 0, "could not create the PMTs");
    for id in [ticari, bein] {
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
        sched.pmt_add_caid(id, 0x0664, 0x1F00 + id as u16, &[]);
    }
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = ticari;
        adapter.active_pmt[1] = bein;
    }
    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, bein),
        PmtState::Running,
        "subscribed PMT should run"
    );
    assert_eq!(
        pmt_state(&sched, ticari),
        PmtState::Stopped,
        "parsed-first should wait"
    );
    assert_eq!(claim(&sched, 3301), bein, "subscribed PMT should own video");
    sched.start_active_pmts(0);
    assert_ne!(pmt_ca_mask(&sched, bein), 0, "loop should send the winner");
    assert_eq!(pmt_ca_mask(&sched, ticari), 0, "loser should never be sent");

    sched.mark_pid_deleted(0, 0, Some(48));
    assert_eq!(sched.mark_pid_add(0, 0, 52), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, ticari),
        PmtState::Running,
        "new zap should run"
    );
    assert_eq!(
        pmt_state(&sched, bein),
        PmtState::Stopped,
        "old zap should stop"
    );
    assert_eq!(
        claim(&sched, 3301),
        ticari,
        "video should move with the zap"
    );

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1831
#[test]
fn test_1129_stingray_shared_vpid() {
    let mut sched = test_sched(AdapterType::Dvb);
    let eighties = sched.pmt_add(0, 200, 1922);
    let nature = sched.pmt_add(0, 100, 1908);
    assert!(eighties >= 0 && nature >= 0, "could not create the PMTs");
    sched.pmt_add_stream_pid(nature, 108, 2, false, true);
    sched.pmt_add_stream_pid(nature, 208, 3, true, false);
    sched.pmt_add_stream_pid(eighties, 108, 2, false, true);
    sched.pmt_add_stream_pid(eighties, 222, 3, true, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = eighties;
        adapter.active_pmt[1] = nature;
    }
    assert_eq!(sched.mark_pid_add(0, 0, 1908), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 108), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 208), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, nature),
        PmtState::Running,
        "subscribed PMT should run"
    );
    assert_eq!(
        pmt_state(&sched, eighties),
        PmtState::Stopped,
        "sibling should wait"
    );
    assert_eq!(claim(&sched, 108), nature, "shared VPID follows the sub");
    assert_eq!(claim(&sched, 208), nature, "own APID follows the sub");
    assert!(
        sched.find_pid(0, 222).is_none(),
        "other APID stays out of the demux"
    );

    sched.mark_pid_deleted(0, 0, Some(1908));
    sched.mark_pid_deleted(0, 0, Some(208));
    assert_eq!(sched.mark_pid_add(0, 0, 1922), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 222), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, eighties),
        PmtState::Running,
        "new zap should run"
    );
    assert_eq!(
        pmt_state(&sched, nature),
        PmtState::Stopped,
        "old zap should stop"
    );
    assert_eq!(claim(&sched, 108), eighties, "VPID should move with zap");
    assert_eq!(claim(&sched, 222), eighties, "new APID should be owned");

    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:1884
#[test]
fn held_pids_without_client_stop_pmt() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 3301, 2, false, true);
    sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    sched.pmt_add_caid(id, 0x0664, 0x1F06, &[]);
    sched.send_pmt_to_cas(0, id);
    assert_ne!(
        pmt_ca_mask(&sched, id),
        0,
        "PMT was not registered with the CA"
    );
    sched.pmt_slot_mut(id).unwrap().state = PmtState::Running;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }
    for (slot, pid) in [48, 3301, 3401].into_iter().enumerate() {
        set_pid_slot(&mut sched, slot, pid, &[DDCI_SID], 0);
        sched.find_pid_mut(0, pid).unwrap().pmt = id;
    }
    log.borrow_mut().del_calls = 0;
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Stopped,
        "held PMT should stop"
    );
    assert_eq!(log.borrow().del_calls, 1, "held PMT should release the CA");
    assert_eq!(pmt_ca_mask(&sched, id), 0, "CA registration should be gone");

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2022
#[test]
fn pids_all_expands_pmt_pids() {
    let mut sched = test_sched(AdapterType::Dvb);
    sched.pmt.set_emulate_pids_all(true);
    let id = sched.pmt_add(0, 100, 1000);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 100, 2, false, true);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }

    assert_eq!(
        sched.mark_pid_add(0, 0, 8192),
        0,
        "pids=all should be added"
    );
    sched.update_pids(0);
    let pmt_pid = sched.find_pid(0, 1000).unwrap();
    assert!(
        pmt_pid.sids.contains(&0),
        "PMT pid should carry the client sid"
    );
    let stream_pid = sched.find_pid(0, 100).unwrap();
    assert!(
        stream_pid.sids.contains(&0),
        "stream pid should carry the client sid"
    );

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should run for pids=all"
    );

    sched.pmt.set_emulate_pids_all(false);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2057
#[test]
fn multi_service_pid_parses_per_sid() {
    let mut sched = test_sched(AdapterType::Dvb);
    set_pid_slot(&mut sched, 0, 60, &[0], 1);
    let f1 = sched.add_filter(0, 60, FilterTarget::Pmt(None), 0);
    let f2 = sched.add_filter(0, 60, FilterTarget::Pmt(None), 0);
    assert!(f1 >= 0 && f2 >= 0 && f1 != f2, "need two filters on pid 60");

    let mut sec = vec![
        0x02, 0xB0, 0x12, 0x00, 0x64, 0xC1, 0x00, 0x00, 0xE2, 0xBD, 0xF0, 0x00, 0x02, 0xE2, 0xBD,
        0xF0, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(sched.process_pmt(f1, &sec), 0, "sid 100 to parse");
    sec[4] = 0xC8;
    sec[8] = 0xE2;
    sec[9] = 0xBE;
    sec[13] = 0xE2;
    sec[14] = 0xBE;
    assert_eq!(sched.process_pmt(f2, &sec), 0, "sid 200 to parse");

    let p1 = sched.get_pmt_for_sid_pid(0, 100, 60).unwrap().id as i32;
    let p2 = sched.get_pmt_for_sid_pid(0, 200, 60).unwrap().id as i32;
    assert_ne!(p1, p2, "each sid should own a PMT object");
    let streams1 = &sched.pmt_slot(p1).unwrap().stream_pids;
    assert_eq!(streams1.len(), 1, "sid 100 should carry one stream");
    assert_eq!(streams1[0].pid, 701, "sid 100 should carry pid 701");
    let streams2 = &sched.pmt_slot(p2).unwrap().stream_pids;
    assert_eq!(streams2.len(), 1, "sid 200 should carry one stream");
    assert_eq!(streams2[0].pid, 702, "sid 200 should carry pid 702");

    sched.free_all_pmts();
    sched.free_filters();
}

// Ported from tests/test_pmt.cpp:2104
#[test]
fn late_parse_handover() {
    let mut sched = test_sched(AdapterType::Dvb);
    let qid = sched.pmt_add(0, 14104, 52);
    assert!(qid >= 0, "could not create Q");
    sched.pmt_add_stream_pid(qid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(qid, 3401, 3, true, false);
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = qid;
    }
    set_pid_slot(&mut sched, 0, 52, &[1], 5);
    set_pid_slot(&mut sched, 1, 3301, &[0, 1], 6);
    set_pid_slot(&mut sched, 2, 3401, &[0, 1], 7);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, qid),
        PmtState::Running,
        "Q should run alone"
    );
    assert_eq!(claim(&sched, 3301), qid, "Q should own the video pid");

    set_pid_slot(&mut sched, 3, 48, &[0], 3);
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(None), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    let sec: [u8; 26] = [
        0x02, 0xB0, 0x17, 0x37, 0x15, 0xC1, 0x00, 0x00, 0xEC, 0xE5, 0xF0, 0x00, 0x02, 0xEC, 0xE5,
        0xF0, 0x00, 0x03, 0xED, 0x49, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(sched.process_pmt(fid, &sec), 0, "P to parse");
    let pid = sched.get_pmt_for_sid_pid(0, 14101, 48).unwrap().id as i32;
    assert_eq!(
        sched.pmt_slot(pid).unwrap().stream_pids.len(),
        2,
        "P should hold both streams"
    );
    assert_eq!(
        pmt_state(&sched, qid),
        PmtState::Stopped,
        "Q should stop at parse"
    );
    assert_eq!(claim(&sched, 3301), -1, "video claim should be released");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, pid),
        PmtState::Running,
        "P should start on next pass"
    );
    assert_eq!(claim(&sched, 3301), pid, "P should own the video pid");

    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2169
#[test]
fn running_pmt_pid_deleted_on_unsubscribe() {
    let mut sched = test_sched(AdapterType::Dvb);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 3301, 2, false, true);
    sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }
    set_pid_slot(&mut sched, 0, 48, &[0], 1);
    set_pid_slot(&mut sched, 1, 3301, &[0], 2);
    set_pid_slot(&mut sched, 2, 3401, &[0], 3);
    sched.find_pid_mut(0, 48).unwrap().filter = fid;

    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");
    assert_eq!(
        sched.filter_slot(fid).unwrap().flags,
        FILTER_CRC,
        "running filter keeps CRC only"
    );

    sched.mark_pid_deleted(0, 0, Some(48));
    sched.update_pids(0);
    assert!(
        sched.find_pid(0, 48).is_none(),
        "PMT pid should leave the demux"
    );
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should stay on pid delete"
    );
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should stay running"
    );

    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2224
#[test]
fn pmt_pid_remove_readd_no_churn() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 222, 222);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 2221, 27, false, true);
    sched.pmt_add_stream_pid(id, 2222, 6, true, false);
    sched.pmt_add_caid(id, 0x4AFC, 2220, &[]);
    let fid = sched.add_filter(0, 222, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 222), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 2221), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 2222), 0);
    sched.update_pids(0);
    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");
    assert_eq!(log.borrow().add_calls, 1, "PMT should reach the CA once");

    log.borrow_mut().reset();
    sched.mark_pid_deleted(0, 0, Some(222));
    sched.update_pids(0);
    assert!(
        sched.find_pid(0, 222).is_none(),
        "pid 222 should leave the demux"
    );
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should stay");
    assert_eq!(log.borrow().del_calls, 0, "PMT should not leave the CA");
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should stay on pass"
    );
    assert_eq!(log.borrow().add_calls, 0, "PMT should not be re-sent");

    assert_eq!(
        sched.mark_pid_add(0, 0, 222),
        0,
        "pid 222 should be added back"
    );
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should stay");
    assert_eq!(log.borrow().del_calls, 0, "PMT should not leave the CA");
    assert_eq!(log.borrow().add_calls, 0, "PMT should not be re-sent");
    assert_eq!(claim(&sched, 2221), id, "video claim should be kept");

    sched.del_ca(ica);
    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2295
#[test]
fn pmt_pid_delete_hands_over() {
    let mut sched = test_sched(AdapterType::Dvb);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    assert!(aid >= 0 && bid >= 0, "could not create the PMTs");
    for id in [aid, bid] {
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    }
    let fa = sched.add_filter(0, 48, FilterTarget::Pmt(Some(aid as usize)), 0);
    let fb = sched.add_filter(0, 52, FilterTarget::Pmt(Some(bid as usize)), 0);
    assert!(fa >= 0 && fb >= 0, "could not add the PMT filters");
    sched.pmt_slot_mut(aid).unwrap().filter = fa;
    sched.pmt_slot_mut(bid).unwrap().filter = fb;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, aid), PmtState::Running, "A should run");
    assert_eq!(claim(&sched, 3301), aid, "A should own the video pid");

    sched.mark_pid_deleted(0, 0, Some(48));
    assert_eq!(sched.mark_pid_add(0, 0, 52), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "A should stop on pid delete"
    );
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "B should take over at once"
    );
    assert_eq!(claim(&sched, 3301), bid, "B should own the video pid");
    assert!(
        sched.find_pid(0, 48).is_none(),
        "pid 48 should leave the demux"
    );

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "B should stay up"
    );

    sched.del_filter(fa);
    sched.del_filter(fb);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2357
#[test]
fn update_pids_tail_elects() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let aid = sched.pmt_add(0, 14101, 48);
    let bid = sched.pmt_add(0, 14104, 52);
    assert!(aid >= 0 && bid >= 0, "could not create the PMTs");
    for id in [aid, bid] {
        sched.pmt_add_stream_pid(id, 3301, 2, false, true);
        sched.pmt_add_stream_pid(id, 3401, 3, true, false);
        sched.pmt_add_caid(id, 0x0664, 0x1F00 + id as u16, &[]);
    }
    let fa = sched.add_filter(0, 48, FilterTarget::Pmt(Some(aid as usize)), 0);
    let fb = sched.add_filter(0, 52, FilterTarget::Pmt(Some(bid as usize)), 0);
    assert!(fa >= 0 && fb >= 0, "could not add the PMT filters");
    sched.pmt_slot_mut(aid).unwrap().filter = fa;
    sched.pmt_slot_mut(bid).unwrap().filter = fb;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 2;
        adapter.active_pmt[0] = aid;
        adapter.active_pmt[1] = bid;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run with no pump"
    );
    assert_eq!(claim(&sched, 3301), aid, "A should own the video pid");
    assert_eq!(
        pmt_ca_mask(&sched, aid),
        0,
        "A should be elected but unsent"
    );

    sched.start_active_pmts(0);
    assert_ne!(
        pmt_ca_mask(&sched, aid),
        0,
        "the loop should send A to the CA"
    );

    sched.mark_pid_deleted(0, 0, Some(48));
    assert_eq!(sched.mark_pid_add(0, 0, 52), 0);
    log.borrow_mut().reset();
    sched.update_pids(0);
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "A should stop on pid delete"
    );
    assert_eq!(
        log.borrow().del_calls,
        1,
        "A should release the CA at the tail"
    );
    assert_eq!(log.borrow().last_del_pmt, aid, "the wrong PMT left the CA");
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Running,
        "B should take over with no pump"
    );
    assert_eq!(claim(&sched, 3301), bid, "B should own the video pid");
    assert_eq!(
        pmt_ca_mask(&sched, bid),
        0,
        "B should be elected but unsent"
    );
    sched.start_active_pmts(0);
    assert_ne!(
        pmt_ca_mask(&sched, bid),
        0,
        "the loop should send B to the CA"
    );

    sched.del_ca(ica);
    sched.del_filter(fa);
    sched.del_filter(fb);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2434
#[test]
fn stream_pid_delete_stops_pmt() {
    let mut sched = test_sched(AdapterType::Dvb);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 3301, 2, false, true);
    sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");

    sched.mark_pid_deleted(0, 0, Some(3301));
    sched.mark_pid_deleted(0, 0, Some(3401));
    sched.update_pids(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Stopped, "PMT should stop");
    assert!(
        sched.find_pid(0, 3301).is_none(),
        "video pid should be gone"
    );
    let pmt_pid = sched.find_pid(0, 48).unwrap();
    assert_eq!(pmt_pid.state, PidState::Active, "PMT pid should remain");

    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2480
#[test]
fn pmt_cosmetic_update_keeps_running() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);

    let types = [2, 3];
    let spids = [3301, 3401];
    let sec = build_pmt(100, 1, 3301, 0x0B00, 0x0C00, &types, &spids);
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v1 to parse");
    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");
    assert_eq!(log.borrow().add_calls, 1, "PMT should be sent to the CA");

    let sec = build_pmt(100, 2, 3301, 0x0B00, 0x0C00, &types, &spids);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v2 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 2, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(claim(&sched, 3401), id, "audio claim should be kept");
    assert_eq!(log.borrow().del_calls, 0, "CA should see no delete");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(log.borrow().add_calls, 1, "CA should see the update");
    assert_eq!(log.borrow().last_add_update, 1, "re-send is an update");

    let types3 = [3, 2];
    let spids3 = [3401, 3301];
    let sec = build_pmt(100, 3, 3301, 0x0B00, 0x0C00, &types3, &spids3);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v3 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 3, "version should advance");
    assert_eq!(
        sched.pmt_slot(id).unwrap().stream_pids.len(),
        2,
        "streams should be intact"
    );
    assert_eq!(
        sched.pmt_slot(id).unwrap().pcr_pid,
        3301,
        "PCR should be intact"
    );
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(claim(&sched, 3401), id, "audio claim should be kept");
    assert_eq!(log.borrow().del_calls, 0, "CA should see no delete");
    assert_eq!(pmt_ca_mask(&sched, id), 0, "CA re-send should be pending");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(
        log.borrow().add_calls,
        1,
        "CA should see one benign re-send"
    );

    sched.del_ca(ica);
    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2661
#[test]
fn pmt_content_update_delta() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);

    let types = [2, 3];
    let spids = [3301, 3401];
    let sec = build_pmt(100, 1, 3301, 0x0B00, 0x0C00, &types, &spids);
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v1 to parse");
    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");
    assert_eq!(log.borrow().add_calls, 1, "PMT should be sent to the CA");

    let types2 = [2];
    let spids2 = [3301];
    let sec = build_pmt(100, 2, 3301, 0x0B00, 0x0C00, &types2, &spids2);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v2 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 2, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(claim(&sched, 3401), id, "stale audio claim should linger");
    assert_eq!(log.borrow().del_calls, 0, "CA should see no delete");
    assert_eq!(pmt_ca_mask(&sched, id), 0, "CA re-send should be pending");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(log.borrow().add_calls, 1, "CA should see one re-send");
    assert_ne!(
        pmt_ca_mask(&sched, id),
        0,
        "PMT should hold a CA slot again"
    );
    assert_eq!(claim(&sched, 3401), id, "stale claim should survive pass");

    let sec = build_pmt(100, 3, 3301, 0x0B00, 0x0C01, &types2, &spids2);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v3 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 3, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(log.borrow().del_calls, 0, "CA should see no delete");
    assert_eq!(pmt_ca_mask(&sched, id), 0, "CA re-send should be pending");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(log.borrow().add_calls, 1, "CA should see one re-send");
    assert_ne!(
        pmt_ca_mask(&sched, id),
        0,
        "PMT should hold a CA slot again"
    );

    let types4 = [27];
    let sec = build_pmt(100, 4, 3301, 0x0B00, 0x0C01, &types4, &spids2);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v4 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 4, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(log.borrow().del_calls, 0, "CA should see no delete");
    assert_eq!(pmt_ca_mask(&sched, id), 0, "CA re-send should be pending");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(log.borrow().add_calls, 1, "CA should see one re-send");
    assert_ne!(
        pmt_ca_mask(&sched, id),
        0,
        "PMT should hold a CA slot again"
    );

    let sec = build_pmt(100, 5, 3301, 0, 0, &types4, &spids2);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v5 to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 5, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(
        log.borrow().del_calls,
        1,
        "CA registration should be released"
    );
    assert_eq!(pmt_registered_mask(&sched, id), 0, "registration is gone");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(log.borrow().add_calls, 0, "FTA PMT should see no re-send");

    let bid = sched.pmt_add(0, 200, 52);
    assert!(bid >= 0, "could not create the second PMT");
    sched.pmt_add_caid(bid, 0x0B00, 0x0C00, &[]);
    sched.pmt_add_stream_pid(bid, 3401, 3, true, false);
    assert_eq!(sched.mark_pid_add(0, 0, 52), 0);
    sched.update_pids(0);
    sched.pmt_add_active_pmt(0, bid);

    assert_eq!(claim(&sched, 3401), id, "3401 should still be stale-held");
    assert!(
        pid_order(&sched, 48) < pid_order(&sched, 52),
        "B should subscribe later than A"
    );
    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(claim(&sched, 3401), id, "stale holder should keep 3401");
    assert_eq!(
        pmt_state(&sched, bid),
        PmtState::Stopped,
        "claimless rival waits stopped"
    );
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "first PMT should still run"
    );
    assert_eq!(
        log.borrow().add_calls,
        0,
        "stopped rival should see no CA send"
    );

    sched.del_ca(ica);
    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2880
#[test]
fn handover_stops_stale_holder() {
    let mut sched = test_sched(AdapterType::Dvb);
    assert_eq!(sched.mark_pid_add(0, 0, 60), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3302), 0);
    sched.update_pids(0);
    assert!(
        pid_order(&sched, 60) < pid_order(&sched, 48),
        "B should subscribe earlier than A"
    );

    let aid = sched.pmt_add(0, 100, 48);
    assert!(aid >= 0, "could not create PMT A");
    sched.pmt_add_stream_pid(aid, 3301, 2, false, true);
    sched.pmt_add_stream_pid(aid, 3302, 2, false, true);
    sched.pmt_add_active_pmt(0, aid);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, aid), PmtState::Running, "A should run");
    assert_eq!(claim(&sched, 3301), aid, "A should claim 3301");

    {
        let pmt = sched.pmt_slot_mut(aid).unwrap();
        pmt.stream_pids.retain(|stream| stream.pid != 3301);
    }

    let bid = sched.pmt_add(0, 200, 60);
    assert!(bid >= 0, "could not create PMT B");
    let bfid = sched.add_filter(0, 60, FilterTarget::Pmt(Some(bid as usize)), 0);
    assert!(bfid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(bid).unwrap().filter = bfid;
    let types = [2];
    let spids = [3301];
    let sec = build_pmt(200, 1, 3301, 0, 0, &types, &spids);
    assert_eq!(sched.process_pmt(bfid, &sec), 0, "B to parse");
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Stopped,
        "A must yield to handover"
    );

    sched.start_active_pmts(0);
    assert_eq!(claim(&sched, 3301), bid, "B should take 3301");
    assert_eq!(pmt_state(&sched, bid), PmtState::Running, "B should run");
    assert_eq!(
        pmt_state(&sched, aid),
        PmtState::Running,
        "A should run on 3302"
    );

    sched.del_filter(bfid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:3282
#[test]
fn version_update_keeps_claims() {
    let mut sched = test_sched(AdapterType::Dvb);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_stream_pid(id, 3301, 2, false, true);
    sched.pmt_add_stream_pid(id, 3401, 3, true, false);
    sched.pmt_slot_mut(id).unwrap().pcr_pid = 3301;
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }

    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3401), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
    assert_eq!(pmt_state(&sched, id), PmtState::Running, "PMT should run");
    assert_eq!(claim(&sched, 3301), id, "PMT should own the video pid");

    let sec: [u8; 26] = [
        0x02, 0xB0, 0x17, 0x00, 0x64, 0xC3, 0x00, 0x00, 0xEC, 0xE5, 0xF0, 0x00, 0x02, 0xEC, 0xE5,
        0xF0, 0x00, 0x03, 0xED, 0x49, 0xF0, 0x00, 0x00, 0x00, 0x00, 0x00,
    ];
    assert_eq!(sched.process_pmt(fid, &sec), 0, "update to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 1, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");
    assert_eq!(claim(&sched, 3401), id, "audio claim should be kept");

    let mut sec2 = sec;
    sec2[5] = 0xC1 | (2 << 1);
    assert_eq!(sched.process_pmt(fid, &sec2), 0, "same content to parse");
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should keep running"
    );
    assert_eq!(pmt_version(&sched, id), 2, "version should advance");
    assert_eq!(claim(&sched, 3301), id, "video claim should be kept");

    sched.start_active_pmts(0);
    assert_eq!(
        pmt_state(&sched, id),
        PmtState::Running,
        "PMT should still run"
    );
    assert_eq!(claim(&sched, 3301), id, "video claim should stay kept");

    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Wiring additions (not ports): pmt_tune, CA masks and filter queries.

#[test]
fn post_tune_arms_pat_and_sdt_filters_once() {
    let mut sched = test_sched(AdapterType::Dvb);
    sched.post_tune_adapter(0);
    assert_eq!(sched.section_pids(0), vec![0, 17]);
    let pat = sched.filters_for_pid(0, 0);
    assert_eq!(pat.len(), 1);
    assert_eq!(sched.filter_slot(pat[0]).unwrap().target, FilterTarget::Pat);
    assert_eq!(
        sched.filter_slot(pat[0]).unwrap().flags,
        FILTER_PERMANENT | FILTER_CRC
    );
    // A retune reuses the permanent filters (pmt_tune checks pat_filter == -1).
    sched.post_tune_adapter(0);
    assert_eq!(sched.filters_for_pid(0, 0), pat);
    assert_eq!(sched.filters_for_pid(0, 17).len(), 1);
}

#[test]
fn ci_adapters_only_get_pid_zero_on_post_tune() {
    let mut sched = test_sched(AdapterType::Ci);
    sched.post_tune_adapter(0);
    assert_eq!(sched.adapter(0).unwrap().active_pids(), vec![0]);
    let mut dvb = test_sched(AdapterType::Dvb);
    dvb.post_tune_adapter(0);
    assert_eq!(dvb.adapter(0).unwrap().active_pids(), vec![0, 1, 16, 18]);
}

#[test]
fn ca_masks_follow_registration_and_restart() {
    let mut sched = test_sched(AdapterType::Dvb);
    let log = Rc::new(RefCell::new(CaLog::default()));
    let ca = sched.add_ca(Box::new(FakeCa(log.clone())));
    // An unregistered CA id never reaches the adapter mask.
    sched.init_ca_for_adapter(0, ca + 1);
    assert_eq!(sched.adapter_ca_mask(0), 0);
    sched.init_ca_for_adapter(0, ca);
    assert_eq!(sched.adapter_ca_mask(0), 1 << ca);

    let pmt = sched.pmt_add(0, SID_KEPT, PID_KEPT);
    sched.pmt_add_caid(pmt, 0x0B00, 0x500, &[]);
    sched.send_pmt_to_cas(0, pmt);
    assert_eq!(pmt_ca_mask(&sched, pmt), 1 << ca);
    sched.restart_pmt_for_ca(ca, pmt);
    assert_eq!(pmt_ca_mask(&sched, pmt), 0, "restart must allow a resend");
    assert_eq!(log.borrow().del_calls, 1);

    sched.send_pmt_to_cas(0, pmt);
    sched.pmt_slot_mut(pmt).unwrap().disabled_ca_mask = 1 << ca;
    sched.clear_pmt_ca_masks(pmt, ca, true);
    let slot = sched.pmt_slot(pmt).unwrap();
    assert_eq!(
        (slot.ca_mask, slot.ca_registered_mask, slot.disabled_ca_mask),
        (0, 0, 0)
    );

    sched.close_ca_for_adapter(0);
    assert_eq!(sched.adapter_ca_mask(0), 0);
}

/// Makes `id` a running service on adapter 0 with claims on `pmt_pid` and `stream_pid`,
/// the way the tune path starts it; the first `start_active_pmts` offers it to the CA.
fn run_ca_pmt(sched: &mut Scheduler, id: i32, pmt_pid: i32, stream_pid: i32) {
    {
        let adapter = sched.adapter_mut(0).unwrap();
        adapter.active_pmts = 1;
        adapter.active_pmt[0] = id;
    }
    assert_eq!(sched.mark_pid_add(0, 0, pmt_pid), 0);
    assert_eq!(sched.mark_pid_add(0, 0, stream_pid), 0);
    sched.update_pids(0);
    sched.start_active_pmts(0);
}

// Ported from tests/test_ca.cpp:884 (PMT pid 0x0201 for the 13-bit 0x2001, stream 0x0202 added,
// adapter 0): a PMT stopped before its re-send must still be released on its CA (#1442)
#[test]
fn test_close_pmt_after_caid_update() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    // PIDs are 13 bit here, so C++ pmt pid 0x2001 is 0x0201.
    let id = sched.pmt_add(0, 0x2000, 0x0201);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_caid(id, 0x0664, 0x1F06, &[]);
    sched.pmt_add_stream_pid(id, 0x0202, 2, false, true);
    run_ca_pmt(&mut sched, id, 0x0201, 0x0202);
    assert_eq!(log.borrow().add_calls, 1, "add_ca did not publish the op");
    assert_eq!(
        pmt_ca_mask(&sched, id),
        1 << ica,
        "PMT was not sent to the CA"
    );

    // a new CA descriptor turns up, the PMT is due for a re-send ...
    sched.pmt_add_caid(id, 0x06EE, 0x1F07, &[]);
    assert_eq!(
        pmt_ca_mask(&sched, id),
        0,
        "pmt_add_caid() expected to clear ca_mask"
    );

    // ... but it is stopped before the re-send
    log.borrow_mut().reset();
    sched.stop_pmt(id);
    assert_eq!(
        log.borrow().del_calls,
        1,
        "the CA holding the PMT was not told that it stopped"
    );
    assert_eq!(
        pmt_registered_mask(&sched, id),
        0,
        "the CA registration was not cleared"
    );

    // nothing left to release: a second stop is a no-op, and the close path itself
    // must not release a registration that is already gone
    sched.stop_pmt(id);
    sched.close_pmt_for_cas(0, id);
    assert_eq!(log.borrow().del_calls, 1, "the PMT was released twice");

    // The literal `ca_mask = 0` (`invalidate`) must be released the same way.
    sched.start_pmt(id);
    sched.start_active_pmts(0);
    assert_eq!(
        pmt_registered_mask(&sched, id),
        1 << ica,
        "the restarted PMT should be registered"
    );
    sched.pmt_slot_mut(id).unwrap().ca_mask = 0;
    assert_eq!(
        pmt_ca_mask(&sched, id),
        0,
        "invalidate() expected to clear ca_mask"
    );
    sched.stop_pmt(id);
    assert_eq!(
        log.borrow().del_calls,
        2,
        "an invalidated PMT was not released"
    );
    assert_eq!(
        pmt_registered_mask(&sched, id),
        0,
        "the CA registration was not cleared"
    );

    sched.del_ca(ica);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:2816 (the CA update flag tells a re-send from a first add)
#[test]
fn test_ca_update_flag_distinguishes_resend() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    let fid = sched.add_filter(0, 48, FilterTarget::Pmt(Some(id as usize)), 0);
    assert!(fid >= 0, "could not add the PMT filter");
    sched.pmt_slot_mut(id).unwrap().filter = fid;
    assert_eq!(sched.mark_pid_add(0, 0, 48), 0);
    assert_eq!(sched.mark_pid_add(0, 0, 3301), 0);
    sched.update_pids(0);

    let sec = build_pmt(100, 1, 3301, 0x0B00, 0x0C00, &[2], &[3301]);
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v1 to parse");
    log.borrow_mut().reset();
    sched.start_active_pmts(0);
    assert_eq!(log.borrow().add_calls, 1, "PMT should be sent to the CA");
    assert_eq!(log.borrow().last_add_update, 0, "first send is an add");
    assert_eq!(pmt_ca_mask(&sched, id), 1 << ica, "PMT holds its CA slot");

    let sec = build_pmt(100, 2, 3301, 0x0B00, 0x0C00, &[2], &[3301]);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v2 to parse");
    assert_eq!(
        pmt_ca_mask(&sched, id),
        0,
        "cosmetic bump clears the send mask"
    );
    sched.start_active_pmts(0);
    assert_eq!(
        log.borrow().add_calls,
        1,
        "cosmetic bump still informs the CA"
    );
    assert_eq!(log.borrow().last_add_update, 1, "re-send is an update");

    let sec = build_pmt(100, 3, 3301, 0x0B00, 0x0C00, &[2, 3], &[3301, 3401]);
    log.borrow_mut().reset();
    assert_eq!(sched.process_pmt(fid, &sec), 0, "v3 to parse");
    assert_eq!(
        pmt_ca_mask(&sched, id),
        0,
        "content change clears the send mask"
    );
    sched.start_active_pmts(0);
    assert_eq!(log.borrow().add_calls, 1, "content change re-sends");
    assert_eq!(log.borrow().last_add_update, 1, "re-send is an update");

    sched.del_ca(ica);
    sched.del_filter(fid);
    sched.free_all_pmts();
}

// Ported from tests/test_pmt.cpp:3206 (PMT run state lives outside the CA layer)
#[test]
fn test_resend_rejected_releases_ca() {
    let mut sched = test_sched(AdapterType::Dvb);
    let (ica, log) = register_ca(&mut sched);
    let id = sched.pmt_add(0, 100, 48);
    assert!(id >= 0, "could not create the PMT");
    sched.pmt_add_caid(id, 0x0B00, 0x0C00, &[]);
    sched.pmt_add_stream_pid(id, 3301, 2, false, true);
    run_ca_pmt(&mut sched, id, 48, 3301);
    assert_eq!(
        pmt_registered_mask(&sched, id),
        1 << ica,
        "PMT should be registered"
    );

    // update clears the send mask; the re-send is rejected this time
    sched.pmt_slot_mut(id).unwrap().ca_mask = 0;
    log.borrow_mut().reset();
    log.borrow_mut().fail_next = true;
    sched.send_pmt_to_cas(0, id);
    assert_eq!(log.borrow().add_calls, 1, "the re-send was offered");
    assert_eq!(log.borrow().last_add_update, 1, "the re-send is an update");
    assert_eq!(log.borrow().del_calls, 1, "stale registration should close");
    assert_eq!(pmt_registered_mask(&sched, id), 0, "registration is gone");
    assert_eq!(
        sched.pmt_slot(id).unwrap().disabled_ca_mask,
        1 << ica,
        "CA should be disabled"
    );

    log.borrow_mut().reset();
    sched.send_pmt_to_cas(0, id);
    assert_eq!(log.borrow().add_calls, 0, "a disabled CA is not retried");

    sched.del_ca(ica);
    sched.free_all_pmts();
}

#[test]
fn ca_service_carries_pcr_and_av_pids() {
    let mut sched = test_sched(AdapterType::Dvb);
    let log = Rc::new(RefCell::new(CaLog::default()));
    let ca = sched.add_ca(Box::new(FakeCa(log.clone())));
    sched.init_ca_for_adapter(0, ca);
    let pmt = sched.pmt_add(0, SID_KEPT, PID_KEPT);
    sched.pmt_add_caid(pmt, 0x0B00, 0x500, &[]);
    sched.pmt_add_stream_pid(pmt, 1001, 2, false, true);
    sched.pmt_add_stream_pid(pmt, 1002, 6, false, false);
    sched.pmt_slot_mut(pmt).unwrap().pcr_pid = 1003;
    let service = sched.ca_service(pmt).unwrap();
    assert_eq!(service.pcr_pid, 1003);
    assert_eq!(service.av_pids, vec![1001]);
}

/// One SDT section; each service carries a 0x48 service descriptor.
fn build_sdt(table_id: u8, tsid: u16, onid: u16, services: &[(u16, &str, &str)]) -> Vec<u8> {
    let mut body = vec![0xC1, 0, 0];
    body.extend_from_slice(&onid.to_be_bytes());
    body.push(0xFF);
    for &(sid, provider, name) in services {
        let mut desc = vec![0x48, 0, 0x19, provider.len() as u8];
        desc.extend_from_slice(provider.as_bytes());
        desc.push(name.len() as u8);
        desc.extend_from_slice(name.as_bytes());
        desc[1] = (desc.len() - 2) as u8;
        body.extend_from_slice(&sid.to_be_bytes());
        // EIT schedule and p/f set; running (4) and scrambled.
        body.push(0xFF);
        body.push(0x90 | ((desc.len() >> 8) as u8 & 0x0F));
        body.push(desc.len() as u8);
        body.extend_from_slice(&desc);
    }
    let mut section = vec![table_id, 0, 0];
    section.extend_from_slice(&tsid.to_be_bytes());
    section.extend_from_slice(&body);
    section.extend_from_slice(&[0; 4]);
    let len = section.len() - 3;
    section[1] = 0xB0 | ((len >> 8) as u8 & 0x0F);
    section[2] = len as u8;
    section
}

// pmt.cpp:2246-2291: the SDT fields follow every section, the names only the first.
#[test]
fn sdt_names_a_pmt_once_and_tracks_its_fields() {
    let mut sched = test_sched(AdapterType::Dvb);
    let id = sched.pmt_add(0, 0x409E, 67);
    let filter = sched.add_filter(0, 17, FilterTarget::Sdt, FILTER_CRC);
    sched.process_sdt(
        filter,
        &build_sdt(0x42, 43305, 1, &[(0x409E, "Provider", "Service 1")]),
    );
    let pmt = sched.pmt(id).unwrap();
    assert_eq!(
        (pmt.name.as_str(), pmt.provider.as_str()),
        ("Service 1", "Provider")
    );
    let sdt = pmt.sdt.expect("has_sdt");
    assert_eq!(
        (
            sdt.service_type,
            sdt.running_status,
            sdt.ca_mode,
            sdt.eit_schedule,
            sdt.eit_pf
        ),
        (0x19, 4, 1, 1, 1)
    );
    assert_eq!((sdt.tsid, sdt.onid), (43305, 1));

    sched.process_sdt(
        filter,
        &build_sdt(0x42, 43306, 1, &[(0x409E, "x", "renamed")]),
    );
    let pmt = sched.pmt(id).unwrap();
    assert_eq!(pmt.name, "Service 1", "the name is kept once set");
    assert_eq!(pmt.sdt.unwrap().tsid, 43306, "the fields follow the SDT");
}

#[test]
fn sdt_other_and_unknown_services_are_ignored() {
    let mut sched = test_sched(AdapterType::Dvb);
    let id = sched.pmt_add(0, 0x2000, 100);
    let filter = sched.add_filter(0, 17, FilterTarget::Sdt, FILTER_CRC);
    sched.process_sdt(
        filter,
        &build_sdt(0x46, 1, 1, &[(0x2000, "p", "other mux")]),
    );
    assert!(
        sched.pmt(id).unwrap().name.is_empty(),
        "SDT other (0x46) is skipped"
    );
    assert!(sched.pmt(id).unwrap().sdt.is_none());
    sched.process_sdt(filter, &build_sdt(0x42, 1, 1, &[(0x2001, "p", "no pmt")]));
    assert!(sched.pmt(id).unwrap().name.is_empty());
}

// ddci.cpp:1020-1032: the CI-side PMT takes the source PMT's name once.
#[test]
fn name_pmt_for_sid_names_the_ci_side_pmt() {
    let mut sched = test_sched(AdapterType::Dvb);
    let source = sched.pmt_add(0, 0x409E, 67);
    let ci = sched.pmt_add(8, 0x409E, 67);
    assert_eq!(
        sched.name_pmt_for_sid(8, 0x409E, source, "Service 1", "Provider"),
        Some(ci)
    );
    assert_eq!(sched.pmt(ci).unwrap().name, "Service 1");
    assert_eq!(sched.pmt(ci).unwrap().provider, "Provider");
    assert_eq!(
        sched.name_pmt_for_sid(8, 0x409E, source, "Service 1", "Provider"),
        None
    );
    assert_eq!(
        sched.name_pmt_for_sid(8, 0x409E, ci, "other", "p"),
        None,
        "not itself"
    );
    assert_eq!(sched.name_pmt_for_sid(8, 0x1234, source, "x", "p"), None);
}

// pmt.cpp:1918-1925: with pmt_scan (the default) each PAT entry's PMT filter
// opens its pid; without it the pid waits for a stream to request it.
#[test]
fn pmt_scan_opens_the_pmt_pids_of_the_pat() {
    for scan in [true, false] {
        let mut sched = test_sched(AdapterType::Dvb);
        sched.set_pmt_scan(scan);
        sched.post_tune_adapter(0);
        let pat_filter = sched.filters_for_pid(0, 0)[0];
        sched.process_pat(pat_filter, &build_pat(0x10, 1, &[(100, 0x200)]));
        let pmt = sched.filters_for_pid(0, 0x200);
        assert_eq!(pmt.len(), 1, "scan {scan}");
        let flags = sched.filter_slot(pmt[0]).unwrap().flags;
        assert_eq!(flags & FILTER_ADD_REMOVE != 0, scan);
        assert_eq!(sched.find_pid(0, 0x200).is_some(), scan);
    }
}

/// Records every `set_pid` call and every bulk PID set.
struct PidLog {
    opened: Rc<RefCell<Vec<i32>>>,
    bulk: Rc<RefCell<Vec<Vec<u16>>>>,
}

impl AdapterTuner for PidLog {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }

    fn set_pid(&mut self, pid: i32) -> Result<i32, TuneError> {
        self.opened.borrow_mut().push(pid);
        Ok(1)
    }

    fn set_pids(&mut self, pids: &ms_tuner::PidSet) -> Result<(), TuneError> {
        self.bulk.borrow_mut().push(pids.iter().collect());
        Ok(())
    }
}

// adapter.cpp:997 and pmt.cpp:62: with -k, pids=all never opens pid 8192; the
// enforced PSI list and the null pid are opened for the stream instead.
#[test]
fn emulated_pids_all_opens_the_enforced_list_not_8192() {
    for emulate in [true, false] {
        let opened = Rc::new(RefCell::new(Vec::new()));
        let bulk = Rc::new(RefCell::new(Vec::new()));
        let mut sched = test_sched(AdapterType::Dvb);
        sched.adapter_mut(0).unwrap().set_tuner(Box::new(PidLog {
            opened: Rc::clone(&opened),
            bulk: Rc::clone(&bulk),
        }));
        sched.set_emulate_pids_all(emulate);
        assert_eq!(sched.mark_pid_add(0, 0, 8192), 0);
        sched.update_pids(0);
        let opened = opened.borrow();
        let all = sched.find_pid(0, 8192).unwrap();
        assert_eq!(all.state, PidState::Active);
        let last = bulk.borrow().last().cloned().unwrap_or_default();
        if emulate {
            assert!(!opened.contains(&8192), "{opened:?}");
            assert_eq!(all.fd, -1);
            for pid in [0, 1, 16, 17, 18, 20, 21, 8191] {
                assert!(opened.contains(&pid), "pid {pid} not opened: {opened:?}");
                assert!(sched.find_pid(0, pid).unwrap().sids.contains(&0));
            }
            assert!(!last.contains(&8192), "{last:?}");
        } else {
            assert_eq!(*opened, vec![8192]);
            assert_eq!(last, vec![8192]);
        }
    }
}

// pmt.cpp:1427: the null pid joins only when encrypted packets are sent (-E).
#[test]
fn emulated_pids_all_adds_the_null_pid_only_without_drop_encrypted() {
    for drop in [false, true] {
        let mut sched = test_sched(AdapterType::Dvb);
        sched.set_emulate_pids_all(true);
        sched.set_drop_encrypted(drop);
        assert_eq!(sched.mark_pid_add(0, 0, 8192), 0);
        sched.update_pids(0);
        assert_eq!(sched.find_pid(0, 8191).is_some(), !drop, "drop {drop}");
    }
}

type Updates = Rc<RefCell<Vec<(&'static str, Vec<u16>)>>>;

/// Records the order of section-PID and PID-set updates.
struct OrderLog(Updates);

impl AdapterTuner for OrderLog {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }

    fn set_section_pids(&mut self, pids: &ms_tuner::PidSet) {
        self.0
            .borrow_mut()
            .push(("sections", pids.iter().collect()));
    }

    fn set_pids(&mut self, pids: &ms_tuner::PidSet) -> Result<(), TuneError> {
        self.0.borrow_mut().push(("pids", pids.iter().collect()));
        Ok(())
    }
}

// dvb_is_psi (dvb.cpp:1824) reads the filter table when a PID opens, so the
// section PIDs reach the tuner before the PID set of the same update.
#[test]
fn update_pids_sends_section_pids_before_the_pid_set() {
    let log = Rc::new(RefCell::new(Vec::new()));
    let mut sched = test_sched(AdapterType::Dvb);
    sched
        .adapter_mut(0)
        .unwrap()
        .set_tuner(Box::new(OrderLog(Rc::clone(&log))));
    sched.post_tune_adapter(0);
    let pmt = sched.add_filter(0, 0x100, FilterTarget::Pmt(None), FILTER_ADD_REMOVE);
    assert!(pmt >= 0);
    sched.update_pids(0);
    let log = log.borrow();
    let last = log.len() - 1;
    assert_eq!(log[last].0, "pids");
    assert_eq!(log[last - 1], ("sections", vec![0, 17, 0x100]));
    assert!(log[last].1.contains(&0x100), "{log:?}");
}

// Ported from tests/test_utils.cpp:237 (test_find_new_id) on pmt_add, the
// find_new_id user (pmt.cpp:1533); the threaded cases are not ported (DEV-282).
#[test]
fn test_find_new_id() {
    let mut sched = test_sched(AdapterType::Dvb);
    for expected in 0..4 {
        assert_eq!(
            sched.pmt_add(0, 100 + expected, 200),
            expected,
            "slot gets id {expected}"
        );
    }
    sched.pmt_del(1);
    assert_eq!(sched.pmt_add(0, 300, 200), 1, "freed slot is reused");
    for expected in 4..MAX_PMT as i32 {
        assert_eq!(
            sched.pmt_add(0, 300, 200),
            expected,
            "slot gets id {expected}"
        );
    }
    assert_eq!(sched.pmt_add(0, 300, 200), -1, "full table returns -1");
}
