//! Hardware-free adapter allocation, same-mux reuse and PID bookkeeping.
//!
//! Mirrors the allocation half of the C++ `adapter.cpp`; hardware access is
//! behind [`AdapterTuner`].

use std::collections::BTreeSet;

use ms_tuner::{DeliverySystem, Polarization, TuneParams};

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
    /// LNB band-switch frequency in kHz.
    pub lnb_switch: u32,
    /// Circular LNB local-oscillator frequency in kHz.
    pub lnb_circular: u32,
}

impl Default for AdapterConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            sys: [DeliverySystem::Undefined; MAX_DELSYS],
            absolute_table: [0; MAX_SOURCES],
            master_source: None,
            lnb_switch: 11_700,
            lnb_circular: 10_750,
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
    max_pids: i32,
}

impl Adapter {
    fn new(config: AdapterConfig, tuner: Box<dyn AdapterTuner>) -> Self {
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
            max_pids: 0,
        }
    }

    /// Replaces the tuner backend, mainly for tests.
    pub fn set_tuner(&mut self, tuner: Box<dyn AdapterTuner>) {
        self.tuner = tuner;
    }

    /// PIDs currently active or about to be activated on this adapter.
    pub fn active_pids(&self) -> Vec<i32> {
        self.pids
            .iter()
            .filter(|entry| matches!(entry.state, PidState::Active | PidState::New))
            .map(|entry| entry.pid)
            .collect()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum PidState {
    #[default]
    Inactive,
    Active,
    New,
    Deleted,
}

#[derive(Debug, Clone, Default)]
struct PidEntry {
    pid: i32,
    state: PidState,
    sids: BTreeSet<StreamId>,
}

/// Adapter table and allocation policy.
pub struct Scheduler {
    adapters: Vec<Option<Adapter>>,
    fe_map: [i16; FE_MAP_LEN],
    disabled: [bool; MAX_ADAPTERS],
    absolute_switch: bool,
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
        }
    }

    /// Installs an adapter at `aid`.
    pub fn add_adapter(&mut self, aid: usize, config: AdapterConfig, tuner: Box<dyn AdapterTuner>) {
        if aid < MAX_ADAPTERS {
            self.adapters[aid] = Some(Adapter::new(config, tuner));
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
        for index in 0..MAX_ADAPTERS {
            let Some(adapter) = self.adapters[index].as_ref() else {
                continue;
            };
            if adapter.config.enabled
                && adapter.sid_cnt == 0
                && delsys_match(adapter, tp.msys)
                && self.source_enabled(index, tp)
                && !self.compare_slave_parameters(index as i32, tp)
            {
                return Some(index);
            }
        }
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
                return Some(index);
            }
        }
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
        if (i64::from(tp.freq) - i64::from(adapter.params.freq)).abs() > 1 {
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
        if tp.sr > 1 && adapter.params.sr > 1 && tp.sr != adapter.params.sr {
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
    pub fn compare_slave_parameters(&self, aid: i32, tp: &TuneParams) -> bool {
        let Some(adapter) = self.enabled_adapter(aid) else {
            return false;
        };
        if adapter.config.master_source.is_none() && !adapter.used.iter().any(|used| *used) {
            return false;
        }
        let pol = (polarization_code(tp.pol) - 1) & 1;
        let diseqc = if tp.src > 0 { i32::from(tp.src) - 1 } else { 0 };
        let hiband = (tp.freq > 0).then(|| lnb_hiband(adapter, tp));
        for (index, used) in adapter.used.iter().enumerate() {
            if !used {
                continue;
            }
            if let Some(other) = self.adapters[index].as_ref()
                && incompatible(other, pol, hiband, diseqc)
            {
                return true;
            }
        }
        if let Some(master) = adapter.config.master_source
            && let Some(other) = self.adapters.get(master).and_then(Option::as_ref)
            && incompatible(other, pol, hiband, diseqc)
        {
            return true;
        }
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
            i32::from(adapter.config.absolute_table[src as usize - 1])
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
        let master_source = adapter.config.master_source;
        if let Some(master) = master_source
            && let Some(master) = self.adapters.get_mut(master).and_then(Option::as_mut)
        {
            master.used[index] = true;
        }
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
        if count == 0 {
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.master_sid = -1;
                adapter.params = TuneParams::default();
            }
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
            let master_source = self.adapters[index]
                .as_ref()
                .and_then(|adapter| adapter.config.master_source);
            if let Some(master) = master_source
                && let Some(master) = self.adapters.get_mut(master).and_then(Option::as_mut)
            {
                master.used[index] = false;
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
        if adapter.master_sid == -1 {
            adapter.master_sid = sid;
        }
        adapter.do_tune = false;
        if self.compare_tuning_parameters(aid, tp) {
            if self.enabled_adapter(aid).map(|a| a.master_sid) != Some(sid) {
                return Err(SetParamsError::SlaveConflict);
            }
            if let Some(adapter) = self.adapters[index].as_mut() {
                adapter.do_tune = true;
            }
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
            if self.update_pids(index) {
                return Err(SetParamsError::PidFailure);
            }
        }
        let Some(adapter) = self.adapters[index].as_mut() else {
            return Err(SetParamsError::MissingAdapter);
        };
        adapter.params = tp.clone();
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
            if let Some(adapter) = self.adapters[index].as_mut() {
                result = adapter.tuner.tune(&params);
            }
            // C++ closes the slaves before post_tune, so their PIDs are gone first.
            for slave in &slaves {
                self.close_adapter_for_stream(*slave, aid);
            }
            close_streams = slaves;
            self.post_tune(index);
        }
        if result.is_err() {
            self.mark_pid_deleted(index, PID_STREAM_ID_UNDEFINED, None);
        }
        if self.update_pids(index) && result.is_ok() {
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

    /// Adds `pid` to `aid`'s table for `sid`; `false` when the table is full.
    pub fn mark_pid_add(&mut self, aid: i32, sid: StreamId, pid: i32) -> bool {
        if !(0..=PID_ALL).contains(&pid) {
            return false;
        }
        let Some(adapter) = self.enabled_adapter_mut(aid) else {
            return false;
        };
        if let Some(entry) = adapter
            .pids
            .iter_mut()
            .find(|entry| entry.state != PidState::Inactive && entry.pid == pid)
        {
            if sid != PID_STREAM_ID_UNDEFINED {
                entry.sids.insert(sid);
            }
            if entry.state == PidState::Deleted {
                entry.state = PidState::New;
            }
            return true;
        }
        let Some(entry) = adapter
            .pids
            .iter_mut()
            .find(|entry| entry.state == PidState::Inactive)
        else {
            return false;
        };
        entry.pid = pid;
        entry.state = PidState::New;
        entry.sids.clear();
        if sid != PID_STREAM_ID_UNDEFINED {
            entry.sids.insert(sid);
        }
        true
    }

    fn mark_pid_deleted(&mut self, index: usize, sid: StreamId, pid: Option<i32>) {
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
            if sid == PID_STREAM_ID_UNDEFINED {
                entry.state = PidState::Deleted;
                entry.sids.clear();
            } else {
                entry.sids.remove(&sid);
                if entry.sids.is_empty() {
                    entry.state = PidState::Deleted;
                }
            }
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
            if !present && !self.mark_pid_add(aid, sid, pid) {
                return false;
            }
        }
        true
    }

    fn post_tune(&mut self, index: usize) {
        let Some(adapter) = self.adapters[index].as_ref() else {
            return;
        };
        let all_active =
            |state: PidState| state != PidState::Inactive && state != PidState::Deleted;
        if adapter
            .pids
            .iter()
            .any(|entry| all_active(entry.state) && entry.pid == PID_ALL)
        {
            return;
        }
        let existing: Vec<i32> = adapter
            .pids
            .iter()
            .filter(|entry| all_active(entry.state))
            .map(|entry| entry.pid)
            .collect();
        for pid in [0, 1, 16, 18] {
            if !existing.contains(&pid) {
                self.mark_pid_add(index as i32, PID_STREAM_ID_UNDEFINED, pid);
            }
        }
    }

    fn update_pids(&mut self, index: usize) -> bool {
        let Some(adapter) = self.adapters[index].as_mut() else {
            return true;
        };
        for entry in &mut adapter.pids {
            if entry.state == PidState::Deleted {
                *entry = PidEntry::default();
            }
        }
        adapter.active_pids = adapter
            .pids
            .iter()
            .filter(|entry| entry.state == PidState::Active)
            .count() as i32;
        for slot in 0..MAX_PIDS {
            if adapter.pids[slot].state != PidState::New {
                continue;
            }
            if adapter.max_pids > 0 && adapter.max_pids < adapter.active_pids {
                break;
            }
            adapter.active_pids += 1;
            adapter.pids[slot].state = PidState::Active;
        }
        false
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

fn polarization_code(pol: Polarization) -> i32 {
    match pol {
        Polarization::Vertical => 1,
        Polarization::Horizontal => 2,
        Polarization::Right => 3,
        Polarization::Left => 4,
        Polarization::None => 0,
    }
}

fn lnb_hiband(adapter: &Adapter, tp: &TuneParams) -> i32 {
    if polarization_code(tp.pol) > 2 && adapter.config.lnb_circular > 0 {
        return 0;
    }
    i32::from(adapter.config.lnb_switch > 0 && tp.freq > adapter.config.lnb_switch)
}

fn incompatible(adapter: &Adapter, pol: i32, hiband: Option<i32>, diseqc: i32) -> bool {
    pol != adapter.old_pol
        || hiband.is_some_and(|hiband| hiband != adapter.old_hiband)
        || diseqc != adapter.old_diseqc
}

#[cfg(test)]
mod tests {
    use super::*;
    use ms_tuner::PidSet;

    struct OkTuner;

    impl AdapterTuner for OkTuner {
        fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
            Ok(())
        }
    }

    fn params(freq: u32, src: u8) -> TuneParams {
        TuneParams {
            freq,
            src,
            src_specified: true,
            msys: DeliverySystem::DvbS2,
            pol: Polarization::Horizontal,
            sr: 27_500,
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

    #[test]
    fn same_mux_selects_busy_adapter() {
        let mut scheduler = scheduler();
        let tp = params(11_362, 1);
        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());

        assert_eq!(scheduler.get_free_adapter(&tp), Some(0));
        let other = params(12_543, 1);
        assert_eq!(scheduler.get_free_adapter(&other), None);
        assert_eq!(scheduler.out_of_range_attrs(&other), None);
    }

    #[test]
    fn unsupported_delivery_system_reports_msys() {
        let mut scheduler = scheduler();
        let mut tp = params(538, 0);
        tp.msys = DeliverySystem::DvbT2;
        assert_eq!(scheduler.get_free_adapter(&tp), None);
        assert_eq!(scheduler.out_of_range_attrs(&tp), Some("msys"));
    }

    #[test]
    fn unmapped_src_reports_src() {
        let mut scheduler = scheduler();
        scheduler.set_absolute_switch(true);
        let tp = params(11_362, 3);
        assert_eq!(scheduler.get_free_adapter(&tp), None);
        assert_eq!(scheduler.out_of_range_attrs(&tp), Some("src"));
    }

    #[test]
    fn frequency_tolerance_is_one_khz() {
        let mut scheduler = scheduler();
        let tp = params(11_362, 1);
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());

        let mut near = tp.clone();
        near.freq = 11_363;
        assert!(!scheduler.compare_tuning_parameters(0, &near));
        near.freq = 11_364;
        assert!(scheduler.compare_tuning_parameters(0, &near));
    }

    #[test]
    fn pid_table_capacity_and_release() {
        let mut scheduler = scheduler();
        let tp = params(11_362, 1);
        assert!(scheduler.set_adapter_for_stream(0, 0));
        assert!(scheduler.set_adapter_parameters(0, 0, &tp).is_ok());
        assert!(scheduler.tune(0, 0).result.is_ok());

        let mut added = 0;
        let mut pid = 100;
        while pid < 8000 && scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, pid) {
            added += 1;
            pid += 1;
        }
        assert_eq!(added, MAX_PIDS - 4, "four default pids are already active");

        scheduler.close_adapter_for_stream(0, 0);
        assert!(
            scheduler.mark_pid_add(0, PID_STREAM_ID_UNDEFINED, 5000),
            "releasing the adapter must free the PID table"
        );
    }
}
