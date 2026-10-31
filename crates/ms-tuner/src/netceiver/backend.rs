//! `Tuner` for one netceiver tuner: the `SNetceiver` state machine of netceiver.cpp.

use std::sync::Arc;

use super::api::{FE_HAS_LOCK, McliReceiverApi, NETCV_PIPE_SIZE, NetcvShared, ReceiverId};
use super::params::{TuneInputs, mcli_tune, tune_inputs};
use crate::names;
use crate::tuner::{
    DeliverySystem, PidSet, Result, SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner,
};

/// `MAX_PIDS` (adapter.h), the size of `SNetceiver::npid`.
pub const MAX_PIDS: usize = 128;

/// `map_posc` in `netcv_commit` (netceiver.cpp:235-236).
const MAP_POSC: [&str; 5] = ["", " @ 19.2E", " @ 13E", " @ 28.2E", " @ 5W"];

/// `fe_delivery_system_t` number of the requested system.
fn sys_number(sys: DeliverySystem) -> i32 {
    match sys {
        DeliverySystem::DvbS => 5,
        DeliverySystem::DvbS2 => 6,
        DeliverySystem::DvbT => 3,
        DeliverySystem::DvbT2 => 16,
        DeliverySystem::DvbC => 1,
        DeliverySystem::DvbC2 => 19,
        DeliverySystem::Undefined => 0,
        DeliverySystem::Other(value) => value,
    }
}

/// One netceiver adapter (`SNetceiver` plus the `ad->*` callbacks).
pub struct NetceiverTuner<A: McliReceiverApi> {
    api: A,
    id: usize,
    shared: Arc<NetcvShared>,
    rx: Option<ReceiverId>,
    err: bool,
    npid: [u16; MAX_PIDS],
    lp: usize,
    want_tune: bool,
    want_commit: bool,
    tp: TuneParams,
    applied: Vec<u16>,
    partial: Vec<u8>,
}

impl<A: McliReceiverApi> NetceiverTuner<A> {
    /// `netcv_open_device` for logical adapter `id`.
    pub fn new(api: A, id: usize) -> Self {
        Self {
            api,
            id,
            shared: NetcvShared::new(),
            rx: None,
            err: false,
            npid: [0; MAX_PIDS],
            lp: 0,
            want_tune: false,
            want_commit: false,
            tp: TuneParams::default(),
            applied: Vec::new(),
            partial: Vec::with_capacity(NETCV_PIPE_SIZE.min(64 * 1024)),
        }
    }

    /// The libmcli binding.
    pub fn api(&self) -> &A {
        &self.api
    }

    /// Mutable libmcli binding.
    pub fn api_mut(&mut self) -> &mut A {
        &mut self.api
    }

    /// State written by the libmcli callbacks.
    pub fn shared(&self) -> &Arc<NetcvShared> {
        &self.shared
    }

    /// Active PIDs in `npid` order.
    pub fn active_pids(&self) -> &[u16] {
        &self.npid[..self.lp]
    }

    /// `netcv_set_pid`: queues `pid`; returns the C fd (`id + 100`, or 0 after an error).
    pub fn set_pid(&mut self, pid: u16) -> i32 {
        ms_log::log!(
            "netceiver: set_pid for adapter {}, pid {}, err {}",
            self.id,
            pid,
            i32::from(self.err)
        );
        if self.err {
            return 0;
        }
        // C++ writes past npid[MAX_PIDS - 1]; the extra PID is refused instead.
        if self.lp >= MAX_PIDS {
            return -1;
        }
        self.npid[self.lp] = pid;
        self.lp += 1;
        self.shared.set_active_pids(self.lp);
        self.want_commit = true;
        self.id as i32 + 100
    }

    /// `netcv_del_pid`: shifts the first match out of the whole `npid` array.
    pub fn del_pid(&mut self, pid: u16) -> i32 {
        // C++ logs `fd - 100`, which is the adapter id passed to `set_pid`.
        ms_log::log!(
            "netceiver: del_pid for aid {}, pid {}, err {}",
            self.id,
            pid,
            i32::from(self.err)
        );
        if self.err {
            return 0;
        }
        let mut hit = false;
        for i in 0..MAX_PIDS - 1 {
            if self.npid[i] == pid || hit {
                self.npid[i] = self.npid[i + 1];
                hit = true;
            }
        }
        if hit && self.lp > 0 {
            self.lp -= 1;
        }
        self.shared.set_active_pids(self.lp);
        self.want_commit = true;
        0
    }

    /// `netcv_tune`: only arms `want_tune`; `commit` does the work.
    pub fn netcv_tune(&mut self, tp: &TuneParams) {
        // C++ logs `ad->id` before checking `ad`; the Rust API has no null adapter.
        ms_log::log!("netceiver: called netcv_tune for adapter {}", self.id);
        self.tp = tp.clone();
        self.want_tune = true;
        self.want_commit = false;
        self.lp = 0;
        self.shared.set_active_pids(0);
    }

    /// `netcv_commit`: creates the receiver, tunes, then pushes or stops the PID list.
    pub fn commit(&mut self) {
        ms_log::log!(
            "netceiver: call netcv_commit for adapter {} want_tune={}",
            self.id,
            i32::from(self.want_tune)
        );
        self.err = false;
        if self.rx.is_none() {
            ms_log::log!(
                "netceiver: add a new receiver instance for adapter {}",
                self.id
            );
            self.rx = self.api.recv_add(Arc::clone(&self.shared));
            self.err = self.rx.is_none();
        }
        if self.want_tune {
            let inputs = tune_inputs(&self.tp);
            ms_log::log!(
                "netceiver: netcv_commit tune tp->sys={}",
                sys_number(self.tp.msys)
            );
            if let Some(rx) = self.rx {
                match self.tp.msys {
                    DeliverySystem::DvbS | DeliverySystem::DvbS2 => {
                        ms_log::log!(
                            "netceiver: adapter {} tuning to {}{} pol:{} sr:{} fec:{} delsys:{} mod:{}",
                            self.id,
                            inputs.freq / 1000,
                            MAP_POSC[inputs.diseqc as usize],
                            names::fe_pol(inputs.pol),
                            inputs.sr / 1000,
                            names::fe_fec(inputs.fec),
                            names::fe_delsys(sys_number(self.tp.msys)),
                            names::fe_modulation(inputs.mtype)
                        );
                    }
                    DeliverySystem::DvbC => {
                        ms_log::log!(
                            "netceiver: adapter {} tuning to {} sr:{} delsys:{} mod:{}",
                            self.id,
                            inputs.freq / 1000,
                            inputs.sr / 1000,
                            names::fe_delsys(sys_number(self.tp.msys)),
                            names::fe_modulation(inputs.mtype)
                        );
                    }
                    DeliverySystem::DvbT => {
                        let TuneInputs {
                            inversion,
                            mtype,
                            hprate,
                            tmode,
                            gi,
                            bw,
                            sm,
                            t2id,
                            ..
                        } = inputs;
                        // netceiver.cpp:304-306: logged while choosing the
                        // bandwidth, before the tuning line.
                        if !matches!(
                            bw,
                            8_000_000
                                | 7_000_000
                                | 6_000_000
                                | 0
                                | 5_000_000
                                | 10_000_000
                                | 1_712_000
                        ) {
                            ms_log::log!("netceiver: DVB-T: unknown bandwith: {}", bw);
                        }
                        ms_log::log!(
                            "netceiver: adapter {} tuning to {} inv: {} mtype: {} hprate: {} tmode: {} gi: {} bw:{} sm {} t2id {}",
                            self.id,
                            inputs.freq / 1000,
                            inversion,
                            mtype,
                            hprate,
                            tmode,
                            gi,
                            bw,
                            sm,
                            t2id
                        );
                    }
                    _ => {
                        ms_log::log!(
                            "netceiver: adapter {} tuning not supported for type {}",
                            self.id,
                            sys_number(self.tp.msys)
                        );
                    }
                }
                if self.api.recv_tune(rx, &mcli_tune(&self.tp)) != 0 {
                    ms_log::log!("netceiver: Tuning receiver failed");
                }
            }
            self.want_tune = false;
        }
        if self.want_commit {
            if let Some(rx) = self.rx {
                if self.lp > 0 {
                    self.api.sleep_ms(10);
                    let pids = self.npid[..self.lp].to_vec();
                    if self.api.recv_pids(rx, &pids) != 0 {
                        ms_log::log!("netceiver: seting PIDs failed");
                    }
                } else if self.api.recv_stop(rx) != 0 {
                    ms_log::log!("netceiver: removing all PIDs failed");
                }
            }
            self.want_commit = false;
        }
        self.shared.set_active_pids(self.lp);
    }

    /// `netcv_close`: unregisters the handlers and deletes the receiver.
    pub fn close(&mut self) {
        self.want_commit = false;
        if self.rx.is_none() {
            ms_log::log!("netceiver: receiver instance is NULL (id={})", self.id);
        }
        ms_log::log!(
            "netceiver: delete receiver instance for adapter {}",
            self.id
        );
        if let Some(rx) = self.rx.take() {
            self.api.recv_del(rx);
        }
        self.shared.set_active_pids(0);
    }
}

impl<A: McliReceiverApi> Drop for NetceiverTuner<A> {
    fn drop(&mut self) {
        self.close();
    }
}

impl<A: McliReceiverApi> Tuner for NetceiverTuner<A> {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        self.netcv_tune(params);
        self.shared.reset_signal();
        // netcv_tune forgets every PID; re-add the current set so it survives the retune.
        let applied = std::mem::take(&mut self.applied);
        for pid in applied {
            if self.set_pid(pid) > 0 {
                self.applied.push(pid);
            }
        }
        if !self.applied.is_empty() {
            self.commit();
        }
        Ok(())
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<()> {
        let mut changed = false;
        let removed: Vec<u16> = self
            .applied
            .iter()
            .rev()
            .filter(|p| !pids.contains_exact(**p))
            .copied()
            .collect();
        for pid in removed {
            self.del_pid(pid);
            self.applied.retain(|p| *p != pid);
            changed = true;
        }
        for pid in pids.iter() {
            if self.applied.contains(&pid) {
                continue;
            }
            changed = true;
            if self.set_pid(pid) > 0 {
                self.applied.push(pid);
            }
        }
        if changed {
            self.commit();
        }
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        let s = self.shared.signal();
        SignalStatus {
            locked: s.status & FE_HAS_LOCK != 0,
            strength: s.strength,
            snr: s.snr,
            ber: s.ber,
            // The netceiver feed carries no dB SNR (ad->db stays MAX_DB).
            db: SignalStatus::DB_UNKNOWN,
            status_valid: true,
        }
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        let want = buf.remaining_bytes().saturating_sub(self.partial.len());
        self.shared.drain(&mut self.partial, want);
        let mut appended = 0;
        while self.partial.len() - appended >= TS_PACKET_SIZE
            && buf.push_packet(&self.partial[appended..appended + TS_PACKET_SIZE])
        {
            appended += TS_PACKET_SIZE;
        }
        self.partial.drain(..appended);
        Ok(appended)
    }
}
