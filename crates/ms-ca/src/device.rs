//! Per-CAM CAPMT slots and the C++ send rules (`dvbca_process_pmt`, `dvbca_del_pmt`).

use crate::capmt::{
    BuildCtx, CAPMT_KEPT_ES_MAX, CLM_ONLY, CLM_UPDATE, CMD_ID_NOT_SELECTED, CMD_ID_OK_DESCRAMBLING,
    Capmt, MAX_CA_PMT, MAX_PMT_PER_CAPMT, capmt_add_pmt, capmt_content_hash, create_capmt,
    make_sid_for_ca, pmt_id_is_valid,
};
use crate::program::ProgramTable;
use crate::{CaDriver, TablesResult};

/// `CA_STATE_*`: only `Initialized` accepts CAPMTs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CaState {
    #[default]
    Inactive,
    Active,
    Initialized,
}

/// Outcome of `send_capmt`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendOutcome {
    Sent,
    /// Content hash unchanged and not forced: nothing written.
    Unchanged,
    /// The write failed; C++ still reports success (returns 0).
    WriteFailed,
    /// No CA session (C++ returns 1).
    NoSession,
    /// `create_capmt` failed (C++ NORETRY).
    BuildFailed,
}

impl SendOutcome {
    /// The C++ return value treated as failure by callers.
    pub fn is_error(self) -> bool {
        matches!(self, Self::NoSession | Self::BuildFailed)
    }
}

/// CAPMT state of one CAM (`ca_device_t`, CAPMT part).
#[derive(Clone, Debug)]
pub struct CaDevice {
    pub id: i32,
    pub enabled: bool,
    pub state: CaState,
    pub max_ca_pmt: usize,
    /// Number of `*` in `-ca-channels`: up to `multiple_pmt + 1` programs per CAPMT.
    pub multiple_pmt: usize,
    pub capmt: [Capmt; MAX_CA_PMT],
    /// CAIDs the CA handles on this adapter (`add_caid_mask`); empty accepts all.
    pub caid_filter: Vec<(u16, u16)>,
}

impl Default for CaDevice {
    fn default() -> Self {
        Self {
            id: 0,
            enabled: false,
            state: CaState::Inactive,
            max_ca_pmt: 0,
            multiple_pmt: 0,
            capmt: Default::default(),
            caid_filter: Vec::new(),
        }
    }
}

impl CaDevice {
    pub fn new(id: i32, max_ca_pmt: usize, multiple_pmt: usize) -> Self {
        Self {
            id,
            enabled: true,
            state: CaState::Initialized,
            max_ca_pmt: max_ca_pmt.min(MAX_CA_PMT),
            multiple_pmt,
            ..Self::default()
        }
    }

    fn ctx<'a>(&'a self, programs: &'a ProgramTable) -> BuildCtx<'a> {
        BuildCtx {
            programs,
            caid_filter: &self.caid_filter,
        }
    }

    fn slots(&self) -> std::ops::Range<usize> {
        0..self.max_ca_pmt.min(MAX_CA_PMT)
    }

    /// `get_max_pmt_for_ca`.
    pub fn max_pmts(&self) -> usize {
        (self.multiple_pmt + 1) * self.max_ca_pmt
    }

    /// `get_active_capmts`.
    pub fn active_capmts(&self) -> usize {
        self.slots()
            .filter(|&i| self.capmt[i].pmt_count() > 0)
            .count()
    }

    /// `get_enabled_pmts_for_ca`.
    pub fn enabled_pmts(&self) -> usize {
        self.slots().map(|i| self.capmt[i].pmt_count()).sum()
    }

    /// Slot carrying the program, if any.
    pub fn slot_of(&self, pmt_id: i32) -> Option<usize> {
        self.slots().find(|&i| self.capmt[i].has_pmt(pmt_id))
    }

    /// `add_pmt_to_capmt`: the slot index now carrying the program.
    pub fn add_pmt_to_capmt(
        &mut self,
        programs: &ProgramTable,
        pmt_id: i32,
        multiple: usize,
    ) -> Option<usize> {
        if let Some(i) = self.slot_of(pmt_id) {
            return Some(i);
        }
        let pmt = programs.get(pmt_id)?;
        // A program back before the next send takes its kept place, matched by streams.
        let mut es = Vec::new();
        let mut len = capmt_add_pmt(
            &mut es,
            8192,
            pmt,
            CMD_ID_OK_DESCRAMBLING,
            self.multiple_pmt > 0,
            self.ctx(programs),
        );
        if len > CAPMT_KEPT_ES_MAX {
            len = 0;
        }
        if len > 0 {
            for i in self.slots() {
                let c = &mut self.capmt[i];
                for place in 0..MAX_PMT_PER_CAPMT {
                    if !pmt_id_is_valid(c.place(place)) && c.sent_es[place] == es {
                        *c.place_mut(place) = pmt_id;
                        return Some(i);
                    }
                }
            }
        }
        // Prefer an empty CAPMT: packing next to a running PMT restarts that channel.
        if let Some(i) = self.slots().find(|&i| self.capmt[i].pmt_count() == 0) {
            self.capmt[i].pmt_id = pmt_id;
            return Some(i);
        }
        if multiple == 0 {
            return None;
        }
        // Least loaded CAPMT; on a tie the one without kept places.
        let limit = (multiple + 1).min(MAX_PMT_PER_CAPMT);
        let load = |c: &Capmt| (c.pmt_count(), c.kept_count());
        let mut best: Option<usize> = None;
        for i in self.slots() {
            let c = &self.capmt[i];
            if c.pmt_count() < limit && best.is_none_or(|b| load(c) < load(&self.capmt[b])) {
                best = Some(i);
            }
        }
        let b = best?;
        let c = &mut self.capmt[b];
        if !pmt_id_is_valid(c.pmt_id) {
            c.pmt_id = pmt_id;
        } else if let Some(id) = c.other_id.iter_mut().find(|id| !pmt_id_is_valid(**id)) {
            *id = pmt_id;
        }
        Some(b)
    }

    /// `remove_pmt_from_device`: the place stays empty until the next send.
    pub fn remove_pmt_from_device(&mut self, pmt_id: i32) {
        for i in self.slots() {
            let c = &mut self.capmt[i];
            if !c.has_pmt(pmt_id) {
                continue;
            }
            for place in 0..MAX_PMT_PER_CAPMT {
                if c.place(place) == pmt_id {
                    *c.place_mut(place) = crate::capmt::PMT_INVALID;
                }
            }
            if c.pmt_count() == 0 {
                c.capmt_hash = 0;
                c.capmt_hash_valid = false;
                c.forget_sent();
            }
        }
    }

    /// `send_capmt`: builds, suppresses unchanged updates, bumps the version and writes.
    pub fn send_capmt(
        &mut self,
        slot: usize,
        listmgmt: u8,
        reason: u8,
        force: bool,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) -> SendOutcome {
        let added_only = self.multiple_pmt > 0;
        let filter = self.caid_filter.clone();
        let ctx = BuildCtx {
            programs,
            caid_filter: &filter,
        };
        let c = &mut self.capmt[slot];
        let Ok(probe) = create_capmt(c, listmgmt, 8192, reason, added_only, true, ctx) else {
            return SendOutcome::BuildFailed;
        };
        let hash = capmt_content_hash(&probe.bytes);
        if !force && c.capmt_hash_valid && hash == c.capmt_hash {
            return SendOutcome::Unchanged;
        }
        c.compact();
        c.version = (c.version + 1) & 0xF;
        let Ok(built) = create_capmt(c, listmgmt, 8192, reason, added_only, false, ctx) else {
            return SendOutcome::BuildFailed;
        };
        if !driver.has_ca_session() {
            c.forget_sent();
            return SendOutcome::NoSession;
        }
        if !driver.send_ca_pmt(&built.bytes) {
            c.forget_sent();
            return SendOutcome::WriteFailed;
        }
        c.capmt_hash = capmt_content_hash(&built.bytes);
        c.capmt_hash_valid = true;
        for (place, &(start, len)) in built.spans.iter().enumerate() {
            c.sent_es[place].clear();
            if len > 0 && len <= CAPMT_KEPT_ES_MAX {
                c.sent_es[place].extend_from_slice(&built.bytes[start..start + len]);
            }
        }
        SendOutcome::Sent
    }

    /// `dvbca_process_pmt` without the enigma CI routing and CI+ key push.
    pub fn process_pmt(
        &mut self,
        pmt_id: i32,
        update: bool,
        programs: &ProgramTable,
        driver: &mut dyn CaDriver,
    ) -> TablesResult {
        if self.state != CaState::Initialized {
            return TablesResult::Retry;
        }
        let was_member = self.slot_of(pmt_id).is_some();
        let Some(slot) = self.add_pmt_to_capmt(programs, pmt_id, self.multiple_pmt) else {
            return TablesResult::Retry;
        };
        let Some(first_sid) = programs
            .get(self.capmt[slot].first_pmt_id())
            .map(|p| i32::from(p.sid))
        else {
            return TablesResult::NoRetry;
        };
        // Only a CAPMT starting fresh takes the real sid; a shared one keeps the running number.
        let fresh = !was_member && self.capmt[slot].pmt_count() == 1;
        let mut listmgmt = if self.active_capmts() == 1 {
            CLM_ONLY
        } else {
            CLM_UPDATE
        };
        if listmgmt == CLM_ONLY {
            if !fresh || self.capmt[slot].sid == first_sid {
                listmgmt = CLM_UPDATE;
            }
            if fresh {
                self.capmt[slot].sid = first_sid;
            }
            let own_sid = self.capmt[slot].sid;
            for i in self.slots() {
                if i == slot {
                    continue;
                }
                let sid = make_sid_for_ca(self.id, i as i32);
                self.capmt[i].sid = if own_sid == sid {
                    make_sid_for_ca(self.id, MAX_CA_PMT as i32)
                } else {
                    sid
                };
            }
        }
        if self
            .send_capmt(
                slot,
                listmgmt,
                CMD_ID_OK_DESCRAMBLING,
                !update,
                programs,
                driver,
            )
            .is_error()
        {
            return TablesResult::NoRetry;
        }
        TablesResult::Ok
    }

    /// `dvbca_del_pmt`: the last program of a CAPMT is released as not_selected.
    pub fn del_pmt(&mut self, pmt_id: i32, programs: &ProgramTable, driver: &mut dyn CaDriver) {
        if !self.enabled || self.state != CaState::Initialized {
            return;
        }
        let Some(slot) = self.slot_of(pmt_id) else {
            return;
        };
        if self.capmt[slot].pmt_count() == 1 {
            // A failed release is only logged by C++.
            let _ = self.send_capmt(
                slot,
                CLM_UPDATE,
                CMD_ID_NOT_SELECTED,
                true,
                programs,
                driver,
            );
        }
        self.remove_pmt_from_device(pmt_id);
    }

    /// `ca_release_pmts`: forget every program after the CAM went away.
    pub fn release_pmts(&mut self) -> Vec<i32> {
        let mut ids = Vec::new();
        for i in self.slots() {
            for place in 0..MAX_PMT_PER_CAPMT {
                let id = self.capmt[i].place(place);
                if pmt_id_is_valid(id) {
                    ids.push(id);
                }
            }
        }
        self.capmt = Default::default();
        ids
    }
}
