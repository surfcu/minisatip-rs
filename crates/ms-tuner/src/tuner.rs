//! `Tuner` trait and the parameter types shared by all backends.

use std::collections::BTreeSet;
use std::fmt;
use std::io;

/// Size of an MPEG-2 transport-stream packet in bytes.
pub const TS_PACKET_SIZE: usize = 188;
/// PID list entry meaning "all PIDs", matching the C++ sentinel.
pub const PID_ALL: u16 = 8192;

/// DVB delivery system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DeliverySystem {
    /// DVB-S.
    DvbS,
    /// DVB-S2.
    DvbS2,
    /// DVB-T.
    DvbT,
    /// DVB-T2.
    DvbT2,
    /// DVB-C.
    DvbC,
    /// DVB-C2.
    DvbC2,
    /// Undefined delivery system (`SYS_UNDEFINED`).
    #[default]
    Undefined,
    /// Any other `fe_delivery_system_t` value.
    Other(i32),
}

/// Satellite polarisation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Polarization {
    /// Horizontal.
    Horizontal,
    /// Vertical.
    Vertical,
    /// Circular left.
    Left,
    /// Circular right.
    Right,
    /// No polarisation specified.
    #[default]
    None,
}

/// Tuning parameters passed from the RTSP layer; backend-specific fields are optional.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct TuneParams {
    /// Frequency in kHz, like the C++ `transponder::freq`.
    ///
    /// SAT>IP sends MHz, so `freq=11361.75` parses to `11_361_750`.
    pub freq: i32,
    /// Polarisation.
    pub pol: Polarization,
    /// Delivery system (`msys`).
    pub msys: DeliverySystem,
    /// Symbol rate in sym/s, like the C++ `transponder::sr` (`sr * 1000`).
    pub sr: u32,
    /// Source or diSEqC input.
    pub src: u8,
    /// Whether `src` was present, like the C++ optional `diseqc`.
    pub src_specified: bool,
    /// DVB code rate (`fe_code_rate_t`).
    pub fec: u8,
    /// Whether `fec` was present, like the C++ optional field.
    pub fec_specified: bool,
    /// Modulation (`fe_modulation_t`).
    pub mtype: u32,
    /// Whether `mtype` was present, like the C++ optional field.
    pub mtype_specified: bool,
    /// Pilot symbols (DVB-S2).
    pub plts: Option<bool>,
    /// Roll-off (`fe_rolloff_t`).
    pub ro: Option<u8>,
    /// Bandwidth in Hz (DVB-T/C).
    pub bw: Option<u32>,
    /// PLP or ISI (DVB-T2/S2).
    pub plp: Option<i32>,
    /// PIDs requested together with the tune.
    pub pids: PidSet,
    /// Frontend number (`fe`).
    pub fe: Option<i32>,
    /// Transmission mode (`fe_transmit_mode_t`).
    pub tmode: Option<i32>,
    /// Guard interval (`fe_guard_interval_t`).
    pub gi: Option<i32>,
    /// T2 transponder id (`t2id`).
    pub t2id: Option<i32>,
    /// Stream or input-stream id (`sm`).
    pub sm: Option<i32>,
    /// DVB-C2 tuner frequency (`c2tft`).
    pub c2tft: Option<i32>,
    /// DVB-C2 data slice (`ds`).
    pub ds: Option<i32>,
    /// Spectral inversion (`fe_spectral_inversion_t`).
    pub specinv: Option<i32>,
    /// PLS mode (`fe_pls_mode_t`).
    pub plsm: Option<i32>,
    /// PLS code (`plsc`).
    pub plsc: Option<i32>,
    /// External PMT PID (`x_pmt`).
    pub x_pmt: Option<i32>,
}

/// Set of PIDs with the C++ `8192` sentinel meaning every PID.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PidSet {
    pids: BTreeSet<u16>,
}

impl PidSet {
    /// Creates an empty set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a set containing only the "all PIDs" sentinel.
    pub fn all() -> Self {
        let mut set = Self::new();
        set.insert(PID_ALL);
        set
    }

    /// Creates a set from an iterator of PIDs.
    pub fn from_pids<I: IntoIterator<Item = u16>>(pids: I) -> Self {
        Self {
            pids: pids.into_iter().collect(),
        }
    }

    /// Adds a PID; `PID_ALL` switches the set to "all".
    pub fn insert(&mut self, pid: u16) {
        self.pids.insert(pid);
    }

    /// Adds every PID from an iterator.
    pub fn extend<I: IntoIterator<Item = u16>>(&mut self, pids: I) {
        self.pids.extend(pids);
    }

    /// Removes a PID.
    pub fn remove(&mut self, pid: u16) {
        self.pids.remove(&pid);
    }

    /// Removes all entries.
    pub fn clear(&mut self) {
        self.pids.clear();
    }

    /// Whether `pid` is selected, or whether the set is in "all" mode.
    pub fn contains(&self, pid: u16) -> bool {
        self.pids.contains(&pid) || self.pids.contains(&PID_ALL)
    }

    /// Exact membership, without the `PID_ALL` sentinel widening the answer.
    pub fn contains_exact(&self, pid: u16) -> bool {
        self.pids.contains(&pid)
    }

    /// Whether the set contains the `PID_ALL` sentinel.
    pub fn is_all(&self) -> bool {
        self.pids.contains(&PID_ALL)
    }

    /// Number of stored entries, including the sentinel.
    pub fn len(&self) -> usize {
        self.pids.len()
    }

    /// Whether no PID is stored.
    pub fn is_empty(&self) -> bool {
        self.pids.is_empty()
    }

    /// Iterates over the stored entries.
    pub fn iter(&self) -> impl Iterator<Item = u16> + '_ {
        self.pids.iter().copied()
    }
}

/// Frontend lock state, with C++-style 0..255 strength and SNR values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SignalStatus {
    /// Whether the frontend reports a lock.
    pub locked: bool,
    /// Signal strength, 0..255.
    pub strength: u16,
    /// Signal-to-noise ratio, 0..255.
    pub snr: u16,
}

impl Default for SignalStatus {
    fn default() -> Self {
        Self {
            locked: true,
            strength: 255,
            snr: 255,
        }
    }
}

impl SignalStatus {
    /// Creates a locked status with explicit strength and SNR.
    pub fn locked(strength: u16, snr: u16) -> Self {
        Self {
            locked: true,
            strength,
            snr,
        }
    }
}

/// Errors returned by tuner backends.
#[derive(Debug)]
pub enum TunerError {
    /// The backing file or device failed.
    Io(io::Error),
    /// The backend rejected the request with a C++-style negative code.
    Backend(i32),
}

impl fmt::Display for TunerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(error) => write!(f, "tuner I/O error: {error}"),
            Self::Backend(code) => write!(f, "tuner backend error: {code}"),
        }
    }
}

impl std::error::Error for TunerError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(error) => Some(error),
            Self::Backend(_) => None,
        }
    }
}

impl From<io::Error> for TunerError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

/// Result alias used by the tuner API.
pub type Result<T> = std::result::Result<T, TunerError>;

/// Caller-owned buffer that only ever holds whole 188-byte packets.
#[derive(Debug, Clone, Default)]
pub struct TsBuf {
    data: Vec<u8>,
    capacity: usize,
}

impl TsBuf {
    /// Creates a buffer rounded down to a whole number of packets.
    pub fn new(capacity_bytes: usize) -> Self {
        let capacity = capacity_bytes / TS_PACKET_SIZE * TS_PACKET_SIZE;
        Self {
            data: Vec::with_capacity(capacity),
            capacity,
        }
    }

    /// Creates a buffer holding at most `packets` packets.
    pub fn with_packets(packets: usize) -> Self {
        Self::new(packets * TS_PACKET_SIZE)
    }

    /// Drops buffered packets.
    pub fn clear(&mut self) {
        self.data.clear();
    }

    /// Bytes currently buffered.
    pub fn len(&self) -> usize {
        self.data.len()
    }

    /// Whether no packets are buffered.
    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    /// Capacity in whole packets.
    pub fn capacity_packets(&self) -> usize {
        self.capacity / TS_PACKET_SIZE
    }

    /// Space left in bytes; always a multiple of 188.
    pub fn remaining_bytes(&self) -> usize {
        self.capacity - self.data.len()
    }

    /// Buffered bytes.
    pub fn as_bytes(&self) -> &[u8] {
        &self.data
    }

    /// Iterates over whole buffered packets.
    pub fn packets(&self) -> impl Iterator<Item = &[u8]> {
        self.data
            .as_chunks::<TS_PACKET_SIZE>()
            .0
            .iter()
            .map(|packet| packet.as_slice())
    }

    pub(crate) fn push_packet(&mut self, packet: &[u8]) -> bool {
        if packet.len() != TS_PACKET_SIZE || self.remaining_bytes() < TS_PACKET_SIZE {
            return false;
        }
        self.data.extend_from_slice(packet);
        true
    }
}

/// A tuning backend.
pub trait Tuner {
    /// Applies tuning parameters.
    fn tune(&mut self, params: &TuneParams) -> Result<()>;
    /// Replaces the PID filter.
    fn set_pids(&mut self, pids: &PidSet) -> Result<()>;
    /// Latest frontend status.
    fn status(&self) -> SignalStatus;
    /// Appends whole TS packets to `buf` and returns the bytes appended.
    fn read(&mut self, buf: &mut TsBuf) -> Result<usize>;
}
