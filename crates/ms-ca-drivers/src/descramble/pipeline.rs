//! Per-adapter descrambling pipeline (`send_cw`, `update_cw`, `pmt_decrypt_stream`).

use std::collections::HashMap;

use ms_ca::CwAlgo;
use ms_config::DropEncrypted;

use super::cipher::{Cipher, CipherRegistry, KeyMaterial};

/// `DEFAULT_LOG` of `pmt.cpp` (`#define DEFAULT_LOG LOG_PMT`).
const DEFAULT_LOG: u32 = ms_log::LOG_PMT;

/// TS packet size (`DVB_FRAME`).
pub const DVB_FRAME: usize = 188;
/// CW slots per descrambler (`MAX_CW`).
pub const MAX_CW: usize = 200;
/// CW lifetime when the CA gives no expiry (`MAX_CW_TIME`, ms).
pub const MAX_CW_TIME: i64 = 45_000;
/// Start packets tried when validating a CW (`start[10]`, 9 used).
pub const MAX_START_PACKETS: usize = 9;
/// Lifetime left to CWs replaced by a validated one (ms).
pub const REPLACED_CW_EXPIRY: i64 = 10_000;

const NO_PMT: u32 = u32::MAX;

/// Why [`Descrambler::send_cw`] rejected a CW (C++ return 1 or 3).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendCwError {
    /// No service with this PMT id (1).
    PmtNotFound,
    /// The same key is already live for this PMT and parity (1).
    Duplicate,
    /// Every slot is in use (1).
    Full,
    /// No cipher registered for the algorithm (3).
    NoAlgo,
}

/// One CW slot (`SCW`).
struct CwSlot {
    enabled: bool,
    key: KeyMaterial,
    time: i64,
    expiry: i64,
    set_time: i64,
    algo: CwAlgo,
    pmt: usize,
    parity: u8,
    cipher: Box<dyn Cipher>,
    /// Never print this slot's key or IV (`<redacted>`).
    redact: bool,
}

/// Descrambling state of one service (the `SPMT` decrypt fields).
struct Service {
    pids: Vec<u16>,
    parity: i8,
    update_cw: bool,
    last_update_cw: i64,
    last_decrypt_ok: i64,
    last_scrambled_pkt: i64,
    cw: Option<usize>,
    batch: Vec<usize>,
}

/// Decrypt statistics of one service, for the CA watchdogs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ServiceStats {
    /// PMT slot id.
    pub pmt_id: usize,
    /// Last batch decrypted with a verified CW (ms), 0 for never.
    pub last_decrypt_ok: i64,
    /// Last scrambled packet (ms), 0 for never.
    pub last_scrambled_pkt: i64,
    /// Active parity, -1 before the first scrambled packet.
    pub parity: i8,
    /// Slot of the active CW.
    pub cw: Option<usize>,
}

/// One pid of the adapter pid table as the descrambler counts it (`SPid`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PidCounters {
    /// The pid is in the adapter pid table (`find_pid` finds it).
    pub tracked: bool,
    /// `PID_STATE_ACTIVE`; only active slots count for `ad_decerr`.
    pub active: bool,
    /// Claiming PMT (`p->pmt`), -1 when none.
    pub claim: i32,
    /// `p->packets` since the pid became active.
    pub packets: u64,
    /// `p->dec_err`: scrambled packets since the last reset.
    pub dec_err: u64,
    /// `p->is_decrypted`: a clear packet was seen.
    pub decrypted: bool,
}

/// One entry of [`Descrambler::set_adapter_pids`]: pid, active, claiming PMT.
pub type AdapterPid = (u16, bool, i32);

/// Owns the CW slots and per-service batches of one adapter.
pub struct Descrambler {
    registry: CipherRegistry,
    slots: Vec<Option<CwSlot>>,
    services: HashMap<usize, Service>,
    pid_pmt: Box<[u32; 8192]>,
    order: Vec<usize>,
    dec_err: u64,
    clear_pkts: u64,
    pids: Box<[PidCounters; 8192]>,
}

/// Header length up to the payload (`get_adaptation_len`).
pub fn adaptation_len(b: &[u8]) -> usize {
    if b[3] & 0x20 != 0 {
        if b[4] < 183 {
            b[4] as usize + 5
        } else {
            DVB_FRAME
        }
    } else {
        4
    }
}

fn pid_of(b: &[u8]) -> u16 {
    (((b[1] & 0x1F) as u16) << 8) | b[2] as u16
}

/// Mutable payload slices of the packets at ascending `offsets` (`cw_decrypt_stream`).
fn payloads<'a>(buf: &'a mut [u8], offsets: &[usize]) -> Vec<&'a mut [u8]> {
    let mut out = Vec::with_capacity(offsets.len());
    let mut rest = buf;
    let mut base = 0;
    for &off in offsets {
        let tail = std::mem::take(&mut rest).split_at_mut(off - base).1;
        let (packet, tail) = tail.split_at_mut(DVB_FRAME);
        let start = adaptation_len(packet);
        out.push(&mut packet[start..]);
        rest = tail;
        base = off + DVB_FRAME;
    }
    out
}

/// Decrypts copies of `packets` and reports whether one starts with `00 00 01`
/// (`test_decrypt_packet` returning 0).
pub fn test_decrypt_packets(
    cipher: &mut dyn Cipher,
    key: &KeyMaterial,
    packets: &[[u8; DVB_FRAME]],
) -> bool {
    let mut copy = packets.concat();
    let offsets: Vec<usize> = (0..packets.len()).map(|i| i * DVB_FRAME).collect();
    for packet in packets {
        ms_log::hexdump!("test_decrypt_packet before: ", packet);
    }
    cipher.decrypt(key, &mut payloads(&mut copy, &offsets));
    for packet in copy.as_chunks::<DVB_FRAME>().0 {
        ms_log::hexdump!("test_decrypt_packet after: ", packet);
    }
    copy.as_chunks::<DVB_FRAME>().0.iter().any(|b| {
        let p = &b[adaptation_len(b)..];
        p.len() >= 3 && p[..3] == [0, 0, 1]
    })
}

fn mark_pid_null(b: &mut [u8]) {
    b[1] |= 0x1F;
    b[2] |= 0xFF;
}

fn has_pcr(b: &[u8]) -> bool {
    b[3] & 0x20 != 0 && b[4] != 0 && b[5] & 0x10 != 0
}

fn mark_padding_header(b: &mut [u8]) {
    if b[3] & 0x10 == 0 {
        b[3] &= 0x3F;
        return;
    }
    let payload = adaptation_len(b);
    if payload < 184 {
        b[payload..].fill(0xFF);
        b[4] = (DVB_FRAME - 5) as u8;
        if payload < 5 {
            b[5] = 0;
        }
    }
    b[1] &= 0x3F;
    b[3] &= 0x2F;
    b[3] |= 0x20;
}

fn mark_pcr_only(b: &mut [u8]) {
    if b[3] & 0x10 == 0 {
        mark_pid_null(b);
        return;
    }
    let payload = adaptation_len(b);
    let stuffed = DVB_FRAME.saturating_sub(payload);
    b[payload.min(DVB_FRAME)..].fill(0xFF);
    b[4] = b[4].wrapping_add(stuffed as u8);
    b[1] &= 0x3F;
    b[3] &= 0x2F;
    b[5] |= 0x80;
    b[5] &= 0x9F;
}

/// `cw_to_string`: the human-readable form of one CW slot.
fn cw_to_string(id: usize, cw: &CwSlot, now: i64) -> String {
    let mut out = format!(
        "id {}, parity {}, pmt {},{} time {} ms ago, expiry in {} ms",
        id,
        cw.parity,
        cw.pmt,
        if cw.algo == CwAlgo::CsaIcam {
            " ICAM,"
        } else {
            ""
        },
        now - cw.time,
        cw.expiry - now,
    );
    if cw.redact {
        out.push_str(", CW: <redacted>");
        if cw.key.iv[0] != 0 {
            out.push_str(", IV: <redacted>");
        }
        return out;
    }
    out.push_str(&format!(
        ", CW: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
        cw.key.cw[0],
        cw.key.cw[1],
        cw.key.cw[2],
        cw.key.cw[3],
        cw.key.cw[4],
        cw.key.cw[5],
        cw.key.cw[6],
        cw.key.cw[7]
    ));
    if cw.key.iv[0] != 0 {
        out.push_str(&format!(
            ", IV: {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X} {:02X}",
            cw.key.iv[0],
            cw.key.iv[1],
            cw.key.iv[2],
            cw.key.iv[3],
            cw.key.iv[4],
            cw.key.iv[5],
            cw.key.iv[6],
            cw.key.iv[7]
        ));
    }
    out
}

impl Descrambler {
    /// A descrambler using the ciphers in `registry`.
    pub fn new(registry: CipherRegistry) -> Self {
        Self {
            registry,
            slots: Vec::new(),
            services: HashMap::new(),
            pid_pmt: Box::new([NO_PMT; 8192]),
            order: Vec::new(),
            dec_err: 0,
            clear_pkts: 0,
            pids: vec![PidCounters::default(); 8192]
                .into_boxed_slice()
                .try_into()
                .expect("8192 pid entries"),
        }
    }

    /// Mirrors the adapter pid table: a new pid starts at zero (slot init in
    /// update_pids), a changed claim clears `decrypted` (pmt.cpp:1264).
    pub fn set_adapter_pids(&mut self, table: &[AdapterPid]) {
        let mut seen = [false; 8192];
        for &(pid, active, claim) in table {
            let Some(entry) = self.pids.get_mut(usize::from(pid)) else {
                continue;
            };
            seen[usize::from(pid)] = true;
            if !entry.tracked {
                *entry = PidCounters {
                    tracked: true,
                    claim,
                    ..PidCounters::default()
                };
            } else if entry.claim != claim {
                entry.claim = claim;
                entry.decrypted = false;
            }
            // NEW -> ACTIVE re-initialises the slot counters (adapter.cpp:1015-1028).
            if active && !entry.active {
                entry.packets = 0;
                entry.dec_err = 0;
            }
            entry.active = active;
        }
        for (pid, entry) in self.pids.iter_mut().enumerate() {
            if entry.tracked && !seen[pid] {
                *entry = PidCounters::default();
            }
        }
    }

    /// `p->packets++` for tracked pids (check_cc, stream.cpp:899-923): nothing
    /// is counted while pid 8192 is requested, and TEI packets are skipped.
    pub fn count_packets(&mut self, buf: &[u8], pid_all_requested: bool) {
        if pid_all_requested {
            return;
        }
        for b in buf.as_chunks::<DVB_FRAME>().0 {
            if b[1] & 0x80 != 0 {
                continue;
            }
            let entry = &mut self.pids[usize::from(pid_of(b))];
            if entry.tracked {
                entry.packets += 1;
            }
        }
    }

    /// `get_adapter_decerrs` (adapter.cpp:2363): the dec errors of the first
    /// two active slots, which sort_pids orders by packets.
    pub fn adapter_decerr(&self) -> u64 {
        let mut active: Vec<&PidCounters> =
            self.pids.iter().filter(|p| p.tracked && p.active).collect();
        active.sort_by_key(|p| std::cmp::Reverse(p.packets));
        active.iter().take(2).map(|p| p.dec_err).sum()
    }

    /// Counters of one pid, for tests and diagnostics.
    pub fn pid_counters(&self, pid: u16) -> PidCounters {
        self.pids[usize::from(pid & 0x1FFF)]
    }

    /// Starts tracking a service and maps its PIDs to it (`pmt_add` + `p->pmt`).
    pub fn add_service(&mut self, pmt_id: usize, pids: &[u16]) {
        self.remove_service(pmt_id);
        let pids: Vec<u16> = pids.iter().copied().filter(|&p| p < 8192).collect();
        for &pid in &pids {
            self.pid_pmt[pid as usize] = pmt_id as u32;
        }
        self.services.insert(
            pmt_id,
            Service {
                pids,
                parity: -1,
                update_cw: true,
                last_update_cw: 0,
                last_decrypt_ok: 0,
                last_scrambled_pkt: 0,
                cw: None,
                batch: Vec::new(),
            },
        );
    }

    /// Remaps a tracked service's PIDs, keeping its CWs (`p->pmt` claim moves).
    pub fn set_service_pids(&mut self, pmt_id: usize, pids: &[u16]) {
        let Some(service) = self.services.get_mut(&pmt_id) else {
            self.add_service(pmt_id, pids);
            return;
        };
        for &pid in &service.pids {
            if self.pid_pmt[pid as usize] == pmt_id as u32 {
                self.pid_pmt[pid as usize] = NO_PMT;
            }
        }
        service.pids = pids.iter().copied().filter(|&p| p < 8192).collect();
        for &pid in &service.pids {
            self.pid_pmt[pid as usize] = pmt_id as u32;
        }
        let pids = service.pids.clone();
        for slot in self.slots.iter_mut().flatten() {
            if slot.pmt == pmt_id {
                slot.cipher.set_context(pmt_id, slot.parity, &pids);
            }
        }
    }

    /// Drops a service and disables its CWs (`pmt_del` + `clear_cw_for_pmt`).
    pub fn remove_service(&mut self, pmt_id: usize) {
        let Some(service) = self.services.remove(&pmt_id) else {
            return;
        };
        for pid in service.pids {
            if self.pid_pmt[pid as usize] == pmt_id as u32 {
                self.pid_pmt[pid as usize] = NO_PMT;
            }
        }
        for parity in 0..2 {
            for slot in self.slots.iter_mut().flatten() {
                if slot.enabled && slot.pmt == pmt_id && slot.parity == parity {
                    slot.cipher.stop_cw(&slot.key);
                    slot.enabled = false;
                }
            }
        }
    }

    /// Whether `pmt_id` is tracked.
    pub fn has_service(&self, pmt_id: usize) -> bool {
        self.services.contains_key(&pmt_id)
    }

    /// Statistics of one service.
    pub fn stats(&self, pmt_id: usize) -> Option<ServiceStats> {
        self.services.get(&pmt_id).map(|s| ServiceStats {
            pmt_id,
            last_decrypt_ok: s.last_decrypt_ok,
            last_scrambled_pkt: s.last_scrambled_pkt,
            parity: s.parity,
            cw: s.cw,
        })
    }

    /// Statistics of every service, by PMT id.
    pub fn all_stats(&self) -> Vec<ServiceStats> {
        let mut ids: Vec<usize> = self.services.keys().copied().collect();
        ids.sort_unstable();
        ids.into_iter().filter_map(|id| self.stats(id)).collect()
    }

    /// Scrambled packets left after decrypting (`ad->dec_err`).
    pub fn dec_err(&self) -> u64 {
        self.dec_err
    }

    /// Clear data packets (pid above 0x1F) seen while the CA is active; with
    /// `dec_err` this gives the scrambled share of the adapter's output.
    pub fn clear_pkts(&self) -> u64 {
        self.clear_pkts
    }

    /// Overrides the decrypt stamps, like tests that reset `SPMT` fields.
    pub fn set_stamps(&mut self, pmt_id: usize, last_decrypt_ok: i64, last_scrambled_pkt: i64) {
        if let Some(s) = self.services.get_mut(&pmt_id) {
            s.last_decrypt_ok = last_decrypt_ok;
            s.last_scrambled_pkt = last_scrambled_pkt;
        }
    }

    /// Marks the active CW as stale so the next batch searches again (`disable_cw`).
    pub fn disable_cw(&mut self, pmt_id: usize) {
        if let Some(s) = self.services.get_mut(&pmt_id) {
            s.update_cw = true;
        }
    }

    /// Key bytes and parity of a slot, for tests and status output.
    pub fn slot(&self, id: usize) -> Option<(usize, u8, &[u8], bool)> {
        let s = self.slots.get(id)?.as_ref()?;
        Some((s.pmt, s.parity, &s.key.cw[..s.key.cw_len], s.enabled))
    }

    /// Sets a slot's expiry (ms), like tests that age `cws[i]->expiry`.
    pub fn set_slot_expiry(&mut self, id: usize, expiry: i64) {
        if let Some(Some(s)) = self.slots.get_mut(id) {
            s.expiry = expiry;
        }
    }

    /// A slot's expiry (ms).
    pub fn slot_expiry(&self, id: usize) -> Option<i64> {
        self.slots.get(id)?.as_ref().map(|s| s.expiry)
    }

    /// Makes `slot` the service's active CW, as `update_cw` would.
    pub fn set_active_cw(&mut self, pmt_id: usize, slot: Option<usize>) {
        if let Some(s) = self.services.get_mut(&pmt_id) {
            s.cw = slot;
        }
    }

    /// Validates a slot's key against `packets` (`test_decrypt_packet`).
    pub fn test_slot(&mut self, id: usize, packets: &[[u8; DVB_FRAME]]) -> bool {
        match self.slots.get_mut(id) {
            Some(Some(s)) => test_decrypt_packets(s.cipher.as_mut(), &s.key, packets),
            _ => false,
        }
    }

    /// The `dump_cws` line of a live slot, for tests.
    #[cfg(test)]
    pub fn cw_dump_line(&self, id: usize, now: i64) -> Option<String> {
        let slot = self.slots.get(id)?.as_ref()?;
        (slot.enabled && slot.expiry > now).then(|| cw_to_string(id, slot, now))
    }

    /// `dump_cws`: logs every live CW slot.
    fn dump_cws(&self, now: i64) {
        if !ms_log::enabled(ms_log::LOG_GENERAL) {
            return;
        }
        ms_log::log_unrecorded!("List of CWs:");
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(s) = slot
                && s.enabled
                && s.expiry > now
            {
                ms_log::log_unrecorded!("* CW {}: {}", i, cw_to_string(i, s, now));
            }
        }
    }

    fn cw_in_use(&self, slot: usize, pmt: usize) -> bool {
        self.services.get(&pmt).is_some_and(|s| s.cw == Some(slot))
    }

    /// Stores a CW for a service (`send_cw`); `expiry_s == 0` means `MAX_CW_TIME`.
    #[allow(clippy::too_many_arguments)]
    pub fn send_cw(
        &mut self,
        pmt_id: usize,
        algo: CwAlgo,
        parity: u8,
        cw: &[u8],
        iv: Option<&[u8]>,
        expiry_s: i64,
        icam_ecm: u8,
        now: i64,
    ) -> Result<usize, SendCwError> {
        self.send_cw_with(pmt_id, algo, parity, cw, iv, expiry_s, icam_ecm, false, now)
    }

    /// Like [`Self::send_cw`], but the slot never prints its key or IV.
    #[allow(clippy::too_many_arguments)]
    pub fn send_cw_redacted(
        &mut self,
        pmt_id: usize,
        algo: CwAlgo,
        parity: u8,
        cw: &[u8],
        iv: Option<&[u8]>,
        expiry_s: i64,
        icam_ecm: u8,
        now: i64,
    ) -> Result<usize, SendCwError> {
        self.send_cw_with(pmt_id, algo, parity, cw, iv, expiry_s, icam_ecm, true, now)
    }

    #[allow(clippy::too_many_arguments)]
    fn send_cw_with(
        &mut self,
        pmt_id: usize,
        algo: CwAlgo,
        parity: u8,
        cw: &[u8],
        iv: Option<&[u8]>,
        expiry_s: i64,
        icam_ecm: u8,
        redact: bool,
        now: i64,
    ) -> Result<usize, SendCwError> {
        if !self.services.contains_key(&pmt_id) {
            ms_log::log_and_return!(
                Err(SendCwError::PmtNotFound),
                "send_cw: pmt not found {}",
                pmt_id
            );
        }
        let Some(factory) = self.registry.get(algo) else {
            ms_log::log_and_return!(
                Err(SendCwError::NoAlgo),
                "op not found for algo {}",
                crate::algo::Algo::from(algo).raw()
            );
        };
        let mut bytes = [0u8; 32];
        let n = cw.len().min(32);
        bytes[..n].copy_from_slice(&cw[..n]);
        let mut duplicate = None;
        for (i, slot) in self.slots.iter().enumerate() {
            if let Some(s) = slot
                && s.enabled
                && s.pmt == pmt_id
                && s.parity == parity
                && now < s.expiry
                && bytes[..s.key.cw_len] == s.key.cw[..s.key.cw_len]
            {
                duplicate = Some(i);
                break;
            }
        }
        if let Some(i) = duplicate {
            let desc = cw_to_string(i, self.slots[i].as_ref().expect("matched slot"), now);
            // The description carries the key: keep it on the local outputs.
            ms_log::log_unrecorded!("cw already exist at position {}: {} ", i, desc);
            return Err(SendCwError::Duplicate);
        }

        let free = (0..MAX_CW).find(|&i| match self.slots.get(i) {
            None | Some(None) => true,
            Some(Some(s)) => {
                s.algo == algo && !self.cw_in_use(i, s.pmt) && (!s.enabled || now > s.expiry)
            }
        });
        let Some(id) = free else {
            ms_log::log!("CWS is full {}", MAX_CW);
            self.dump_cws(now);
            return Err(SendCwError::Full);
        };
        if id >= self.slots.len() {
            self.slots.resize_with(id + 1, || None);
        }

        let service = &self.services[&pmt_id];
        let current = service
            .cw
            .and_then(|c| self.slots[c].as_ref())
            .map(|c| c.time);
        let mut time = now;
        if parity as i8 == service.parity
            && service.last_update_cw > 0
            && let Some(cur_time) = current
            && !service.update_cw
        {
            // A CW for the active parity may be fake: rank it below the active one.
            time = cur_time - 1000;
        }

        let cw_len = match algo {
            CwAlgo::Csa | CwAlgo::Des | CwAlgo::CsaIcam => 8,
            _ => 16,
        };
        let mut key = KeyMaterial {
            cw: [0; 32],
            iv: [0; 32],
            cw_len,
            icam_ecm,
        };
        key.cw[..cw_len].copy_from_slice(&bytes[..cw_len]);
        if let Some(iv) = iv {
            let n = iv.len().min(cw_len);
            key.iv[..n].copy_from_slice(&iv[..n]);
        }

        let pids = self.services[&pmt_id].pids.clone();
        let slot = self.slots[id].get_or_insert_with(|| CwSlot {
            enabled: false,
            key,
            time,
            expiry: 0,
            set_time: 0,
            algo,
            pmt: pmt_id,
            parity,
            cipher: factory(),
            redact,
        });
        if slot.enabled {
            slot.cipher.stop_cw(&slot.key);
        }
        slot.key = key;
        slot.time = time;
        slot.expiry = now
            + if expiry_s == 0 {
                MAX_CW_TIME
            } else {
                expiry_s * 1000
            };
        slot.set_time = 0;
        slot.algo = algo;
        slot.pmt = pmt_id;
        slot.parity = parity;
        slot.redact = redact;
        slot.cipher.set_context(pmt_id, parity, &pids);
        slot.cipher.set_cw(&slot.key);
        slot.enabled = true;

        let service = self.services.get_mut(&pmt_id).expect("checked above");
        if service.cw.is_none() {
            service.update_cw = true;
        }
        Ok(id)
    }

    /// Picks the CW for the service's current parity (`update_cw`).
    fn update_cw(&mut self, pmt_id: usize, buf: &[u8], now: i64) {
        let Some(service) = self.services.get_mut(&pmt_id) else {
            return;
        };
        let starts: Vec<[u8; DVB_FRAME]> = service
            .batch
            .iter()
            .map(|&off| &buf[off..off + DVB_FRAME])
            .filter(|b| b[1] & 0xC0 == 0x40)
            .take(MAX_START_PACKETS)
            .map(|b| b.try_into().expect("188-byte packet"))
            .collect();
        let validated = !starts.is_empty();

        // No CW yet but the stream can validate one: search again (CI+ case).
        if service.cw.is_none() && !service.update_cw && validated {
            service.update_cw = true;
        }
        if !service.update_cw {
            return;
        }
        service.last_update_cw = now;
        service.cw = None;
        let parity = service.parity;

        let mut found: Option<(usize, i64)> = None;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            let Some(s) = slot else { continue };
            if !s.enabled || s.parity as i8 != parity || s.pmt != pmt_id || now > s.expiry {
                continue;
            }
            if validated {
                if test_decrypt_packets(s.cipher.as_mut(), &s.key, &starts) {
                    ms_log::logm_unrecorded!(
                        "correct CW found (len {}): {}",
                        starts.len(),
                        cw_to_string(i, s, now)
                    );
                    found = Some((i, s.time));
                    break;
                }
                continue;
            }
            // Unverifiable (parity change without PUSI): the newest CW wins.
            match found {
                None => {
                    ms_log::logm_unrecorded!("candidate CW {}", cw_to_string(i, s, now));
                    found = Some((i, s.time));
                }
                Some((_, time)) if time < s.time => {
                    ms_log::logm_unrecorded!("New candidate found: {}", cw_to_string(i, s, now));
                    found = Some((i, s.time));
                }
                _ => {}
            }
        }

        self.services.get_mut(&pmt_id).expect("present").update_cw = !validated;
        let Some((id, _)) = found else {
            // `if (opts.debug & LOG_PMT) dump_cws();`
            if ms_log::debug_enabled(DEFAULT_LOG) {
                self.dump_cws(now);
            }
            return;
        };
        self.services.get_mut(&pmt_id).expect("present").cw = Some(id);
        let active = self.slots[id].as_mut().expect("found slot");
        if active.set_time == 0 {
            active.set_time = now;
        }
        if validated {
            let parity = active.parity;
            for (i, slot) in self.slots.iter_mut().enumerate() {
                if let Some(s) = slot
                    && i != id
                    && s.enabled
                    && s.expiry > now
                    && s.pmt == pmt_id
                    && s.parity == parity
                    && s.set_time > 0
                {
                    s.expiry = now + REPLACED_CW_EXPIRY;
                    ms_log::log_unrecorded!(
                        "Expiring CW {} for pmt {}: {}",
                        i,
                        pmt_id,
                        cw_to_string(i, s, now)
                    );
                }
            }
        } else if ms_log::debug_enabled(DEFAULT_LOG) {
            self.dump_cws(now);
        }
    }

    /// Decrypts the packets batched for a service (`decrypt_batch`).
    fn decrypt_batch(&mut self, pmt_id: usize, buf: &mut [u8], now: i64) {
        if self
            .services
            .get(&pmt_id)
            .is_none_or(|s| s.batch.is_empty())
        {
            return;
        }
        self.update_cw(pmt_id, buf, now);
        let service = self.services.get_mut(&pmt_id).expect("present");
        let batch = std::mem::take(&mut service.batch);
        if let Some(slot) = service.cw.and_then(|c| self.slots[c].as_mut()) {
            slot.cipher.decrypt(&slot.key, &mut payloads(buf, &batch));
            for &off in &batch {
                buf[off + 3] &= 0x3F;
            }
            if !service.update_cw {
                service.last_decrypt_ok = now;
            }
        }
        service.batch = batch;
        service.batch.clear();
    }

    /// Decrypts every scrambled packet of a tracked service (`pmt_decrypt_stream`).
    /// Packets are batched per service and flushed on a parity change and at the end.
    pub fn decrypt_stream(&mut self, buf: &mut [u8], now: i64) {
        let len = buf.len() / DVB_FRAME * DVB_FRAME;
        let mut order = std::mem::take(&mut self.order);
        order.clear();
        for off in (0..len).step_by(DVB_FRAME) {
            let b = &buf[off..off + DVB_FRAME];
            if b[0] != 0x47 || b[3] & 0x80 == 0 {
                continue;
            }
            let pid = pid_of(b);
            let pmt = self.pid_pmt[pid as usize];
            if pmt == NO_PMT {
                // `p ? p->pmt : -3`: `pid_pmt` keeps no entry for untracked
                // pids, so the C++ default is what remains.
                ms_log::debugm_hot!(
                    "PMT not found for pid {}, id {}, packet {}, pos {}",
                    pid,
                    -3,
                    off / DVB_FRAME,
                    off
                );
                continue;
            }
            let pmt = pmt as usize;
            let cp = ((b[3] & 0x40) != 0) as i8;
            let Some(service) = self.services.get_mut(&pmt) else {
                continue;
            };
            service.last_scrambled_pkt = now;
            if service.parity == -1 {
                service.parity = cp;
            }
            if service.parity != cp {
                self.decrypt_batch(pmt, buf, now);
                let service = self.services.get_mut(&pmt).expect("present");
                service.parity = cp;
                service.update_cw = true;
            }
            let service = self.services.get_mut(&pmt).expect("present");
            if service.batch.is_empty() {
                order.push(pmt);
            }
            service.batch.push(off);
        }
        for &pmt in &order {
            self.decrypt_batch(pmt, buf, now);
        }
        self.order = order;
    }

    /// Handles packets still scrambled after decrypting (`mark_pids_null`).
    pub fn mark_pids_null(&mut self, buf: &mut [u8], drop: DropEncrypted, pids_all: bool) {
        let len = buf.len() / DVB_FRAME * DVB_FRAME;
        for (packet, b) in buf[..len]
            .as_chunks_mut::<DVB_FRAME>()
            .0
            .iter_mut()
            .enumerate()
        {
            let pid = pid_of(b);
            if pid == 0x1FFF || pids_all {
                continue;
            }
            if b[3] & 0x80 == 0 {
                if pid > 0x001F {
                    self.clear_pkts += 1;
                }
                // The first clear packet resets the pid's errors (pmt.cpp:1366-1372).
                let entry = &mut self.pids[usize::from(pid)];
                if pid > 0x001F && entry.tracked && !entry.decrypted {
                    entry.decrypted = true;
                    entry.dec_err = 0;
                }
                continue;
            }
            // `opts.debug & (DEFAULT_LOG | LOG_DMX)`.
            if ms_log::debug_enabled(DEFAULT_LOG | ms_log::LOG_DMX) {
                ms_log::log_unrecorded!(
                    "Marking PID {} packet {} pos {} as NULL",
                    pid,
                    packet,
                    packet * DVB_FRAME
                );
            }
            match drop {
                DropEncrypted::Drop if has_pcr(b) => mark_pcr_only(b),
                DropEncrypted::Drop => mark_pid_null(b),
                DropEncrypted::Stuff => mark_padding_header(b),
                DropEncrypted::Send => {}
            }
            self.dec_err += 1;
            let entry = &mut self.pids[usize::from(pid)];
            if entry.tracked {
                entry.dec_err += 1;
            }
        }
    }
}
