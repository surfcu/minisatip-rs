//! Hardware-free adapter allocation, same-mux reuse and PID bookkeeping.
//!
//! Mirrors the allocation half of the C++ `adapter.cpp`; hardware access is
//! behind [`AdapterTuner`].

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use ms_tuner::{DeliverySystem, PidSet, Polarization, TuneParams};

pub mod pmt;

/// `#define DEFAULT_LOG LOG_ADAPTER` (adapter.cpp:58).
const DEFAULT_LOG: u32 = ms_log::LOG_ADAPTER;

/// Maximum PMTs tracked per adapter (`MAX_PMT_FOR_ADAPTER`).
pub const MAX_PMT_FOR_ADAPTER: usize = 255;
/// Adapter kind (`ADAPTER_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum AdapterType {
    /// Type not set (C++ zero-initialized test adapters).
    #[default]
    Unknown,
    /// Local DVB device (`ADAPTER_DVB`).
    Dvb,
    /// SAT>IP client (`ADAPTER_SATIP`).
    Satip,
    /// Network/CI backend (`ADAPTER_NETCV`).
    NetCv,
    /// Common-interface adapter (`ADAPTER_CI`).
    Ci,
}

/// Maximum adapter count (`MAX_ADAPTERS`).
pub const MAX_ADAPTERS: usize = 100;
/// Delivery systems stored per adapter (`MAX_DELSYS`).
pub const MAX_DELSYS: usize = 10;
/// Logical sources stored per adapter (`MAX_SOURCES`).
pub const MAX_SOURCES: usize = 64;
/// PID slots per adapter (`MAX_PIDS`).
pub const MAX_PIDS: usize = 128;
/// Length of the frontend-to-adapter map (`2 * MAX_ADAPTERS`).
pub const FE_MAP_LEN: usize = 2 * MAX_ADAPTERS;
/// PID sentinel meaning "all PIDs".
pub const PID_ALL: i32 = 8192;
/// Minimum usable PID count per adapter (`MIN_ADAPTER_PIDS`).
pub const MIN_ADAPTER_PIDS: i32 = 16;
/// Stream id used when a PID has no owning stream.
pub const PID_STREAM_ID_UNDEFINED: i32 = -1;
/// DVB stream identifier.
pub type StreamId = i32;

/// Tuning error codes exposed to RTSP, matching the C++ return values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TuneError {
    /// Hardware rejected the frequency (`-403`).
    OutOfRange,
    /// Adapter missing or hardware failure (`-500`).
    Hardware,
    /// PID table could not be updated (`-503`).
    Pids,
}

impl TuneError {
    /// Negative RTSP error code used by the C++ implementation.
    pub fn code(self) -> i32 {
        match self {
            Self::OutOfRange => -403,
            Self::Hardware => -500,
            Self::Pids => -503,
        }
    }
}

/// `iterable_to_string`: the PID set joined with `", "` in ascending order.
fn pids_string(pids: &PidSet) -> String {
    pids.iter()
        .map(|pid| pid.to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

/// `copy_dvb_param start` (dvb.cpp:395-403).
fn log_copy_start(p: &TuneParams) {
    ms_log::log!(
        "copy_dvb_param start -> src={}, fe={}, freq={}, fec={}, sr={}, pol={}, ro={}, msys={}, mtype={}, plts={}, bw={}, inv={}, pids={} x_pmt={}",
        p.src_value(),
        p.fe_value(),
        p.freq,
        p.fec_value(),
        p.sr_value(),
        p.pol_number(),
        p.ro.map_or(3, i32::from),
        p.sys_number(),
        p.mtype_value(),
        p.plts_number(),
        p.bw_value(),
        p.inversion_value(),
        pids_string(&p.pids),
        p.x_pmt.unwrap_or(-1)
    );
}

/// `copy_dvb_parameters` (dvb.cpp:463-470).
fn log_copy_end(p: &TuneParams) {
    ms_log::log!(
        "copy_dvb_parameters -> src={}, fe={}, freq={}, fec={} sr={}, pol={}, ro={}, msys={}, mtype={}, plts={}, bw={}, inv={}, pids={} x_pmt={}",
        p.src_value(),
        p.fe_value(),
        p.freq,
        p.fec_value(),
        p.sr_value(),
        p.pol_number(),
        p.ro.map_or(3, i32::from),
        p.sys_number(),
        p.mtype_value(),
        p.plts_number(),
        p.bw_value(),
        p.inversion_value(),
        pids_string(&p.pids),
        p.x_pmt.unwrap_or(-1)
    );
}

/// `set_adapter_parameters` failure, matching the C++ `-2`/`-1` returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SetParamsError {
    /// No such enabled adapter (`-1`).
    MissingAdapter,
    /// This stream is a slave and the tuning changed (`-2`).
    SlaveConflict,
    /// PID table or demux failure (`-1`).
    PidFailure,
}

impl SetParamsError {
    /// Negative return code used by the C++ implementation.
    pub fn code(self) -> i32 {
        match self {
            Self::MissingAdapter | Self::PidFailure => -1,
            Self::SlaveConflict => -2,
        }
    }
}

/// Result of [`Scheduler::tune`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuneOutcome {
    /// Tuning result.
    pub result: Result<(), TuneError>,
    /// Streams that must be closed after a master retune.
    pub close_streams: Vec<StreamId>,
}

/// Minimal tuner interface needed by the scheduler; hardware lives in M4.
pub trait AdapterTuner {
    /// Applies tuning parameters.
    fn tune(&mut self, params: &TuneParams) -> Result<(), TuneError>;

    /// Applies the adapter's active PID set after `update_pids`.
    ///
    /// The scheduler owns the table (defaults included, `dvb_set_pid` order);
    /// backends that do not filter in software can ignore it.
    fn set_pids(&mut self, _pids: &PidSet) -> Result<(), TuneError> {
        Ok(())
    }

    /// Opens one PID filter, returning its descriptor (C++ `set_pid`).
    ///
    /// The scheduler increments `active_pids` only after this succeeds; a
    /// negative or error result triggers the `max_pids` fallback.
    fn set_pid(&mut self, _pid: i32) -> Result<i32, TuneError> {
        Ok(1)
    }

    /// Closes one PID filter (C++ `del_filters`).
    ///
    /// Any error is reported from `update_pids` after the entry is cleaned up.
    fn del_filters(&mut self, _fd: i32, _pid: i32) -> Result<(), TuneError> {
        Ok(())
    }
}

/// Monotonic source for [`PidEntry`] subscription order (`adapter::pid_order_seq`).
#[derive(Debug, Clone, Default)]
pub struct PidOrder(Arc<AtomicU32>);

impl PidOrder {
    /// Next nonzero order, identical to the C++ `next_pid_order`.
    pub fn next_pid_order(&self) -> u32 {
        let mut sequence = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        if sequence == 0 {
            sequence = self.0.fetch_add(1, Ordering::Relaxed) + 1;
        }
        sequence
    }
}

/// Single-cable switch type (`diseqc::switch_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SwitchType {
    /// A conventional DiSEqC switch.
    #[default]
    None,
    /// EN 50494 unicable.
    Unicable,
    /// EN 50607 JESS.
    Jess,
    /// The adapter drives the switch through its master (`SWITCH_SLAVE`).
    Slave,
}

impl SwitchType {
    /// Whether band/polarisation conflicts are irrelevant for this switch.
    pub fn is_single_cable(self) -> bool {
        matches!(self, Self::Unicable | Self::Jess)
    }
}

/// Static and tunable properties of one adapter.
#[derive(Debug, Clone)]
pub struct AdapterConfig {
    /// Whether the adapter is initialized.
    pub enabled: bool,
    /// Supported delivery systems; `Undefined` marks unused slots.
    pub sys: [DeliverySystem; MAX_DELSYS],
    /// Absolute-source mapping for `src - 1`.
    pub absolute_table: [u8; MAX_SOURCES],
    /// Master adapter index when this is a slave tuner.
    pub master_source: Option<usize>,
    /// DiSEqC/single-cable switch type.
    pub switch_type: SwitchType,
    /// LNB band-switch frequency in kHz.
    pub lnb_switch: u32,
    /// Circular LNB local-oscillator frequency in kHz.
    pub lnb_circular: u32,
    /// Initial hardware PID limit (`opts.max_pids`, 0 means unlimited).
    pub max_pids: i32,
    /// Adapter kind; CI adapters accept DDCI's PID subscriptions.
    pub adapter_type: AdapterType,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sys: [DeliverySystem::Undefined; MAX_DELSYS],
            absolute_table: [0; MAX_SOURCES],
            master_source: None,
            switch_type: SwitchType::None,
            lnb_switch: 11_700_000,
            lnb_circular: 10_750_000,
            max_pids: 0,
            adapter_type: AdapterType::Unknown,
        }
    }
}

impl AdapterConfig {
    /// Creates an enabled adapter supporting `sys`.
    pub fn new(sys: DeliverySystem) -> Self {
        let mut config = Self {
            enabled: true,
            ..Self::default()
        };
        config.sys[0] = sys;
        config
    }
}

/// Runtime state of one adapter.
pub struct Adapter {
    /// Static capabilities.
    pub config: AdapterConfig,
    /// Stream owning the adapter, or `-1`.
    pub master_sid: StreamId,
    /// Number of streams using the adapter.
    pub sid_cnt: i32,
    /// Parameters currently applied to the tuner.
    pub params: TuneParams,
    /// Slave adapters currently using this master.
    pub used: [bool; MAX_ADAPTERS],
    /// Polarisation of the last tune (`-1` when unknown).
    pub old_pol: i32,
    /// Band of the last tune (`-1` when unknown).
    pub old_hiband: i32,
    /// Source of the last tune (`-1` when unknown).
    pub old_diseqc: i32,
    tuner: Box<dyn AdapterTuner>,
    pids: [PidEntry; MAX_PIDS],
    attached: BTreeSet<StreamId>,
    do_tune: bool,
    active_pids: i32,
    max_active_pids: i32,
    max_pids: i32,
    dvr: i32,
    pid_order: PidOrder,
    pub(crate) ca_mask: u64,
    pub(crate) active_pmt: [i32; MAX_PMT_FOR_ADAPTER],
    pub(crate) active_pmts: usize,
    pub(crate) pat_processed: bool,
    pub(crate) transponder_id: u16,
    pub(crate) pat_ver: i32,
    pub(crate) pat_filter: i32,
    pub(crate) sdt_filter: i32,
}

impl Adapter {
    fn new(config: AdapterConfig, tuner: Box<dyn AdapterTuner>) -> Self {
        let max_pids = config.max_pids;
        Self {
            config,
            master_sid: -1,
            sid_cnt: 0,
            params: TuneParams::default(),
            used: [false; MAX_ADAPTERS],
            old_pol: -1,
            old_hiband: -1,
            old_diseqc: -1,
            tuner,
            pids: std::array::from_fn(|_| PidEntry::default()),
            attached: BTreeSet::new(),
            do_tune: false,
            active_pids: 0,
            max_active_pids: 0,
            max_pids,
            dvr: -1,
            pid_order: PidOrder::default(),
            ca_mask: 0,
            active_pmt: [-1; MAX_PMT_FOR_ADAPTER],
            active_pmts: 0,
            pat_processed: false,
            transponder_id: 0,
            pat_ver: 0,
            pat_filter: -1,
            sdt_filter: -1,
        }
    }

    /// Replaces the tuner backend, mainly for tests.
    pub fn set_tuner(&mut self, tuner: Box<dyn AdapterTuner>) {
        self.tuner = tuner;
    }

    /// The adapter PID table (`ad->pids`), including inactive slots.
    pub fn pid_entries(&self) -> &[PidEntry] {
        &self.pids
    }

    /// PIDs currently active or about to be activated on this adapter.
    pub fn active_pids(&self) -> Vec<i32> {
        self.pids
            .iter()
            .filter(|entry| matches!(entry.state, PidState::Active | PidState::New))
            .map(|entry| entry.pid)
            .collect()
    }

    /// Active PIDs sharing the demux descriptor with the DVR (`dvb_demux_shared_pid_count`).
    pub fn demux_shared_pid_count(&self) -> i32 {
        self.pids
            .iter()
            .filter(|entry| entry.state == PidState::Active && entry.fd == self.dvr)
            .count() as i32
    }

    /// Sequence source used for PID subscription order.
    pub fn pid_order(&self) -> PidOrder {
        self.pid_order.clone()
    }
}

/// PID entry state (`PID_STATE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PidState {
    /// `PID_STATE_INACTIVE`.
    #[default]
    Inactive,
    /// `PID_STATE_ACTIVE`.
    Active,
    /// `PID_STATE_NEW`.
    New,
    /// `PID_STATE_DELETED`.
    Deleted,
}

/// One PID slot of an adapter (`SPid`).
#[derive(Debug, Clone)]
pub struct PidEntry {
    /// PID number.
    pub pid: i32,
    /// Owning PMT id, `-1` when unclaimed (`SPid.pmt`).
    pub pmt: i32,
    /// Section filter attached to this PID, `-1` when none (`SPid.filter`).
    pub filter: i32,
    /// Slot state.
    pub state: PidState,
    /// Descriptor of the PID filter, 0 when not open.
    pub fd: i32,
    /// Subscription order; 0 when no client stream owns the PID.
    pub order: u32,
    /// Packets received since the last update; `sort_pids` orders by this.
    pub packets: u32,
    /// Set once the first decrypted packet is seen (claim reset on steal).
    pub is_decrypted: bool,
    /// Streams currently owning this PID.
    pub sids: BTreeSet<StreamId>,
}

impl Default for PidEntry {
    fn default() -> Self {
        Self {
            pid: 0,
            pmt: -1,
            filter: -1,
            state: PidState::default(),
            fd: 0,
            order: 0,
            packets: 0,
            is_decrypted: false,
            sids: BTreeSet::new(),
        }
    }
}

impl PidEntry {
    /// Streams currently owning this PID.
    pub fn sids(&self) -> impl Iterator<Item = StreamId> + '_ {
        self.sids.iter().copied()
    }

    /// Whether `sid` owns this PID.
    pub fn has_stream(&self, sid: StreamId) -> bool {
        self.sids.contains(&sid)
    }
}

/// Adapter table and allocation policy.
pub struct Scheduler {
    adapters: Vec<Option<Adapter>>,
    fe_map: [i16; FE_MAP_LEN],
    disabled: [bool; MAX_ADAPTERS],
    absolute_switch: bool,
    pub(crate) pmt: pmt::PmtTables,
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::new()
    }
}

impl Scheduler {
    /// Creates an empty scheduler with every frontend unmapped.
    pub fn new() -> Self {
        let mut adapters = Vec::with_capacity(MAX_ADAPTERS);
        adapters.resize_with(MAX_ADAPTERS, || None);
        Self {
            adapters,
            fe_map: [-1; FE_MAP_LEN],
            disabled: [false; MAX_ADAPTERS],
            absolute_switch: false,
            pmt: pmt::PmtTables::new(),
        }
    }

    /// Installs an adapter at `aid`.
    pub fn add_adapter(&mut self, aid: usize, config: AdapterConfig, tuner: Box<dyn AdapterTuner>) {
        if aid < MAX_ADAPTERS {
            self.adapters[aid] = Some(Adapter::new(config, tuner));
        }
    }

    /// Removes the adapter at `aid` (`disable_satip_server`: `a[i]->type = 0`).
    pub fn remove_adapter(&mut self, aid: usize) -> Option<Adapter> {
        if aid < MAX_ADAPTERS {
            self.adapters[aid].take()
        } else {
            None
        }
    }

    /// Adapter at `aid`, initialized or not.
    pub fn adapter(&self, aid: usize) -> Option<&Adapter> {
        self.adapters.get(aid).and_then(Option::as_ref)
    }

    /// Mutable adapter at `aid`.
    pub fn adapter_mut(&mut self, aid: usize) -> Option<&mut Adapter> {
        self.adapters.get_mut(aid).and_then(Option::as_mut)
    }

    /// Maps frontend number `fe` (zero-based) to an adapter, or `-1`.
    pub fn set_fe_map_entry(&mut self, fe: usize, adapter: i16) {
        if fe < FE_MAP_LEN {
            self.fe_map[fe] = adapter;
        }
    }

    /// Marks every frontend as unmapped.
    pub fn clear_fe_map(&mut self) {
        self.fe_map = [-1; FE_MAP_LEN];
    }

    /// Enables the `--satip-abs` source mapping.
    pub fn set_absolute_switch(&mut self, enabled: bool) {
        self.absolute_switch = enabled;
    }

    /// Creates PAT->PMT filters with `FILTER_ADD_REMOVE` (`opts.pmt_scan`).
    pub fn set_pmt_scan(&mut self, enabled: bool) {
        self.pmt.set_pmt_scan(enabled);
    }

    /// Emulates `pids=all` from the PAT/PMT instead of opening pid 8192 (`-k`).
    pub fn set_emulate_pids_all(&mut self, enabled: bool) {
        self.pmt.set_emulate_pids_all(enabled);
    }

    /// `opts.drop_encrypted != 0`: the emulation leaves out the null pid.
    pub fn set_drop_encrypted(&mut self, enabled: bool) {
        self.pmt.drop_encrypted = enabled;
    }

    /// Sets the absolute-source table of `aid`.
    pub fn set_absolute_table(&mut self, aid: usize, table: [u8; MAX_SOURCES]) {
        if let Some(adapter) = self.adapter_mut(aid) {
            adapter.config.absolute_table = table;
        }
    }

    /// Marks an adapter as disabled for allocation.
    pub fn set_disabled(&mut self, aid: usize, disabled: bool) {
        if aid < MAX_ADAPTERS {
            self.disabled[aid] = disabled;
        }
    }

    /// Whether `aid` is disabled for allocation.
    pub fn is_disabled(&self, aid: usize) -> bool {
        self.disabled.get(aid).copied().unwrap_or(false)
    }

    fn enabled_adapter(&self, aid: i32) -> Option<&Adapter> {
        usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter(index))
            .filter(|adapter| adapter.enabled())
    }

    fn enabled_adapter_mut(&mut self, aid: i32) -> Option<&mut Adapter> {
        usize::try_from(aid)
            .ok()
            .and_then(|index| self.adapter_mut(index))
            .filter(|adapter| adapter.config.enabled)
    }

    fn init_hw(&mut self, index: usize) -> bool {
        if index >= MAX_ADAPTERS || self.disabled[index] {
            return false;
        }
        let Some(adapter) = self.adapters[index].as_mut() else {
            return false;
        };
        if !adapter.config.enabled {
            adapter.config.enabled = true;
            adapter.master_sid = -1;
            adapter.sid_cnt = 0;
            adapter.params = TuneParams::default();
            adapter.attached.clear();
        }
        true
    }

    /// Returns a free or same-mux adapter for `tp`.
    pub fn get_free_adapter(&mut self, tp: &TuneParams) -> Option<usize> {
        let mut fe = tp.fe.unwrap_or(0);
        if fe > 0 && (fe as usize) <= FE_MAP_LEN {
            let mapped = self.fe_map[fe as usize - 1];
            if mapped >= 0 && (mapped as usize) < MAX_ADAPTERS {
                fe = i32::from(mapped);
            } else {
                fe = -1;
            }
        } else {
            fe = -1;
        }
        // C++ keeps a default adapter for the log even when the frontend pin
        // does not resolve: `a[0]` unless another adapter is enabled.
        let mut chosen = self
            .adapters
            .first()
            .is_some_and(Option::is_some)
            .then_some(0);
        if let Some(index) = self
            .adapters
            .iter()
            .position(|slot| slot.as_ref().is_some_and(|adapter| adapter.config.enabled))
        {
            chosen = Some(index);
        }
        if fe >= 0 {
            chosen = Some(fe as usize);
        }
        match chosen.and_then(|index| {
            self.adapters[index]
                .as_ref()
                .map(|adapter| (index, adapter))
        }) {
            Some((index, adapter)) => ms_log::log!(
                "get free adapter {} - a[{}] => e:{} m:{} sid_cnt:{} src:{} f:{} pol={} sys: {} {}",
                tp.fe_value(),
                index,
                i32::from(adapter.config.enabled),
                adapter.master_sid,
                adapter.sid_cnt,
                adapter.params.src_value(),
                adapter.params.freq,
                adapter.params.pol_number(),
                adapter.config.sys[0].name(),
                adapter.config.sys[1].name()
            ),
            None => ms_log::log!(
                "get free adapter {} msys {} requested {}",
                fe,
                ms_tuner::delsys_name(fe),
                ms_tuner::delsys_name(tp.sys_number())
            ),
        }
        if fe >= 0 {
            let index = fe as usize;
            let usable = self.adapters[index].as_ref().is_some_and(|adapter| {
                delsys_match(adapter, tp.msys)
                    && (!adapter.config.enabled
                        || adapter.sid_cnt == 0
                        || !self.compare_tuning_parameters(fe, tp))
            });
            return (usable && self.init_hw(index)).then_some(index);
        }

        // Reuse an adapter already tuned to the same mux (C++ adapter.cpp:754-759).
        for index in 0..MAX_ADAPTERS {
            let Some(adapter) = self.adapters[index].as_ref() else {
                continue;
            };
            if !adapter.config.enabled
                || !delsys_match(adapter, tp.msys)
                || !self.source_enabled(index, tp)
            {
                continue;
            }
            if !self.compare_tuning_parameters(index as i32, tp) {
                return Some(index);
            }
        }
        // A free adapter, or an adapter slot that was never opened (adapter.cpp:761-774).
        for index in 0..MAX_ADAPTERS {
            let Some((enabled, sid_cnt, sys_match)) =
                self.adapters[index].as_ref().map(|adapter| {
                    (
                        adapter.config.enabled,
                        adapter.sid_cnt,
                        delsys_match(adapter, tp.msys),
                    )
                })
            else {
                continue;
            };
            let slave_ok = !self.compare_slave_parameters(index as i32, tp);
            let src_ok = self.source_enabled(index, tp);
            if enabled && sid_cnt == 0 && sys_match && src_ok && slave_ok {
                return Some(index);
            }
            if !enabled && sys_match && slave_ok && src_ok && self.init_hw(index) {
                return Some(index);
            }
        }
        // No regular tuner is free: a slave may still be usable (adapter.cpp:776-785).
        for index in 0..MAX_ADAPTERS {
            let Some(adapter) = self.adapters[index].as_ref() else {
                continue;
            };
            if adapter.config.enabled
                && adapter.sid_cnt == 0
                && delsys_match(adapter, tp.msys)
                && self.source_enabled(index, tp)
                && self.compare_slave_parameters(index as i32, tp)
            {
                ms_log::logm!("get free adapter found slave adapter {}", index);
                return Some(index);
            }
        }
        ms_log::log!(
            "no adapter found for src:{} f:{} pol:{} msys:{}",
            tp.src_value(),
            tp.freq,
            tp.pol_number(),
            tp.sys_number()
        );
        None
    }

    /// Out-of-range attribute for a failed allocation, or `None` when busy.
    pub fn out_of_range_attrs(&self, tp: &TuneParams) -> Option<&'static str> {
        let fe = tp.fe.unwrap_or(0);
        if fe > 0 && (fe as usize) <= FE_MAP_LEN {
            let mapped = i32::from(self.fe_map[fe as usize - 1]);
            if mapped >= 0 && (mapped as usize) < MAX_ADAPTERS {
                let index = mapped as usize;
                let Some(adapter) = self.adapters[index].as_ref() else {
                    return Some("fe");
                };
                if self.disabled[index] || !delsys_match(adapter, tp.msys) {
                    return Some("fe");
                }
                return (!self.source_enabled(index, tp)).then_some("src");
            }
        }

        let mut delsys_ok = false;
        let mut src_ok = false;
        for index in 0..MAX_ADAPTERS {
            let Some(adapter) = self.adapters[index].as_ref() else {
                continue;
            };
            if self.disabled[index] || !delsys_match(adapter, tp.msys) {
                continue;
            }
            delsys_ok = true;
            if self.source_enabled(index, tp) {
                src_ok = true;
            }
        }
        if !delsys_ok {
            Some("msys")
        } else if !src_ok {
            Some("src")
        } else {
            None
        }
    }

    /// Whether the adapter tuning differs from `tp`, as in the C++ `compare_tunning_parameters`.
    pub fn compare_tuning_parameters(&self, aid: i32, tp: &TuneParams) -> bool {
        let Some(adapter) = self.enabled_adapter(aid) else {
            return true;
        };
        ms_log::logm!(
            "new parameters: f:{}, plp/isi:{}, diseqc:{}, pol:{}, sr:{}, mtype:{}",
            tp.freq,
            tp.plp.unwrap_or(-1),
            tp.src_value(),
            tp.pol_number(),
            tp.sr_value(),
            tp.mtype_value()
        );
        ms_log::logm!(
            "old parameters: f:{}, plp/isi:{}, diseqc:{}, pol:{}, sr:{}, mtype:{}",
            adapter.params.freq,
            adapter.params.plp.unwrap_or(-1),
            adapter.params.src_value(),
            adapter.params.pol_number(),
            adapter.params.sr_value(),
            adapter.params.mtype_value()
        );
        if (i64::from(tp.freq) - i64::from(adapter.params.freq)).abs() > 1_000 {
            return true;
        }
        if tp.plp != adapter.params.plp {
            return true;
        }
        let Ok(index) = usize::try_from(aid) else {
            return true;
        };
        let mapped = self.absolute_source(index, tp.src, tp.msys);
        let old_src = adapter
            .params
            .src_specified
            .then(|| i32::from(adapter.params.src));
        if old_src != Some(mapped) {
            return true;
        }
        if tp.pol != Polarization::None && tp.pol != adapter.params.pol {
            return true;
        }
        if tp.sr > 1_000 && adapter.params.sr > 1_000 && tp.sr != adapter.params.sr {
            return true;
        }
        if !mtype_is_auto(tp) && !mtype_is_auto(&adapter.params) && tp.mtype != adapter.params.mtype
        {
            return true;
        }
        false
    }

    /// Whether the pinned frontend, when present, belongs to `aid`.
    pub fn fe_pin_matches_adapter(&self, aid: i32, tp: &TuneParams) -> bool {
        let fe = tp.fe.unwrap_or(0);
        if fe <= 0 || (fe as usize) > FE_MAP_LEN {
            return true;
        }
        let mapped = i32::from(self.fe_map[fe as usize - 1]);
        mapped < 0 || (mapped as usize) >= MAX_ADAPTERS || mapped == aid
    }

    /// Whether a slave tuner cannot follow `tp` because of LNB/DiSEqC state.
    ///
    /// Mirrors `compare_slave_parameters` (adapter.cpp:617): unicable/JESS
    /// adapters ignore band and polarisation, and the band threshold comes from
    /// the master LNB when this adapter has one.
    pub fn compare_slave_parameters(&self, aid: i32, tp: &TuneParams) -> bool {
        let Some(adapter) = self.enabled_adapter(aid) else {
            return false;
        };
        if adapter.config.master_source.is_none() && !adapter.used.iter().any(|used| *used) {
            return false;
        }
        if adapter.config.switch_type.is_single_cable() {
            return false;
        }
        let pol = (polarization_code(tp) - 1) & 1;
        let diseqc = if tp.src_specified && tp.src > 0 {
            i32::from(tp.src) - 1
        } else {
            0
        };
        let lnb = adapter
            .config
            .master_source
            .and_then(|master| self.adapters.get(master).and_then(Option::as_ref))
            .map_or(&adapter.config, |master| &master.config);
        let hiband = (tp.freq > 0).then(|| lnb_hiband(lnb, tp.pol, tp.freq));
        for (index, used) in adapter.used.iter().enumerate() {
            if !used {
                continue;
            }
            if let Some(other) = self.enabled_adapter(index as i32)
                && incompatible(other, pol, hiband, diseqc)
            {
                return true;
            }
        }
        if let Some(master) = adapter.config.master_source
            && let Some(other) = self.enabled_adapter(master as i32)
            && incompatible(other, pol, hiband, diseqc)
        {
            return true;
        }
        ms_log::logm!(
            "compare_slave_parameters: adapter {} master {} (pol {}, band {}, diseqc {}) compatible with freq {}, pol {} band {} diseqc {}",
            aid,
            adapter
                .config
                .master_source
                .map_or(-1, |master| master as i32),
            adapter.old_pol,
            adapter.old_hiband,
            adapter.old_diseqc,
            tp.freq,
            pol,
            hiband.unwrap_or(-1),
            diseqc
        );
        false
    }

    fn source_enabled(&self, aid: usize, tp: &TuneParams) -> bool {
        if !self.absolute_switch || !matches!(tp.msys, DeliverySystem::DvbS | DeliverySystem::DvbS2)
        {
            return true;
        }
        let src = i32::from(tp.src);
        if src <= 0 || (src as usize) > MAX_SOURCES {
            return true;
        }
        self.adapters[aid]
            .as_ref()
            .is_some_and(|adapter| adapter.config.absolute_table[src as usize - 1] != 0)
    }

    fn absolute_source(&self, aid: usize, src: u8, msys: DeliverySystem) -> i32 {
        if !self.absolute_switch || !matches!(msys, DeliverySystem::DvbS | DeliverySystem::DvbS2) {
            return i32::from(src);
        }
        let src = i32::from(src);
        if src <= 0 || (src as usize) > MAX_SOURCES {
            return src;
        }
        self.adapters[aid].as_ref().map_or(-1, |adapter| {
            let mapped = i32::from(adapter.config.absolute_table[src as usize - 1]);
            ms_log::log!("Mapping adapter {} src {} to src {}", aid, src, mapped);
            mapped
        })
    }

    /// Parents `sid` to `aid`, making it master on first use.
    pub fn set_adapter_for_stream(&mut self, sid: StreamId, aid: i32) -> bool {
        let Ok(index) = usize::try_from(aid) else {
            return false;
        };
        let Some(adapter) = self.enabled_adapter_mut(aid) else {
            return false;
        };
        if adapter.master_sid == -1 {
            adapter.master_sid = sid;
        }
        if adapter.sid_cnt == 0 {
            adapter.params.freq = 0;
        }
        adapter.sid_cnt += 1;
        adapter.attached.insert(sid);
        let (master_sid, sid_cnt) = (adapter.master_sid, adapter.sid_cnt);
        let master_source = adapter.config.master_source;
        if let Some(master) = master_source
            && let Some(master) = self.adapters.get_mut(master).and_then(Option::as_mut)
        {
            master.used[index] = true;
        }
        ms_log::log!(
            "set adapter {} for sid {} m:{} s:{}",
            aid,
            sid,
            master_sid,
            sid_cnt
        );
        true
    }

    /// Releases `sid` from `aid`; the last stream clears the adapter.
    pub fn close_adapter_for_stream(&mut self, sid: StreamId, aid: i32) {
        let Ok(index) = usize::try_from(aid) else {
            return;
        };
        let Some(adapter) = self.enabled_adapter_mut(aid) else {
            return;
        };
        adapter.attached.remove(&sid);
        let needs_fix = adapter.master_sid == sid;
        if needs_fix {
            adapter.master_sid = -1;
        }
        if adapter.sid_cnt > 0 {
            adapter.sid_cnt -= 1;
        }
        let count = adapter.sid_cnt;
        if needs_fix {
            self.fix_master_sid(index);
        }
        let master_sid = self.adapters[index]
            .as_ref()
            .map_or(-1, |adapter| adapter.master_sid);
        ms_log::log!(
            "closed adapter {} for sid {} m:{} sid_cnt:{}",
            aid,
            sid,
            master_sid,
            count
        );
        if count == 0 {
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.master_sid = -1;
                adapter.params = TuneParams::default();
            }
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
            let master_source = self.adapters[index]
                .as_ref()
                .and_then(|adapter| adapter.config.master_source);
            if let Some(master) = master_source {
                if let Some(master) = self.adapters.get_mut(master).and_then(Option::as_mut) {
                    master.used[index] = false;
                }
                ms_log::logm!("adapter {} freed from slave adapter {}", master, index);
            }
        } else {
            self.mark_pid_deleted(index, sid, None);
        }
        self.update_pids(index);
    }

    fn fix_master_sid(&mut self, index: usize) {
        let Some(adapter) = self.adapters[index].as_mut() else {
            return;
        };
        if adapter.master_sid != -1 || adapter.sid_cnt < 1 {
            return;
        }
        for sid in adapter.attached.clone() {
            adapter.master_sid = sid;
        }
    }

    /// Applies `tp` to `aid`, retuning when the parameters differ.
    pub fn set_adapter_parameters(
        &mut self,
        aid: i32,
        sid: StreamId,
        tp: &TuneParams,
    ) -> Result<(), SetParamsError> {
        let Ok(index) = usize::try_from(aid) else {
            return Err(SetParamsError::MissingAdapter);
        };
        let Some(adapter) = self.enabled_adapter_mut(aid) else {
            return Err(SetParamsError::MissingAdapter);
        };
        ms_log::log!(
            "setting DVB parameters for adapter {} - master_sid {} sid {} old f:{}",
            aid,
            adapter.master_sid,
            sid,
            adapter.params.freq
        );
        if adapter.master_sid == -1 {
            adapter.master_sid = sid;
        }
        adapter.do_tune = false;
        if self.compare_tuning_parameters(aid, tp) {
            if self.enabled_adapter(aid).map(|a| a.master_sid) != Some(sid) {
                let old = &self.enabled_adapter(aid).expect("adapter checked").params;
                ms_log::log!(
                    "secondary stream requested tune, not gonna happen ad: f:{} sr:{} pol:{} plp/isi:{} src:{} mod {} -> \t\t\tnew: f:{} sr:{} pol:{} plp/isi:{} src:{} mod {}",
                    old.freq,
                    old.sr_value(),
                    old.pol_number(),
                    old.plp.unwrap_or(0),
                    old.src_value(),
                    old.mtype_value(),
                    tp.freq,
                    tp.sr_value(),
                    tp.pol_number(),
                    tp.plp.unwrap_or(0),
                    tp.src_value(),
                    tp.mtype_value()
                );
                return Err(SetParamsError::SlaveConflict);
            }
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.do_tune = true;
            }
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
            if self.update_pids(index) != 0 {
                return Err(SetParamsError::PidFailure);
            }
        }
        let Some(adapter) = self.adapters[index].as_mut() else {
            return Err(SetParamsError::MissingAdapter);
        };
        // copy_dvb_parameters (dvb.cpp:393-471): the start line shows the
        // destination before the copy, the end line the merged result.
        log_copy_start(&adapter.params);
        adapter.params = tp.clone();
        // The C++ copy forces switch position 1 and the DVB-S/S2 modulation
        // defaults on the destination; from_params applies them later.
        let mut logged = adapter.params.clone();
        if logged.src_value() < 1 {
            logged.src = 1;
            logged.src_specified = true;
        }
        if logged.msys == DeliverySystem::DvbS2 && !logged.mtype_specified {
            logged.mtype = 9;
            logged.mtype_specified = true;
        }
        if logged.msys == DeliverySystem::DvbS && !logged.mtype_specified {
            logged.mtype = 0;
            logged.mtype_specified = true;
        }
        log_copy_end(&logged);
        let need_mapping = adapter.params.fe.unwrap_or(0) < 1;
        let (src, msys) = (adapter.params.src, adapter.params.msys);
        if need_mapping {
            let mapped = self.absolute_source(index, src, msys);
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.params.src = u8::try_from(mapped).unwrap_or(0);
                adapter.params.src_specified = true;
            }
        }
        let params = self.adapters[index]
            .as_ref()
            .map(|adapter| adapter.params.clone())
            .unwrap_or_default();
        if !self.sync_pids(sid, aid, &params) {
            return Err(SetParamsError::PidFailure);
        }
        Ok(())
    }

    /// Runs a pending tune for the master stream `sid`.
    pub fn tune(&mut self, aid: i32, sid: StreamId) -> TuneOutcome {
        // ms-sched has no DVR socket; C++ prints `-1` when the adapter is absent.
        ms_log::logm!("adapter tune: sid {} aid {} => sock: {} ", sid, aid, -1);
        let mut close_streams = Vec::new();
        let Ok(index) = usize::try_from(aid) else {
            return TuneOutcome {
                result: Err(TuneError::Hardware),
                close_streams,
            };
        };
        let Some(adapter) = self.enabled_adapter(aid) else {
            return TuneOutcome {
                result: Err(TuneError::Hardware),
                close_streams,
            };
        };
        let mut result = Ok(());
        if adapter.master_sid == sid && adapter.do_tune {
            let params = adapter.params.clone();
            let config = adapter.config.clone();
            let slaves: Vec<StreamId> = if adapter.sid_cnt > 1 {
                adapter
                    .attached
                    .iter()
                    .copied()
                    .filter(|attached| *attached != sid)
                    .collect()
            } else {
                Vec::new()
            };
            // setup_switch records the switch position before tuning
            // (dvb.cpp:1224-1232); a slave also updates its master's cache.
            if matches!(params.msys, DeliverySystem::DvbS | DeliverySystem::DvbS2) {
                let pol = (polarization_code(&params) - 1) & 1;
                let diseqc = if params.src_specified && params.src > 0 {
                    i32::from(params.src) - 1
                } else {
                    0
                };
                let hiband = lnb_hiband(&config, params.pol, params.freq);
                if let Some(adapter) = self.adapters[index].as_mut() {
                    adapter.old_pol = pol;
                    adapter.old_hiband = hiband;
                    adapter.old_diseqc = diseqc;
                }
                if let Some(master) = config.master_source
                    && let Some(master) = self.adapters.get_mut(master).and_then(Option::as_mut)
                {
                    master.old_pol = pol;
                    master.old_hiband = hiband;
                    master.old_diseqc = diseqc;
                }
            }
            if let Some(adapter) = self.adapters[index].as_mut() {
                result = adapter.tuner.tune(&params);
            }
            // C++ closes the slaves before post_tune, so their PIDs are gone first.
            for slave in &slaves {
                self.close_adapter_for_stream(*slave, aid);
            }
            close_streams = slaves;
            self.post_tune(index);
        } else if let Some(adapter) = self.enabled_adapter(aid) {
            ms_log::log!(
                "not tuning for sid {} (do_tune={}, master_sid={})",
                sid,
                i32::from(adapter.do_tune),
                adapter.master_sid
            );
        }
        if result.is_err() {
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
        }
        if self.update_pids(index) != 0 && result.is_ok() {
            result = Err(TuneError::Pids);
        }
        if let Some(adapter) = self.adapters[index].as_mut() {
            adapter.do_tune = false;
        }
        TuneOutcome {
            result,
            close_streams,
        }
    }

    /// Active PIDs belonging to `sid`, in table order (C++ `get_stream_pids`).
    pub fn stream_pids(&self, aid: i32, sid: StreamId) -> Vec<i32> {
        let Some(adapter) = self.enabled_adapter(aid) else {
            return Vec::new();
        };
        adapter
            .pids
            .iter()
            .filter(|entry| entry.state == PidState::Active && entry.sids.contains(&sid))
            .map(|entry| entry.pid)
            .collect()
    }

    /// Adds `pid` to `aid`'s table for `sid`; `-1` on rejection, `0` on success.
    pub fn mark_pid_add(&mut self, aid: i32, sid: StreamId, pid: i32) -> i32 {
        if !(0..=PID_ALL).contains(&pid) {
            return -1;
        }
        let Some(adapter) = self.enabled_adapter_mut(aid) else {
            return -1;
        };
        let pid_order = adapter.pid_order.clone();
        if let Some(entry) = adapter
            .pids
            .iter_mut()
            .find(|entry| entry.state != PidState::Inactive && entry.pid == pid)
        {
            ms_log::log!(
                "found already existing pid {} flags {}",
                pid,
                entry.state as i32
            );
            if sid != PID_STREAM_ID_UNDEFINED {
                let had_client = !entry.sids.is_empty();
                entry.sids.insert(sid);
                if !had_client && !entry.sids.is_empty() {
                    entry.order = pid_order.next_pid_order();
                }
            }
            if entry.state == PidState::Deleted {
                entry.state = PidState::New;
            }
            return 0;
        }
        let Some(entry) = adapter
            .pids
            .iter_mut()
            .find(|entry| entry.state == PidState::Inactive)
        else {
            ms_log::log!(
                "MAX_PIDS ({}) reached for adapter {} in adding PID: {}",
                MAX_PIDS,
                aid,
                pid
            );
            return -1;
        };
        entry.pid = pid;
        entry.pmt = -1;
        entry.filter = -1;
        entry.state = PidState::New;
        entry.fd = 0;
        entry.order = 0;
        entry.packets = 0;
        entry.is_decrypted = false;
        entry.sids.clear();
        if sid != PID_STREAM_ID_UNDEFINED {
            entry.sids.insert(sid);
        }
        if !entry.sids.is_empty() {
            entry.order = pid_order.next_pid_order();
        }
        0
    }

    /// Finds a non-inactive PID entry, scanning the whole table
    /// (`find_pid`, adapter.cpp:1137).
    pub fn find_pid(&self, aid: i32, pid: i32) -> Option<&PidEntry> {
        let adapter = self.enabled_adapter(aid)?;
        adapter
            .pids
            .iter()
            .find(|entry| entry.state != PidState::Inactive && entry.pid == pid)
    }

    /// Non-inactive PID entries of an enabled adapter, in table order.
    pub fn active_pids(&self, aid: i32) -> impl Iterator<Item = &PidEntry> {
        self.enabled_adapter(aid)
            .into_iter()
            .flat_map(|adapter| adapter.pids.iter())
            .filter(|entry| entry.state != PidState::Inactive)
    }

    /// Mutable PID entry lookup, same scan as [`Self::find_pid`].
    pub fn find_pid_mut(&mut self, aid: i32, pid: i32) -> Option<&mut PidEntry> {
        let adapter = self.enabled_adapter_mut(aid)?;
        adapter
            .pids
            .iter_mut()
            .find(|entry| entry.state != PidState::Inactive && entry.pid == pid)
    }

    fn mark_pid_deleted(&mut self, index: usize, sid: StreamId, pid: Option<i32>) {
        let mut deleted = Vec::new();
        {
            let Some(adapter) = self.adapters[index].as_mut() else {
                return;
            };
            for entry in &mut adapter.pids {
                if entry.state == PidState::Inactive {
                    continue;
                }
                if pid.is_some_and(|pid| entry.pid != pid) {
                    continue;
                }
                let was_live = entry.state != PidState::Deleted;
                if sid == PID_STREAM_ID_UNDEFINED {
                    entry.state = PidState::Deleted;
                    entry.sids.clear();
                    entry.order = 0;
                } else {
                    entry.sids.remove(&sid);
                    if entry.sids.is_empty() {
                        entry.state = PidState::Deleted;
                        entry.order = 0;
                    }
                }
                if was_live && entry.state == PidState::Deleted {
                    deleted.push(entry.pid);
                }
            }
        }
        // C++ mark_pid_deleted() closes CA PIDs and stops PMTs that lost
        // their last live stream before update_pids() runs.
        for pid in deleted {
            self.pmt_pid_del(index, pid);
        }
    }

    fn sync_pids(&mut self, sid: StreamId, aid: i32, tp: &TuneParams) -> bool {
        let Ok(index) = usize::try_from(aid) else {
            return false;
        };
        let Some(adapter) = self.enabled_adapter(aid) else {
            return false;
        };
        let to_delete: Vec<i32> = adapter
            .pids
            .iter()
            .filter(|entry| {
                entry.state != PidState::Inactive
                    && entry.sids.contains(&sid)
                    && !tp.pids.iter().any(|pid| i32::from(pid) == entry.pid)
            })
            .map(|entry| entry.pid)
            .collect();
        for pid in to_delete {
            self.mark_pid_deleted(index, sid, Some(pid));
        }
        for pid in tp.pids.iter() {
            let pid = i32::from(pid);
            let present = self.enabled_adapter(aid).is_some_and(|adapter| {
                adapter.pids.iter().any(|entry| {
                    entry.state != PidState::Inactive
                        && entry.pid == pid
                        && entry.sids.contains(&sid)
                })
            });
            if !present && self.mark_pid_add(aid, sid, pid) < 0 {
                return false;
            }
        }
        true
    }

    fn post_tune(&mut self, index: usize) {
        ms_log::logm!("adapter post_tune: aid {}", index);
        let Some(adapter) = self.adapters[index].as_ref() else {
            return;
        };
        let all_active =
            |state: PidState| state != PidState::Inactive && state != PidState::Deleted;
        let ci = adapter.config.adapter_type == AdapterType::Ci;
        if !adapter
            .pids
            .iter()
            .any(|entry| all_active(entry.state) && entry.pid == PID_ALL)
        {
            let existing: Vec<i32> = adapter
                .pids
                .iter()
                .filter(|entry| all_active(entry.state))
                .map(|entry| entry.pid)
                .collect();
            // CI adapters only need pid 0, for the generated PAT (adapter.cpp:1046-1061).
            for pid in [0, 1, 16, 18] {
                if !existing.contains(&pid) && (!ci || pid == 0) {
                    ms_log::log!(
                        "Adding pid {} to the list of pids as not explicitly added for adapter {}",
                        pid,
                        index
                    );
                    let _ = self.mark_pid_add(index as i32, PID_STREAM_ID_UNDEFINED, pid);
                }
            }
        }
        self.pmt_tune(index as i32);
    }

    /// `post_tune` for an adapter that is never tuned (`ddci_post_init`).
    pub fn post_tune_adapter(&mut self, aid: usize) {
        if self.adapters.get(aid).is_some_and(Option::is_some) {
            self.post_tune(aid);
            self.update_pids(aid);
        }
    }

    /// Transport stream id of the adapter's last PAT (`ad->transponder_id`).
    pub fn adapter_transponder_id(&self, aid: usize) -> u16 {
        self.adapter(aid)
            .map_or(0, |adapter| adapter.transponder_id)
    }

    /// Drops `sid`'s claim on one PID (`mark_pid_deleted`); apply it with
    /// [`Self::update_adapter_pids`].
    pub fn unmark_pid(&mut self, aid: usize, sid: StreamId, pid: i32) {
        if self.adapters.get(aid).is_some_and(Option::is_some) {
            self.mark_pid_deleted(aid, sid, Some(pid));
        }
    }

    /// Applies pending PID changes of one adapter, as callers of the C++
    /// `update_pids(aid)` do after adding filters. Returns 0 on success.
    pub fn update_adapter_pids(&mut self, aid: usize) -> i32 {
        if self.adapters.get(aid).is_none_or(Option::is_none) {
            return 1;
        }
        self.update_pids(aid)
    }

    /// Applies pending DELETED/NEW entries and sorts the table
    /// (`update_pids`, adapter.cpp:928-1047). Returns 0 on success, 1 on error.
    fn update_pids(&mut self, index: usize) -> i32 {
        ms_log::logm!("Updating pids for adapter {}", index);
        let mut rv = 0;
        // pids=all expansion must precede the NEW->ACTIVE pass below, so the
        // PMT/stream pids it adds can start the PMT in this same update.
        self.emulate_add_all_pids(index);
        let emulate = self.pmt.emulate_pids_all;
        {
            let Some(adapter) = self.adapters[index].as_mut() else {
                return 1;
            };
            for slot in (0..MAX_PIDS).rev() {
                if adapter.pids[slot].state != PidState::Deleted {
                    continue;
                }
                if adapter.pids[slot].fd > 0 {
                    if adapter.active_pids > 0 {
                        adapter.active_pids -= 1;
                    }
                    let (fd, pid) = (adapter.pids[slot].fd, adapter.pids[slot].pid);
                    if adapter.tuner.del_filters(fd, pid).is_err() {
                        ms_log::log0!("failed to remove pid {}, reporting error", pid);
                        rv = 1;
                    }
                }
                // Soft cleanup: C++ keeps `pid` and clears the rest.
                let entry = &mut adapter.pids[slot];
                entry.fd = 0;
                entry.order = 0;
                entry.state = PidState::Inactive;
                entry.packets = 0;
            }

            for slot in 0..MAX_PIDS {
                if adapter.pids[slot].state != PidState::New {
                    continue;
                }
                if adapter.max_pids != 0 && adapter.max_pids < adapter.active_pids {
                    ms_log::log!(
                        "maximum number of pids {} out of {} reached",
                        adapter.active_pids,
                        adapter.max_pids
                    );
                    break;
                }
                if adapter.pids[slot].fd <= 0 {
                    let pid = adapter.pids[slot].pid;
                    // adapter.cpp:997: emulation opens the PAT-derived pids instead.
                    let opened = if emulate && pid == PID_ALL {
                        Ok(-1)
                    } else {
                        adapter.tuner.set_pid(pid)
                    };
                    match opened {
                        Ok(fd) => adapter.pids[slot].fd = fd,
                        Err(_) => {
                            adapter.max_pids = adapter.max_active_pids - 1;
                            if adapter.max_pids < MIN_ADAPTER_PIDS {
                                adapter.max_pids = MIN_ADAPTER_PIDS;
                            }
                            ms_log::log0!(
                                "Maximum pid filter reached, lowering the value to {}",
                                adapter.max_pids
                            );
                            if adapter.active_pids < MIN_ADAPTER_PIDS {
                                ms_log::log0!(
                                    "failed to add pid {} with only {} active pids, reporting error",
                                    pid,
                                    adapter.active_pids
                                );
                                rv = 1;
                            }
                            break;
                        }
                    }
                    adapter.active_pids += 1;
                    if adapter.max_active_pids < adapter.active_pids {
                        adapter.max_active_pids = adapter.active_pids;
                    }
                }
                adapter.pids[slot].state = PidState::Active;
                adapter.pids[slot].packets = 0;
            }

            // The C++ drives each PID through set_pid/del_filters above; also
            // hand the resulting active set to backends that filter in bulk.
            let pids = PidSet::from_pids(
                adapter
                    .pids
                    .iter()
                    .filter(|entry| entry.state == PidState::Active && entry.fd > 0)
                    .filter_map(|entry| u16::try_from(entry.pid).ok()),
            );
            if adapter.tuner.set_pids(&pids).is_err() {
                rv = 1;
            }
        }
        self.sort_pids(index);
        // C++ update_pids() tail: settle claims and PMT states even when no
        // demux pass follows. CAPMTs are sent by start_active_pmts instead.
        self.pmt_pid_updated_pids(index);
        rv
    }

    /// Bubble-sorts active entries to the front, highest packet count first
    /// (`sort_pids`, adapter.cpp:1565).
    fn sort_pids(&mut self, index: usize) {
        let Some(adapter) = self.adapters[index].as_mut() else {
            return;
        };
        let pids = &mut adapter.pids;
        loop {
            let mut swapped = false;
            for slot in 0..MAX_PIDS - 1 {
                if pids[slot + 1].state == PidState::Inactive {
                    continue;
                }
                if pids[slot].state == PidState::Inactive
                    || pids[slot].packets < pids[slot + 1].packets
                {
                    pids.swap(slot, slot + 1);
                    swapped = true;
                }
            }
            if !swapped {
                break;
            }
        }
    }
}

impl Adapter {
    fn enabled(&self) -> bool {
        self.config.enabled
    }
}

fn delsys_match(adapter: &Adapter, msys: DeliverySystem) -> bool {
    if msys == DeliverySystem::Undefined {
        return false;
    }
    adapter.config.sys.contains(&msys)
}

fn mtype_is_auto(tp: &TuneParams) -> bool {
    !tp.mtype_specified || tp.mtype == 6
}

/// `tp.pol.value_or(-1)`: an explicit `pol=0` is distinct from an absent one.
fn polarization_code(tp: &TuneParams) -> i32 {
    match tp.pol {
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
        Polarization::None if tp.pol_specified => 0,
        Polarization::None => -1,
    }
}

/// `tp.pol.value_or(0)`, used by the LNB band computation.
fn lnb_polarization_code(pol: Polarization) -> i32 {
    match pol {
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
        Polarization::None => 0,
    }
}

fn lnb_hiband(config: &AdapterConfig, pol: Polarization, freq: i32) -> i32 {
    if lnb_polarization_code(pol) > 2 && config.lnb_circular > 0 {
        return 0;
    }
    i32::from(config.lnb_switch > 0 && i64::from(freq) > i64::from(config.lnb_switch))
}

fn incompatible(adapter: &Adapter, pol: i32, hiband: Option<i32>, diseqc: i32) -> bool {
    pol != adapter.old_pol
        || hiband.is_some_and(|hiband| hiband != adapter.old_hiband)
        || diseqc != adapter.old_diseqc
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::BTreeSet;
    use std::rc::Rc;

    use super::*;

    struct OkTuner;

    impl AdapterTuner for OkTuner {
        fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
            Ok(())
        }
    }

    fn params(freq: i32, src: u8) -> TuneParams {
        TuneParams {
            freq,
            src,
            src_specified: true,
            msys: DeliverySystem::DvbS2,
            pol: Polarization::Horizontal,
            sr: 27_500_000,
            pids: PidSet::new(),
            ..TuneParams::default()
        }
    }

    fn scheduler() -> Scheduler {
        let mut scheduler = Scheduler::new();
        scheduler.add_adapter(
            0,
            AdapterConfig::new(DeliverySystem::DvbS2),
            Box::new(OkTuner),
        );
        scheduler
    }

    fn add_ok(scheduler: &mut Scheduler, aid: usize, config: AdapterConfig) {
        scheduler.add_adapter(aid, config, Box::new(OkTuner));
    }

    fn add_sat(scheduler: &mut Scheduler, aid: usize) {
        add_ok(scheduler, aid, AdapterConfig::new(DeliverySystem::DvbS2));
    }

    fn sat_params(freq: i32, pol: Polarization) -> TuneParams {
        TuneParams {
            freq,
            pol,
            src: 1,
            src_specified: true,
            msys: DeliverySystem::DvbS2,
            sr: 27_500_000,
            ..TuneParams::default()
        }
    }

    type RecordedTunes = Rc<RefCell<Vec<TuneParams>>>;
    type RecordedPidSets = Rc<RefCell<Vec<BTreeSet<i32>>>>;

    /// Records `tune` and `set_pids` calls; `result` overrides the tune result.
    struct RecordingTuner {
        tunes: RecordedTunes,
        pid_sets: RecordedPidSets,
        result: Option<TuneError>,
    }

    impl AdapterTuner for RecordingTuner {
        fn tune(&mut self, params: &TuneParams) -> Result<(), TuneError> {
            self.tunes.borrow_mut().push(params.clone());
            self.result.map_or(Ok(()), Err)
        }

        fn set_pids(&mut self, pids: &PidSet) -> Result<(), TuneError> {
            self.pid_sets
                .borrow_mut()
                .push(pids.iter().map(i32::from).collect());
            Ok(())
        }
    }

    fn recording_scheduler() -> (Scheduler, RecordedTunes, RecordedPidSets) {
        let tunes = Rc::new(RefCell::new(Vec::new()));
        let pid_sets = Rc::new(RefCell::new(Vec::new()));
        let mut scheduler = Scheduler::new();
        add_ok(&mut scheduler, 0, AdapterConfig::new(DeliverySystem::DvbS2));
        scheduler
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(RecordingTuner {
                tunes: Rc::clone(&tunes),
                pid_sets: Rc::clone(&pid_sets),
                result: None,
            }));
        (scheduler, tunes, pid_sets)
    }

    fn active_pids(scheduler: &Scheduler) -> BTreeSet<i32> {
        scheduler
            .adapter(0)
            .map(|adapter| adapter.active_pids().into_iter().collect())
            .unwrap_or_default()
    }

    #[test]
    fn same_mux_selects_busy_adapter() {
        let mut scheduler = scheduler();
        let tp = params(11_362_000, 1);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());

        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        let other = params(12_543_000, 1);
        assert_eq!(scheduler.get_free_adapter(&other), None);
        assert_eq!(scheduler.out_of_range_attrs(&other), None);
    }

    #[test]
    fn unsupported_delivery_system_reports_msys() {
        let mut scheduler = scheduler();
        let mut tp = params(538_000, 0);
        tp.msys = DeliverySystem::DvbT2;
        assert_eq!(scheduler.get_free_adapter(&tp), None);
        assert_eq!(scheduler.out_of_range_attrs(&tp), Some("msys"));
    }

    #[test]
    fn unmapped_src_reports_src() {
        let mut scheduler = scheduler();
        scheduler.set_absolute_switch(true);
        let tp = params(11_362_000, 3);
        assert_eq!(scheduler.get_free_adapter(&tp), None);
        assert_eq!(scheduler.out_of_range_attrs(&tp), Some("src"));
    }

    #[test]
    fn frequency_tolerance_is_one_mhz() {
        let mut scheduler = scheduler();
        let tp = params(11_362_000, 1);
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());

        let mut near = tp.clone();
        near.freq = 11_363_000;
        assert!(
            !scheduler.compare_tuning_parameters(0, &near),
            "a 1000 kHz difference is the same mux"
        );
        near.freq = 11_362_999;
        assert!(!scheduler.compare_tuning_parameters(0, &near));
        near.freq = 11_363_001;
        assert!(
            scheduler.compare_tuning_parameters(0, &near),
            "1001 kHz is a different mux"
        );
        near.freq = 11_360_999;
        assert!(
            scheduler.compare_tuning_parameters(0, &near),
            "the threshold is symmetric"
        );
    }

    #[test]
    fn symbol_rate_comparison_uses_sym_per_second_threshold() {
        let mut scheduler = scheduler();
        let tp = params(11_362_000, 1);
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());

        let mut other = tp.clone();
        other.sr = 22_000_000;
        assert!(scheduler.compare_tuning_parameters(0, &other));

        other.sr = 1_000;
        assert!(
            !scheduler.compare_tuning_parameters(0, &other),
            "symbol rates at or below 1000 sym/s are not compared"
        );
    }

    #[test]
    fn pid_table_capacity_and_release() {
        let mut scheduler = scheduler();
        let tp = params(11_362_000, 1);
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());
        assert!(scheduler.tune(0, 0).result.is_ok());

        let mut added = 0;
        let mut pid = 100;
        while pid < 8000 && scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, pid) == 0 {
            added += 1;
            pid += 1;
        }
        assert_eq!(added, MAX_PIDS - 4, "four default pids are already active");

        scheduler.close_adapter_for_stream(0, 0);
        assert_eq!(
            scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, 5000),
            0,
            "releasing the adapter must free the PID table"
        );
    }

    /// PID hooks that can be made to fail, like the C++ `set_pid`/`del_filters`
    /// test callbacks in test_adapter.cpp.
    struct FailingTuner {
        set_pid_fails: bool,
        del_filters_fails: bool,
    }

    impl AdapterTuner for FailingTuner {
        fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
            Ok(())
        }

        fn set_pid(&mut self, _pid: i32) -> Result<i32, TuneError> {
            if self.set_pid_fails {
                Err(TuneError::Hardware)
            } else {
                Ok(1)
            }
        }

        fn del_filters(&mut self, _fd: i32, _pid: i32) -> Result<(), TuneError> {
            if self.del_filters_fails {
                Err(TuneError::Hardware)
            } else {
                Ok(())
            }
        }
    }

    // Ported from tests/test_adapter.cpp:125
    #[test]
    fn test_update_pids() {
        let mut scheduler = scheduler();
        {
            let adapter = scheduler.adapter_mut(0).unwrap();
            for i in 0..8 {
                adapter.pids[i].state = match i % 4 {
                    0 => PidState::Inactive,
                    1 => PidState::Active,
                    2 => PidState::New,
                    _ => PidState::Deleted,
                };
                adapter.pids[i].pid = i as i32;
                adapter.pids[i].packets = i as u32;
            }
        }
        scheduler.update_pids(0);
        let adapter = scheduler.adapter(0).unwrap();
        assert_eq!(
            adapter.pids[0].pid, 5,
            "Expected pid with flags = 1 and largest number of packets"
        );
        assert_eq!(
            adapter.pids[1].pid, 1,
            "Expected pid with flags = 1 and second largest number of packets"
        );
        assert!(
            adapter.pids[2].pid == 2 && adapter.pids[2].packets == 0,
            "Expected pid with flags = 1 and 0 packets"
        );
        assert!(
            adapter.pids[3].pid == 6 && adapter.pids[2].packets == 0,
            "Expected pid with flags = 1 and 0 packets"
        );
        assert!(
            adapter.pids[4].pid == 0 && adapter.pids[4].state == PidState::Inactive,
            "Expected pid with flags = 0"
        );
    }

    // Ported from tests/test_adapter.cpp:169
    #[test]
    fn test_find_pid_unsorted() {
        let mut scheduler = scheduler();
        {
            let adapter = scheduler.adapter_mut(0).unwrap();
            adapter.pids[0].state = PidState::Inactive;
            adapter.pids[1].state = PidState::New;
            adapter.pids[1].pid = 18;
            adapter.pids[2].state = PidState::Active;
            adapter.pids[2].pid = 1151;
        }

        let found = scheduler.find_pid(0, 18);
        assert!(
            found.is_some_and(|entry| entry.pid == 18),
            "find_pid must find a NEW entry behind an INACTIVE slot"
        );
        let found = scheduler.find_pid(0, 1151);
        assert!(
            found.is_some_and(|entry| entry.pid == 1151),
            "find_pid must find an ACTIVE entry behind an INACTIVE slot"
        );
        assert!(
            scheduler.find_pid(0, 9999).is_none(),
            "find_pid must return NULL for a missing pid"
        );
    }

    // Ported from tests/test_adapter.cpp:213
    #[test]
    fn test_update_pids_del_filters_error() {
        let mut first = scheduler();
        first
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(FailingTuner {
                set_pid_fails: false,
                del_filters_fails: true,
            }));
        {
            let adapter = first.adapter_mut(0).unwrap();
            adapter.active_pids = 5;
            adapter.pids[0].state = PidState::Deleted;
            adapter.pids[0].pid = 0;
            adapter.pids[0].fd = 42;
        }
        assert_ne!(
            first.update_pids(0),
            0,
            "update_pids should fail when a pid cannot be removed"
        );
        {
            let adapter = first.adapter(0).unwrap();
            assert!(
                adapter.pids[0].state == PidState::Inactive && adapter.pids[0].fd == 0,
                "removed pid entry must be cleaned up even on failure"
            );
        }

        let mut second = scheduler();
        second
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(FailingTuner {
                set_pid_fails: false,
                del_filters_fails: true,
            }));
        {
            let adapter = second.adapter_mut(0).unwrap();
            adapter.active_pids = 20;
            adapter.pids[0].state = PidState::Deleted;
            adapter.pids[0].pid = 0;
            adapter.pids[0].fd = 42;
        }
        assert_ne!(
            second.update_pids(0),
            0,
            "update_pids should fail on removal failure regardless of pid count"
        );
    }

    // Ported from tests/test_adapter.cpp:254
    #[test]
    fn test_update_pids_max_pids_floor() {
        let mut first = scheduler();
        first
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(FailingTuner {
                set_pid_fails: true,
                del_filters_fails: false,
            }));
        {
            let adapter = first.adapter_mut(0).unwrap();
            adapter.active_pids = 5;
            adapter.max_active_pids = 5;
            adapter.pids[0].state = PidState::New;
            adapter.pids[0].pid = 1151;
        }
        assert_ne!(
            first.update_pids(0),
            0,
            "update_pids should fail when a pid cannot be added below the minimum pid count"
        );
        {
            let adapter = first.adapter(0).unwrap();
            assert_eq!(
                adapter.max_pids, MIN_ADAPTER_PIDS,
                "max_pids must not go below MIN_ADAPTER_PIDS"
            );
            assert_eq!(
                adapter.pids[0].state,
                PidState::New,
                "failing pid must stay NEW so a later update retries it"
            );
        }

        assert_ne!(
            first.update_pids(0),
            0,
            "repeated failure should still report error"
        );
        assert_eq!(
            first.adapter(0).unwrap().max_pids,
            MIN_ADAPTER_PIDS,
            "max_pids must stay at MIN_ADAPTER_PIDS on repeated failures"
        );

        let mut second = scheduler();
        second
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(FailingTuner {
                set_pid_fails: true,
                del_filters_fails: false,
            }));
        {
            let adapter = second.adapter_mut(0).unwrap();
            adapter.active_pids = 20;
            adapter.max_active_pids = 20;
            adapter.pids[0].state = PidState::New;
            adapter.pids[0].pid = 1151;
        }
        assert_eq!(
            second.update_pids(0),
            0,
            "update_pids should succeed when the cap is reached at/above the minimum pid count"
        );
        assert_eq!(
            second.adapter(0).unwrap().max_pids,
            19,
            "max_pids should lower to max_active_pids - 1"
        );
    }

    // Ported from tests/test_adapter.cpp:308
    #[test]
    fn test_dvb_demux_shared_pid_count() {
        let mut scheduler = scheduler();
        let adapter = scheduler.adapter_mut(0).unwrap();
        adapter.dvr = 12;
        adapter.pids[0].state = PidState::Active;
        adapter.pids[0].pid = 0;
        adapter.pids[0].fd = 18; // PSI pid on its own fd
        adapter.pids[1].state = PidState::Active;
        adapter.pids[1].pid = 1151;
        adapter.pids[1].fd = 12; // media pid on the shared demux fd
        adapter.pids[2].state = PidState::New;
        adapter.pids[2].pid = 2221;
        adapter.pids[2].fd = 0; // not active yet: must not be counted
        adapter.pids[3].state = PidState::Deleted;
        adapter.pids[3].pid = 20;
        adapter.pids[3].fd = 12; // deleted: must not be counted
        assert_eq!(
            adapter.demux_shared_pid_count(),
            1,
            "only active pids on the shared demux fd must be counted"
        );
    }

    // Ported from tests/test_adapter.cpp:329
    #[test]
    fn test_compare_slave_parameters() {
        let mut scheduler = Scheduler::new();
        let empty = TuneParams::default();

        // 1. ad is NULL
        assert!(
            !scheduler.compare_slave_parameters(0, &empty),
            "compare_slave_parameters with NULL adapter should return 0"
        );

        add_sat(&mut scheduler, 0);
        let mut slave_config = AdapterConfig::new(DeliverySystem::DvbS2);
        slave_config.master_source = Some(0);
        add_ok(&mut scheduler, 1, slave_config);

        // 2. master is not a slave and is not marked as used by any slave adapter
        assert!(
            !scheduler.compare_slave_parameters(0, &empty),
            "Adapter with empty used array and master_source < 0 should return 0"
        );

        // 3. JESS/UNICABLE switch types should always return 0
        scheduler.adapter_mut(1).unwrap().config.switch_type = SwitchType::Jess;
        let mut tp = TuneParams {
            pol: Polarization::Horizontal,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "UNICABLE/JESS adapter should return 0"
        );
        scheduler.adapter_mut(1).unwrap().config.switch_type = SwitchType::Unicable;
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "UNICABLE/JESS adapter should return 0"
        );

        // Reset switch type
        scheduler.adapter_mut(1).unwrap().config.switch_type = SwitchType::Slave;

        // Configure LNB parameters (Universal LNB) and master old values.
        {
            let master = scheduler.adapter_mut(0).unwrap();
            master.config.lnb_switch = 11_700_000;
            master.old_pol = 0;
            master.old_hiband = 0;
            master.old_diseqc = 0;
        }

        // 4. master check with optional parameters
        tp = TuneParams::default();
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "No parameters set in transponder should not conflict"
        );

        tp = TuneParams {
            pol: Polarization::Vertical,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Matching polarization should return 0"
        );

        tp = TuneParams {
            pol: Polarization::None,
            pol_specified: true,
            ..TuneParams::default()
        };
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Polarization=0 (unspecified) maps to pol=1 and should conflict with old_pol=0"
        );

        scheduler.adapter_mut(0).unwrap().old_pol = 1;
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Polarization=0 (unspecified) maps to pol=1 and should match old_pol=1"
        );
        scheduler.adapter_mut(0).unwrap().old_pol = 0;

        tp = TuneParams {
            pol: Polarization::Horizontal,
            ..TuneParams::default()
        };
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Conflicting polarization should return 1"
        );

        tp = TuneParams {
            src: 1,
            src_specified: true,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Matching diseqc should return 0"
        );

        tp = TuneParams {
            src: 0,
            src_specified: true,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Diseqc=0 (unspecified) maps to diseqc=0 and should match old_diseqc=0"
        );

        scheduler.adapter_mut(0).unwrap().old_diseqc = 1;
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Diseqc=0 (unspecified) maps to diseqc=0 and should conflict with old_diseqc=1"
        );
        scheduler.adapter_mut(0).unwrap().old_diseqc = 0;

        tp = TuneParams {
            src: 2,
            src_specified: true,
            ..TuneParams::default()
        };
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Conflicting diseqc should return 1"
        );

        tp = TuneParams {
            freq: 10_778_000,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Matching band (lowband) should return 0"
        );

        tp = TuneParams::default();
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Frequency=0 (unspecified) should return 0"
        );

        tp = TuneParams {
            freq: 12_322_000,
            ..TuneParams::default()
        };
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Conflicting band (hiband) should return 1"
        );

        // 4b. Empty tp.diseqc_param: the adapter's settings apply.
        scheduler.adapter_mut(0).unwrap().old_hiband = 1;
        assert!(
            !scheduler.compare_slave_parameters(1, &tp),
            "Highband tp with empty diseqc_param should match master old_hiband=1"
        );
        scheduler.adapter_mut(0).unwrap().old_hiband = 0;
        assert!(
            scheduler.compare_slave_parameters(1, &tp),
            "Highband tp with empty diseqc_param should conflict with master old_hiband=0"
        );

        // 5. master used by slave_ad
        {
            let master = scheduler.adapter_mut(0).unwrap();
            master.used[1] = true;
        }
        {
            let slave = scheduler.adapter_mut(1).unwrap();
            slave.old_pol = 0;
            slave.old_hiband = 0;
            slave.old_diseqc = 0;
        }

        tp = TuneParams {
            pol: Polarization::Horizontal,
            ..TuneParams::default()
        };
        assert!(
            scheduler.compare_slave_parameters(0, &tp),
            "Conflicting polarization on slave adapter should return 1"
        );

        tp = TuneParams {
            pol: Polarization::Vertical,
            ..TuneParams::default()
        };
        assert!(
            !scheduler.compare_slave_parameters(0, &tp),
            "Matching polarization on slave adapter should return 0"
        );
    }

    // DDCI's multiple-PMT filter mirrors this view of the pid table.
    #[test]
    fn active_pids_lists_owned_and_released_pids() {
        let mut scheduler = scheduler();
        assert_eq!(scheduler.mark_pid_add(0, 3, 100), 0);
        assert_eq!(scheduler.mark_pid_add(0, 3, 8192), 0);
        let owned = |s: &Scheduler, pid| {
            s.active_pids(0)
                .find(|p| p.pid == pid)
                .map(|p| p.sids.contains(&3))
        };
        assert_eq!(owned(&scheduler, 100), Some(true));
        assert_eq!(owned(&scheduler, 8192), Some(true));
        scheduler.unmark_pid(0, 3, 100);
        assert_ne!(
            owned(&scheduler, 100),
            Some(true),
            "released pid has no owner"
        );
        assert_eq!(scheduler.active_pids(99).count(), 0, "unknown adapter");
    }

    // Ported from tests/test_adapter.cpp:515
    #[test]
    fn test_mark_pid_add_bounds() {
        let mut scheduler = scheduler();
        assert_eq!(
            scheduler.mark_pid_add(0, 0, 8193),
            -1,
            "pid 8193 must be rejected"
        );
        assert_eq!(
            scheduler.mark_pid_add(0, 0, 99999),
            -1,
            "pid 99999 must be rejected"
        );
        assert_eq!(
            scheduler.mark_pid_add(0, 0, -5),
            -1,
            "negative pid must be rejected"
        );
        assert!(
            scheduler.find_pid(0, 8193).is_none(),
            "rejected pid must leave no entry"
        );
        assert_eq!(
            scheduler.mark_pid_add(0, 0, 8192),
            0,
            "pids=all marker must be added"
        );
        assert!(
            scheduler.find_pid(0, 8192).is_some(),
            "pids=all marker must be found"
        );
    }

    // Ported from tests/test_adapter.cpp:534
    #[test]
    fn test_next_pid_order_concurrent() {
        let scheduler = scheduler();
        let pid_order = scheduler.adapter(0).unwrap().pid_order();
        const NTHREADS: usize = 8;
        const PER: usize = 2000;

        let got: Vec<Vec<u32>> = std::thread::scope(|scope| {
            let mut threads = Vec::new();
            for _ in 0..NTHREADS {
                let pid_order = pid_order.clone();
                threads.push(
                    scope.spawn(move || (0..PER).map(|_| pid_order.next_pid_order()).collect()),
                );
            }
            threads
                .into_iter()
                .map(|thread| thread.join().unwrap())
                .collect()
        });

        let mut seen = std::collections::HashSet::new();
        for value in got.into_iter().flatten() {
            assert_ne!(value, 0, "order must never be 0");
            assert!(seen.insert(value), "duplicate pid order across threads");
        }
        assert_eq!(seen.len(), NTHREADS * PER, "lost sequence updates");
    }

    // C++: get_free_adapter checks an already tuned adapter first
    // (adapter.cpp:754-759), before looking at idle ones (#1509).
    #[test]
    fn same_mux_reuses_busy_adapter_before_free_adapter() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        add_sat(&mut scheduler, 1);

        let tp = sat_params(11_362_000, Polarization::Horizontal);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        assert!(scheduler.set_adapter_for_stream(10, 0));
        assert!(scheduler.set_adapter_parameters(0, 10, &tp).is_ok());

        assert_eq!(
            scheduler.get_free_adapter(&tp),
            Some(0),
            "the tuned adapter is chosen even though adapter 1 is idle"
        );
        let other = sat_params(11_363_001, Polarization::Horizontal);
        assert_eq!(
            scheduler.get_free_adapter(&other),
            Some(1),
            "1001 kHz away is a different mux"
        );
    }

    // C++: adapter.cpp:761-774 opens an allocated but uninitialized slot.
    #[test]
    fn uninitialized_adapter_is_initialized_by_allocation() {
        let mut scheduler = Scheduler::new();
        let mut config = AdapterConfig::new(DeliverySystem::DvbS2);
        config.enabled = false;
        add_ok(&mut scheduler, 0, config);
        assert!(!scheduler.adapter(0).unwrap().config.enabled);

        let tp = sat_params(11_362_000, Polarization::Horizontal);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        assert!(scheduler.adapter(0).unwrap().config.enabled);
    }

    // C++: `-e` marks adapters disabled; init_hw refuses them
    // (adapter.cpp:269-272) and get_free_adapter skips them.
    #[test]
    fn disabled_adapter_is_skipped_and_blames_the_frontend() {
        let mut scheduler = Scheduler::new();
        let mut config = AdapterConfig::new(DeliverySystem::DvbS2);
        config.enabled = false;
        add_ok(&mut scheduler, 0, config);
        scheduler.set_disabled(0, true);

        let tp = sat_params(11_362_000, Polarization::Horizontal);
        assert_eq!(scheduler.get_free_adapter(&tp), None);
        assert_eq!(scheduler.out_of_range_attrs(&tp), Some("msys"));

        scheduler.set_fe_map_entry(0, 0);
        let mut pinned = tp.clone();
        pinned.fe = Some(1);
        assert_eq!(scheduler.get_free_adapter(&pinned), None);
        assert_eq!(scheduler.out_of_range_attrs(&pinned), Some("fe"));
    }

    // C++: a mapped `fe=` pins get_free_adapter to that adapter and `goto
    // noadapter` forbids falling back to an idle one (adapter.cpp:720-752).
    #[test]
    fn pinned_frontend_overrides_same_mux_and_free_adapters() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        add_sat(&mut scheduler, 1);
        scheduler.set_fe_map_entry(0, 1);

        let mut tp = sat_params(11_362_000, Polarization::Horizontal);
        tp.fe = Some(1);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(1));
        assert!(scheduler.set_adapter_for_stream(10, 1));
        assert!(scheduler.set_adapter_parameters(1, 10, &tp).is_ok());
        assert_eq!(scheduler.tune(1, 10).result, Ok(()));

        assert_eq!(scheduler.get_free_adapter(&tp), Some(1));
        let mut other = sat_params(12_543_000, Polarization::Vertical);
        other.fe = Some(1);
        assert_eq!(
            scheduler.get_free_adapter(&other),
            None,
            "a busy pinned frontend must not fall back to the free adapter"
        );
    }

    // C++: absolute sources map `diseqc` per adapter when `-A` is enabled
    // (adapter.cpp:681-705) and are applied when the client omits FE=
    // (adapter.cpp:1411-1415).
    #[test]
    fn absolute_source_table_filters_and_maps() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        scheduler.set_absolute_switch(true);
        let mut table = [0u8; MAX_SOURCES];
        table[0] = 3;
        scheduler.set_absolute_table(0, table);

        let tp = sat_params(11_362_000, Polarization::Horizontal);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        assert!(scheduler.set_adapter_for_stream(10, 0));
        assert!(scheduler.set_adapter_parameters(0, 10, &tp).is_ok());
        let stored = scheduler.adapter(0).unwrap();
        assert_eq!(stored.params.src, 3);
        assert!(stored.params.src_specified);
        assert!(
            !scheduler.compare_tuning_parameters(0, &tp),
            "src 1 maps to the stored src 3"
        );

        let unmapped = sat_params(12_000_000, Polarization::Vertical);
        let unmapped = TuneParams { src: 2, ..unmapped };
        assert_eq!(scheduler.get_free_adapter(&unmapped), None);
        assert_eq!(scheduler.out_of_range_attrs(&unmapped), Some("src"));
    }

    // C++: absolute sources only apply to DVB-S/S2 (adapter.cpp:684-686).
    #[test]
    fn absolute_source_table_ignores_terrestrial() {
        let mut scheduler = Scheduler::new();
        add_ok(&mut scheduler, 0, AdapterConfig::new(DeliverySystem::DvbT2));
        scheduler.set_absolute_switch(true);
        let tp = TuneParams {
            freq: 474_000_000,
            msys: DeliverySystem::DvbT2,
            src: 9,
            src_specified: true,
            ..TuneParams::default()
        };
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
    }

    // C++: compare_tunning_parameters (adapter.cpp:1322-1348).
    #[test]
    fn tuning_comparison_covers_every_cpp_field() {
        let mut scheduler = scheduler();
        let mut tp = params(11_362_000, 1);
        tp.plp = Some(0);
        tp.mtype = 2;
        tp.mtype_specified = true;
        assert!(scheduler.set_adapter_for_stream(10, 0));
        assert!(scheduler.set_adapter_parameters(0, 10, &tp).is_ok());
        assert!(!scheduler.compare_tuning_parameters(0, &tp));

        let mut changed = tp.clone();
        changed.plp = Some(1);
        assert!(scheduler.compare_tuning_parameters(0, &changed));

        let mut changed = tp.clone();
        changed.src_specified = false;
        changed.src = 0;
        assert!(
            scheduler.compare_tuning_parameters(0, &changed),
            "a missing source stores the C++ nullopt, which differs from 1"
        );

        let mut changed = tp.clone();
        changed.pol = Polarization::None;
        assert!(
            !scheduler.compare_tuning_parameters(0, &changed),
            "polarisation is only compared when the new value is > 0"
        );

        let mut changed = tp.clone();
        changed.pol = Polarization::Vertical;
        assert!(scheduler.compare_tuning_parameters(0, &changed));

        let mut changed = tp.clone();
        changed.sr = 1_000;
        assert!(
            !scheduler.compare_tuning_parameters(0, &changed),
            "symbol rates <= 1000 are ignored"
        );

        let mut changed = tp.clone();
        changed.sr = 22_000_000;
        assert!(scheduler.compare_tuning_parameters(0, &changed));

        let mut changed = tp.clone();
        changed.mtype_specified = false;
        assert!(
            !scheduler.compare_tuning_parameters(0, &changed),
            "an unspecified modulation is QAM_AUTO"
        );

        let mut changed = tp.clone();
        changed.mtype = 6;
        assert!(
            !scheduler.compare_tuning_parameters(0, &changed),
            "QAM_AUTO(6) is never compared"
        );

        let mut changed = tp.clone();
        changed.mtype = 3;
        assert!(scheduler.compare_tuning_parameters(0, &changed));
    }

    // C++: compare_slave_parameters returns 0 for an adapter that is neither a
    // slave nor has slaves attached (adapter.cpp:621-624).
    #[test]
    fn standalone_adapter_ignores_slave_lnb_state() {
        let mut scheduler = scheduler();
        scheduler.adapter_mut(0).unwrap().old_pol = 1;
        assert!(
            !scheduler.compare_slave_parameters(0, &sat_params(11_000_000, Polarization::Vertical))
        );
    }

    // C++: a slave must match its master's polarisation, band and diseqc
    // (adapter.cpp:637-672), with hiband from get_lnb_hiband.
    #[test]
    fn master_lnb_state_rejects_incompatible_slaves() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        let mut slave = AdapterConfig::new(DeliverySystem::DvbS2);
        slave.master_source = Some(0);
        add_ok(&mut scheduler, 1, slave);
        {
            let master = scheduler.adapter_mut(0).unwrap();
            master.old_pol = 1;
            master.old_hiband = 0;
            master.old_diseqc = 0;
        }

        let h_low = sat_params(11_000_000, Polarization::Horizontal);
        let v_low = sat_params(11_000_000, Polarization::Vertical);
        let h_high = sat_params(12_000_000, Polarization::Horizontal);
        let mut h_other_src = h_low.clone();
        h_other_src.src = 2;

        assert!(!scheduler.compare_slave_parameters(1, &h_low));
        assert!(scheduler.compare_slave_parameters(1, &v_low));
        assert!(scheduler.compare_slave_parameters(1, &h_high));
        assert!(scheduler.compare_slave_parameters(1, &h_other_src));

        let left_high = sat_params(12_000_000, Polarization::Left);
        assert!(
            !scheduler.compare_slave_parameters(1, &left_high),
            "a circular LNB always uses the low band (adapter.cpp:1294-1300)"
        );
    }

    // C++: a master also checks the adapters that use it (adapter.cpp:649-666).
    #[test]
    fn master_checks_used_slaves_lnb_state() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        let mut slave = AdapterConfig::new(DeliverySystem::DvbS2);
        slave.master_source = Some(0);
        add_ok(&mut scheduler, 1, slave);
        assert!(scheduler.set_adapter_for_stream(5, 1));
        assert!(scheduler.adapter(0).unwrap().used[1]);
        {
            let used = scheduler.adapter_mut(1).unwrap();
            used.old_pol = 1;
            used.old_hiband = 0;
            used.old_diseqc = 0;
        }

        assert!(
            scheduler.compare_slave_parameters(0, &sat_params(11_000_000, Polarization::Vertical))
        );
        assert!(
            !scheduler
                .compare_slave_parameters(0, &sat_params(11_000_000, Polarization::Horizontal))
        );
    }

    // C++: unicable/JESS slaves ignore polarisation and band
    // (adapter.cpp:626-630), so they are allocated as regular tuners.
    #[test]
    fn unicable_and_jess_slaves_skip_lnb_conflicts() {
        for switch in [SwitchType::Unicable, SwitchType::Jess] {
            let mut scheduler = Scheduler::new();
            add_sat(&mut scheduler, 0);
            let mut slave = AdapterConfig::new(DeliverySystem::DvbS2);
            slave.master_source = Some(0);
            slave.switch_type = switch;
            add_ok(&mut scheduler, 1, slave);

            let busy = sat_params(11_362_000, Polarization::Horizontal);
            assert!(scheduler.set_adapter_for_stream(10, 0));
            assert!(scheduler.set_adapter_parameters(0, 10, &busy).is_ok());
            assert_eq!(scheduler.tune(0, 10).result, Ok(()));

            let conflicting = sat_params(12_000_000, Polarization::Vertical);
            assert!(!scheduler.compare_slave_parameters(1, &conflicting));
            assert_eq!(scheduler.get_free_adapter(&conflicting), Some(1));
        }
    }

    // C++: when no regular tuner is free, a conflicting slave is still
    // returned by the final loop (adapter.cpp:776-785).
    #[test]
    fn conflicting_slave_is_the_last_resort() {
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        let mut slave = AdapterConfig::new(DeliverySystem::DvbS2);
        slave.master_source = Some(0);
        add_ok(&mut scheduler, 1, slave);

        let busy = sat_params(11_362_000, Polarization::Horizontal);
        assert!(scheduler.set_adapter_for_stream(10, 0));
        assert!(scheduler.set_adapter_parameters(0, 10, &busy).is_ok());
        assert_eq!(scheduler.tune(0, 10).result, Ok(()));

        let conflicting = sat_params(11_000_000, Polarization::Vertical);
        assert!(scheduler.compare_slave_parameters(1, &conflicting));
        assert_eq!(scheduler.get_free_adapter(&conflicting), Some(1));
    }

    // C++: a slave stream cannot retune a busy adapter (-2,
    // adapter.cpp:1389-1401); the same mux is still accepted.
    #[test]
    fn slave_stream_cannot_retune_the_master() {
        let mut scheduler = scheduler();
        let tp = params(11_362_000, 1);
        assert!(scheduler.set_adapter_for_stream(10, 0));
        assert!(scheduler.set_adapter_parameters(0, 10, &tp).is_ok());
        assert!(scheduler.set_adapter_for_stream(11, 0));

        let other = params(12_543_000, 1);
        let error = scheduler
            .set_adapter_parameters(0, 11, &other)
            .expect_err("a slave must not retune");
        assert_eq!(error, SetParamsError::SlaveConflict);
        assert_eq!(error.code(), -2);
        assert!(scheduler.set_adapter_parameters(0, 11, &tp).is_ok());
    }

    // C++: tune calls the backend, closes the other streams and then runs
    // post_tune's default PIDs (adapter.cpp:1086-1114, 526-554).
    #[test]
    fn tune_forwards_params_closes_slaves_and_syncs_default_pids() {
        let (mut scheduler, tunes, pid_sets) = recording_scheduler();
        let mut tp = sat_params(11_362_000, Polarization::Horizontal);
        tp.pids = PidSet::from_pids([100u16, 200]);
        assert!(scheduler.set_adapter_for_stream(1, 0));
        assert!(scheduler.set_adapter_parameters(0, 1, &tp).is_ok());
        assert!(scheduler.set_adapter_for_stream(2, 0));

        let outcome = scheduler.tune(0, 1);
        assert_eq!(outcome.result, Ok(()));
        assert_eq!(outcome.close_streams, vec![2]);
        assert_eq!(tunes.borrow().len(), 1);
        assert_eq!(tunes.borrow()[0], tp);
        assert_eq!(scheduler.adapter(0).unwrap().sid_cnt, 1);

        let expected = BTreeSet::from([0, 1, 16, 18, 100, 200]);
        assert_eq!(active_pids(&scheduler), expected);
        assert_eq!(
            pid_sets.borrow().last().cloned().unwrap_or_default(),
            expected,
            "the backend receives the table, defaults included"
        );

        // A second tune on the same master has nothing pending.
        assert_eq!(scheduler.tune(0, 1).result, Ok(()));
        assert_eq!(tunes.borrow().len(), 1);
    }

    // C++: post_tune does not add defaults when PID_ALL is already active
    // (adapter.cpp:1010-1018 and post_tune's early return).
    #[test]
    fn pid_all_suppresses_default_pids() {
        let (mut scheduler, _tunes, pid_sets) = recording_scheduler();
        let mut tp = sat_params(11_362_000, Polarization::Horizontal);
        tp.pids = PidSet::all();
        assert!(scheduler.set_adapter_for_stream(1, 0));
        assert!(scheduler.set_adapter_parameters(0, 1, &tp).is_ok());

        assert_eq!(scheduler.tune(0, 1).result, Ok(()));
        assert_eq!(active_pids(&scheduler), BTreeSet::from([PID_ALL]));
        assert_eq!(
            pid_sets.borrow().last().cloned().unwrap_or_default(),
            BTreeSet::from([PID_ALL])
        );
    }

    // C++: a failed tune still deletes the adapter's PIDs and returns the
    // hardware error (adapter.cpp:1118-1127).
    #[test]
    fn failed_tune_returns_the_hardware_error_and_clears_pids() {
        let tunes = Rc::new(RefCell::new(Vec::new()));
        let pid_sets = Rc::new(RefCell::new(Vec::new()));
        let mut scheduler = Scheduler::new();
        add_sat(&mut scheduler, 0);
        scheduler
            .adapter_mut(0)
            .unwrap()
            .set_tuner(Box::new(RecordingTuner {
                tunes,
                pid_sets,
                result: Some(TuneError::OutOfRange),
            }));

        let tp = params(11_362_000, 1);
        assert!(scheduler.set_adapter_for_stream(1, 0));
        assert!(scheduler.set_adapter_parameters(0, 1, &tp).is_ok());
        let outcome = scheduler.tune(0, 1);
        assert_eq!(outcome.result, Err(TuneError::OutOfRange));
        assert_eq!(outcome.result.unwrap_err().code(), -403);
        assert!(active_pids(&scheduler).is_empty());
    }

    // C++: tune returns -500 when get_adapter() is NULL (adapter.cpp:1081-1082).
    #[test]
    fn tune_without_an_enabled_adapter_returns_500() {
        let mut scheduler = Scheduler::new();
        let error = scheduler.tune(3, 0).result.expect_err("no adapter");
        assert_eq!(error, TuneError::Hardware);
        assert_eq!(error.code(), -500);

        let mut config = AdapterConfig::new(DeliverySystem::DvbS2);
        config.enabled = false;
        add_ok(&mut scheduler, 0, config);
        scheduler.set_disabled(0, true);
        let error = scheduler.tune(0, 0).result.expect_err("disabled adapter");
        assert_eq!(error, TuneError::Hardware);
    }

    // C++: mark_pid_add accepts 0..=8192 and shares one entry between streams
    // (adapter.cpp:1227-1259); the last close drops it.
    #[test]
    fn pid_add_range_and_shared_entries() {
        let mut scheduler = scheduler();
        assert_eq!(scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, -1), -1);
        assert_eq!(
            scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, PID_ALL + 1),
            -1
        );
        assert_eq!(
            scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, PID_ALL),
            0
        );

        let mut tp = params(11_362_000, 1);
        tp.pids = PidSet::from_pids([100u16]);
        assert!(scheduler.set_adapter_for_stream(1, 0));
        assert!(scheduler.set_adapter_parameters(0, 1, &tp).is_ok());
        assert_eq!(scheduler.tune(0, 1).result, Ok(()));
        assert_eq!(scheduler.stream_pids(0, 1), vec![100]);

        assert!(scheduler.set_adapter_for_stream(2, 0));
        assert!(scheduler.set_adapter_parameters(0, 2, &tp).is_ok());
        assert_eq!(scheduler.stream_pids(0, 2), vec![100]);

        scheduler.close_adapter_for_stream(2, 0);
        assert_eq!(
            scheduler.stream_pids(0, 1),
            vec![100],
            "the PID is still owned by stream 1"
        );
        assert!(scheduler.adapter(0).unwrap().active_pids().contains(&100));

        scheduler.close_adapter_for_stream(1, 0);
        assert!(scheduler.adapter(0).unwrap().active_pids().is_empty());
    }
}
