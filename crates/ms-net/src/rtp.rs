//! RTP packetizing of adapter buffers, ported from `process_packets_for_stream`.

/// Size of one TS packet (`DVB_FRAME`).
pub const DVB_FRAME: usize = 188;
/// Raw TS bytes carried by one UDP RTP packet (`MAX_UDP_PACKET_SIZE`).
pub const MAX_UDP_PACKET_SIZE: usize = 1316;
/// TS packets per RTP packet over UDP (`UDP_MAX_PACK`).
pub const UDP_MAX_PACK: usize = 7;
/// TS packets per RTP packet over RTSP TCP (`TCP_MAX_PACK`).
pub const TCP_MAX_PACK: usize = 42;
/// iovec slots per flush (`TCP_MAX_IOV`).
pub const TCP_MAX_IOV: usize = 1008;
/// RTP header length.
pub const RTP_HEADER_LEN: usize = 12;
/// RTSP interleaved frame prefix (`$`, channel, length).
pub const INTERLEAVED_LEN: usize = 4;
/// First RTP/AVP header word: V=2, P=0, X=0, CC=0, M=0, PT=33 (MP2T).
pub const RTP_FIRST_WORD: u16 = 0x8021;

/// How a stream frames its TS payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// RTP over UDP, 7 TS packets per RTP packet.
    RtpUdp,
    /// RTP inside RTSP interleaved frames on channel 0, 42 TS packets each.
    RtpTcp,
    /// Raw TS, no headers (HTTP).
    Raw,
}

impl Framing {
    /// TS packets per RTP header, or 0 for raw TS.
    pub fn max_pack(self) -> usize {
        match self {
            Self::RtpUdp => UDP_MAX_PACK,
            Self::RtpTcp => TCP_MAX_PACK,
            Self::Raw => 0,
        }
    }

    /// Header bytes in front of each RTP packet.
    pub fn header_len(self) -> usize {
        match self {
            Self::RtpUdp => RTP_HEADER_LEN,
            Self::RtpTcp => INTERLEAVED_LEN + RTP_HEADER_LEN,
            Self::Raw => 0,
        }
    }
}

/// 90 kHz RTP timestamp derived from a monotonic time in microseconds, as C++ does.
pub fn rtp_timestamp(mono_us: i64) -> u32 {
    let secs = mono_us / 1_000_000;
    let rem = mono_us % 1_000_000;
    (90_000i64.wrapping_mul(secs) + (9 * rem) / 100) as u32
}

/// Per-stream RTP state: sequence number and SSRC.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RtpState {
    /// Next sequence number.
    pub seq: u16,
    /// SSRC; C++ uses the stream's session id.
    pub ssrc: u32,
}

impl RtpState {
    /// Creates state with the given SSRC and initial sequence number.
    pub fn new(ssrc: u32, seq: u16) -> Self {
        Self { seq, ssrc }
    }

    /// Writes one header into `out` and advances the sequence (`enqueue_rtp_header`).
    pub fn write_header(
        &mut self,
        out: &mut Vec<u8>,
        framing: Framing,
        payload_len: usize,
        mono_us: i64,
    ) -> usize {
        let start = out.len();
        if framing == Framing::RtpTcp {
            out.extend_from_slice(&[0x24, 0]);
            out.extend_from_slice(&((payload_len + RTP_HEADER_LEN) as u16).to_be_bytes());
        }
        out.extend_from_slice(&RTP_FIRST_WORD.to_be_bytes());
        out.extend_from_slice(&self.seq.to_be_bytes());
        out.extend_from_slice(&rtp_timestamp(mono_us).to_be_bytes());
        out.extend_from_slice(&self.ssrc.to_be_bytes());
        self.seq = self.seq.wrapping_add(1);
        out.len() - start
    }
}

/// One iovec entry of a batch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Segment {
    /// Header bytes at `headers[start..start + len]`.
    Header { start: usize, len: usize },
    /// TS bytes at `buf[start..start + len]`.
    Data { start: usize, len: usize },
}

/// One `flush_stream` call: segments over the adapter buffer plus header bytes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Batch {
    /// Header storage referenced by [`Segment::Header`].
    pub headers: Vec<u8>,
    /// iovec entries in order.
    pub segments: Vec<Segment>,
}

impl Batch {
    /// Borrows the batch as byte chunks, skipping empty slots.
    pub fn chunks<'a>(&'a self, buf: &'a [u8]) -> Vec<&'a [u8]> {
        self.segments
            .iter()
            .map(|segment| self.segment_bytes(buf, *segment))
            .filter(|chunk| !chunk.is_empty())
            .collect()
    }

    /// Splits the batch into one byte vector per RTP packet (UDP datagrams).
    pub fn datagrams(&self, buf: &[u8]) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        for segment in &self.segments {
            let bytes = self.segment_bytes(buf, *segment);
            match segment {
                Segment::Header { .. } => out.push(bytes.to_vec()),
                Segment::Data { .. } => match out.last_mut() {
                    Some(last) => last.extend_from_slice(bytes),
                    None => out.push(bytes.to_vec()),
                },
            }
        }
        out.retain(|datagram| !datagram.is_empty());
        out
    }

    /// Total bytes in the batch.
    pub fn len(&self, buf: &[u8]) -> usize {
        self.chunks(buf).iter().map(|chunk| chunk.len()).sum()
    }

    /// Whether the batch carries no bytes.
    pub fn is_empty(&self, buf: &[u8]) -> bool {
        self.len(buf) == 0
    }

    fn segment_bytes<'a>(&'a self, buf: &'a [u8], segment: Segment) -> &'a [u8] {
        match segment {
            Segment::Header { start, len } => &self.headers[start..start + len],
            Segment::Data { start, len } => &buf[start..start + len],
        }
    }
}

/// Builds the `flush_stream` batches for one adapter buffer; `selected` decides per PID.
/// RTP framings always end with a header, even when no packet matched.
pub fn packetize<F: Fn(u16) -> bool>(
    rtp: &mut RtpState,
    framing: Framing,
    buf: &[u8],
    selected: F,
    mono_us: i64,
) -> Vec<Batch> {
    let max_pack = framing.max_pack();
    let mut batches = Vec::new();
    let mut batch = Batch::default();
    if max_pack > 0 {
        batch.segments.push(Segment::Header { start: 0, len: 0 });
    }
    let mut last_rtp_header = 0;
    let mut total_len = 0;

    for (index, packet) in buf.as_chunks::<DVB_FRAME>().0.iter().enumerate() {
        if packet[0] != 0x47 {
            continue;
        }
        let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
        if !selected(pid) {
            continue;
        }
        let mut rtp_added = false;
        if total_len > 0 && max_pack > 0 && (total_len / DVB_FRAME).is_multiple_of(max_pack) {
            enqueue_header(rtp, framing, &mut batch, last_rtp_header, mono_us);
            last_rtp_header = batch.segments.len();
            batch.segments.push(Segment::Header { start: 0, len: 0 });
            rtp_added = true;
        }
        if (rtp_added || max_pack == 0) && batch.segments.len() >= TCP_MAX_IOV {
            let mut flushed = std::mem::take(&mut batch);
            if max_pack > 0 {
                flushed.segments.pop();
                batch.segments.push(Segment::Header { start: 0, len: 0 });
            }
            batches.push(flushed);
            last_rtp_header = 0;
            total_len = 0;
        }

        total_len += DVB_FRAME;
        let start = index * DVB_FRAME;
        match batch.segments.last_mut() {
            Some(Segment::Data { start: run, len }) if *run + *len == start => *len += DVB_FRAME,
            _ => batch.segments.push(Segment::Data {
                start,
                len: DVB_FRAME,
            }),
        }
    }

    if max_pack > 0 {
        enqueue_header(rtp, framing, &mut batch, last_rtp_header, mono_us);
    }
    batches.push(batch);
    batches
}

fn enqueue_header(
    rtp: &mut RtpState,
    framing: Framing,
    batch: &mut Batch,
    slot: usize,
    mono_us: i64,
) {
    let payload: usize = batch.segments[slot + 1..]
        .iter()
        .map(|segment| match segment {
            Segment::Header { len, .. } | Segment::Data { len, .. } => *len,
        })
        .sum();
    let start = batch.headers.len();
    let len = rtp.write_header(&mut batch.headers, framing, payload, mono_us);
    batch.segments[slot] = Segment::Header { start, len };
}
