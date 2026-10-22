//! `/state.json` values the session fills from the scheduler and the CA runtime.

use ms_net::session::SessionManager;
use ms_net::web::{ADAPTER_CI, ADAPTER_DVB, CaDeviceState, PmtState};
use ms_sched::{AdapterConfig, AdapterTuner, AdapterType, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, TuneParams};

struct NoTune;

impl AdapterTuner for NoTune {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

fn manager() -> SessionManager {
    let mut sched = Scheduler::new();
    sched.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(NoTune),
    );
    let ci = AdapterConfig {
        enabled: true,
        adapter_type: AdapterType::Ci,
        ..AdapterConfig::default()
    };
    sched.add_adapter(8, ci, Box::new(NoTune));
    SessionManager::with_scheduler(sched)
}

// The status page shows dvbci.png and the CI pids only for ADAPTER_CI.
#[test]
fn ad_type_follows_the_adapter_kind() {
    let snapshot = manager().web_snapshot();
    assert_eq!(snapshot.adapters[0].kind, ADAPTER_DVB);
    assert_eq!(snapshot.adapters[8].kind, ADAPTER_CI);
}

// get_channel_for_adapter/get_pmt_for_adapter read the adapter's active PMTs.
#[test]
fn adapter_pmts_come_from_the_active_list() {
    let mut session = manager();
    let sched = session.scheduler_mut();
    let id = sched.pmt_add(0, 0x409E, 67);
    sched.pmt_add_active_pmt(0, id);
    sched.name_pmt_for_sid(0, 0x409E, -1, "beIN SPORTS 1", "beIN");
    let snapshot = session.web_snapshot();
    assert_eq!(
        snapshot.adapters[0].pmts,
        [PmtState {
            running: false,
            name: "beIN SPORTS 1".to_owned(),
            pid: 67,
        }]
    );
    assert!(snapshot.adapters[8].pmts.is_empty());
}

#[test]
fn ca_slots_pushed_by_the_runtime_reach_the_snapshot() {
    let mut session = manager();
    let mut ca = vec![None; 32];
    ca[8] = Some(CaDeviceState {
        enabled: 1,
        state: 2,
        caids: vec![0x0664],
        ci_name: "Irdeto Access".to_owned(),
    });
    session.set_web_ca(ca.clone(), Vec::new());
    assert_eq!(session.web_snapshot().ca, ca);
}
