//! File-replay tuner: feeds a TS capture through the normal tuner interface.

use crate::tuner::{
    PidSet, Result, SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner, TunerError,
};
use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

/// Largest delay behind the fixed-bitrate schedule that is still caught up.
const MAX_PACING_LAG: Duration = Duration::from_millis(200);

/// Pacing applied while replaying a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pacing {
    /// Deliver as fast as the caller can read.
    #[default]
    None,
    /// Deliver at a fixed bitrate in bits per second.
    FixedBitrate(u64),
    /// Follow PCR timestamps from the stream.
    Pcr,
}

/// Configuration for [`FileReplayTuner`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileReplayConfig {
    /// Restart at the beginning after EOF.
    pub looping: bool,
    /// Output pacing.
    pub pacing: Pacing,
}

impl Default for FileReplayConfig {
    fn default() -> Self {
        Self {
            looping: false,
            pacing: Pacing::None,
        }
    }
}

/// Replays a TS file, recording tune parameters and filtering by PID.
pub struct FileReplayTuner {
    file: File,
    file_len: u64,
    config: FileReplayConfig,
    pids: PidSet,
    last_tune: Option<TuneParams>,
    status: SignalStatus,
    pacing_start: Option<Instant>,
    pacing_bytes: u64,
    last_pcr: Option<u64>,
    last_pcr_time: Option<Instant>,
}

impl FileReplayTuner {
    /// Opens `path` with the given replay configuration.
    pub fn open(path: impl AsRef<Path>, config: FileReplayConfig) -> Result<Self> {
        let file = File::open(path).map_err(TunerError::from)?;
        let file_len = file.metadata().map_err(TunerError::from)?.len();
        Ok(Self {
            file,
            file_len,
            config,
            pids: PidSet::new(),
            last_tune: None,
            status: SignalStatus::default(),
            pacing_start: None,
            pacing_bytes: 0,
            last_pcr: None,
            last_pcr_time: None,
        })
    }

    /// Current replay configuration.
    pub fn config(&self) -> FileReplayConfig {
        self.config
    }

    /// Replaces the replay configuration.
    pub fn set_config(&mut self, config: FileReplayConfig) {
        self.config = config;
    }

    /// Sets the reported frontend status.
    pub fn set_status(&mut self, status: SignalStatus) {
        self.status = status;
    }

    /// Reports a fixed lock with the given strength and SNR.
    pub fn set_signal(&mut self, strength: u16, snr: u16) {
        self.status = SignalStatus::locked(strength, snr);
    }

    /// Parameters recorded by the last `tune` call.
    pub fn last_tune(&self) -> Option<&TuneParams> {
        self.last_tune.as_ref()
    }

    /// Current PID filter.
    pub fn pids(&self) -> &PidSet {
        &self.pids
    }

    fn pace_bitrate(&mut self, bytes: usize) {
        let Pacing::FixedBitrate(bits_per_second) = self.config.pacing else {
            return;
        };
        if bytes == 0 || bits_per_second == 0 {
            return;
        }
        let mut start = *self.pacing_start.get_or_insert_with(Instant::now);
        let sent = Duration::from_secs_f64(self.pacing_bytes as f64 * 8.0 / bits_per_second as f64);
        // After an idle gap, restart the budget instead of bursting to catch up.
        if start.elapsed() > sent + MAX_PACING_LAG {
            start = Instant::now();
            self.pacing_start = Some(start);
            self.pacing_bytes = 0;
        }
        self.pacing_bytes = self.pacing_bytes.saturating_add(bytes as u64);
        let expected =
            Duration::from_secs_f64(self.pacing_bytes as f64 * 8.0 / bits_per_second as f64);
        if let Some(delay) = expected.checked_sub(start.elapsed()) {
            thread::sleep(delay);
        }
    }

    fn pace_pcr(&mut self, packet: &[u8; TS_PACKET_SIZE]) {
        let Some(pcr) = pcr_base(packet) else {
            return;
        };
        let now = Instant::now();
        if let (Some(previous), Some(previous_time)) = (self.last_pcr, self.last_pcr_time) {
            let ticks = pcr.wrapping_sub(previous);
            // Ignore wraparound and discontinuities larger than ten seconds.
            if ticks > 0 && ticks < 90_000 * 10 {
                let target = Duration::from_secs_f64(ticks as f64 / 90_000.0);
                if let Some(delay) = target.checked_sub(now.duration_since(previous_time)) {
                    thread::sleep(delay);
                }
            }
        }
        self.last_pcr = Some(pcr);
        self.last_pcr_time = Some(Instant::now());
    }
}

impl Tuner for FileReplayTuner {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        self.last_tune = Some(params.clone());
        Ok(())
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<()> {
        self.pids = pids.clone();
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        self.status
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        let total_packets = self.file_len / TS_PACKET_SIZE as u64;
        let mut written = 0;
        let mut scanned = 0u64;
        let mut packet = [0u8; TS_PACKET_SIZE];

        while buf.remaining_bytes() >= TS_PACKET_SIZE {
            match read_packet(&mut self.file, &mut packet)? {
                PacketRead::Full => {}
                PacketRead::Eof => {
                    if self.config.looping && scanned < total_packets {
                        self.file.seek(SeekFrom::Start(0))?;
                        continue;
                    }
                    break;
                }
            }
            scanned += 1;
            let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
            if self.pids.contains(pid) {
                if !buf.push_packet(&packet) {
                    break;
                }
                written += TS_PACKET_SIZE;
                if self.config.pacing == Pacing::Pcr {
                    self.pace_pcr(&packet);
                }
            }
        }

        self.pace_bitrate(written);
        Ok(written)
    }
}

enum PacketRead {
    Full,
    Eof,
}

fn read_packet(file: &mut File, packet: &mut [u8; TS_PACKET_SIZE]) -> Result<PacketRead> {
    let mut filled = 0;
    while filled < packet.len() {
        match file.read(&mut packet[filled..]) {
            Ok(0) => return Ok(PacketRead::Eof),
            Ok(count) => filled += count,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(TunerError::from(error)),
        }
    }
    Ok(PacketRead::Full)
}

fn pcr_base(packet: &[u8; TS_PACKET_SIZE]) -> Option<u64> {
    let adaptation = (packet[3] >> 4) & 0x3;
    if adaptation != 2 && adaptation != 3 {
        return None;
    }
    if packet[4] < 7 || packet[5] & 0x10 == 0 {
        return None;
    }
    Some(
        (u64::from(packet[6]) << 25)
            | (u64::from(packet[7]) << 17)
            | (u64::from(packet[8]) << 9)
            | (u64::from(packet[9]) << 1)
            | (u64::from(packet[10]) >> 7),
    )
}
