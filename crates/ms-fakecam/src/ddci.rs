//! DDCI TS side: TS in on the CI, "descrambled" TS out after a configurable delay.

use crate::config::CamConfig;
use crate::programs::ProgramList;
use ms_ts::packet::{TS_PACKET_SIZE, TS_SYNC_BYTE};
use std::collections::VecDeque;

/// Cost of one TS packet against the bandwidth limit, in bits.
const PACKET_BITS: u64 = (TS_PACKET_SIZE * 8) as u64;

/// Counters of the TS path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TsStats {
    pub packets_in: u64,
    pub packets_out: u64,
    /// Dropped because the CI bandwidth limit was exceeded.
    pub dropped_bandwidth: u64,
    /// Scrambled packets whose scrambling bits were cleared.
    pub descrambled: u64,
    /// Scrambled packets passed on unchanged (not selected, over limit or still starting).
    pub still_scrambled: u64,
    /// Bytes skipped while looking for a sync byte.
    pub resync_bytes: u64,
}

/// Token bucket in milli-bits, refilled from the injected clock.
#[derive(Debug, Clone)]
struct Bucket {
    bps: u64,
    capacity: u64,
    tokens: u64,
    last_ms: u64,
}

impl Bucket {
    fn new(bps: u64, burst_ms: u64, now: u64) -> Self {
        let capacity = burst_ms.saturating_mul(bps).max(PACKET_BITS * 1000);
        Self {
            bps,
            capacity,
            tokens: capacity,
            last_ms: now,
        }
    }

    fn take(&mut self, now: u64) -> bool {
        let elapsed = now.saturating_sub(self.last_ms);
        self.last_ms = self.last_ms.max(now);
        let refill = elapsed.saturating_mul(self.bps);
        self.tokens = self.tokens.saturating_add(refill).min(self.capacity);
        let cost = PACKET_BITS * 1000;
        if self.tokens < cost {
            return false;
        }
        self.tokens -= cost;
        true
    }
}

/// TS interface of one fake CAM.
#[derive(Debug, Clone)]
pub struct DdciPort {
    partial: Vec<u8>,
    out: VecDeque<(u64, [u8; TS_PACKET_SIZE])>,
    bucket: Option<Bucket>,
    bandwidth: Option<(u64, u64)>,
    ts_latency_ms: u64,
    descramble_latency_ms: u64,
    stats: TsStats,
}

impl DdciPort {
    pub fn new(config: &CamConfig) -> Self {
        Self {
            partial: Vec::new(),
            out: VecDeque::new(),
            bucket: None,
            bandwidth: config
                .ci_bandwidth_bps
                .map(|bps| (bps, config.bandwidth_burst_ms)),
            ts_latency_ms: config.ts_latency_ms,
            descramble_latency_ms: config.descramble_latency_ms,
            stats: TsStats::default(),
        }
    }

    pub fn stats(&self) -> TsStats {
        self.stats
    }

    /// Packets accepted but not yet due at the output.
    pub fn pending(&self) -> usize {
        self.out.len()
    }

    /// Drops buffered input and output (CAM reset).
    pub fn reset(&mut self) {
        self.partial.clear();
        self.out.clear();
    }

    /// Feeds bytes written to the CI; packets may be split across calls.
    pub fn write(&mut self, data: &[u8], programs: &ProgramList, now: u64) {
        self.partial.extend_from_slice(data);
        let clear = programs.clear_pids(now, self.descramble_latency_ms);
        let mut pos = 0;
        while self.partial.len() - pos >= TS_PACKET_SIZE {
            if self.partial[pos] != TS_SYNC_BYTE {
                pos += 1;
                self.stats.resync_bytes += 1;
                continue;
            }
            let mut pkt = [0u8; TS_PACKET_SIZE];
            pkt.copy_from_slice(&self.partial[pos..pos + TS_PACKET_SIZE]);
            pos += TS_PACKET_SIZE;
            self.stats.packets_in += 1;
            if let Some((bps, burst)) = self.bandwidth {
                let bucket = self
                    .bucket
                    .get_or_insert_with(|| Bucket::new(bps, burst, now));
                if !bucket.take(now) {
                    self.stats.dropped_bandwidth += 1;
                    continue;
                }
            }
            if pkt[3] & 0xC0 != 0 {
                let pid = (u16::from(pkt[1] & 0x1F) << 8) | u16::from(pkt[2]);
                if clear.contains(&pid) {
                    pkt[3] &= 0x3F;
                    self.stats.descrambled += 1;
                } else {
                    self.stats.still_scrambled += 1;
                }
            }
            self.out
                .push_back((now.saturating_add(self.ts_latency_ms), pkt));
        }
        self.partial.drain(..pos);
    }

    /// Returns every packet due at `now`, in input order.
    pub fn read(&mut self, now: u64) -> Vec<u8> {
        let mut data = Vec::new();
        while let Some((due, pkt)) = self.out.front() {
            if *due > now {
                break;
            }
            data.extend_from_slice(pkt);
            self.out.pop_front();
            self.stats.packets_out += 1;
        }
        data
    }
}
