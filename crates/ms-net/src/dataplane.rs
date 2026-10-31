//! Per-adapter data plane: one thread reads a tuner, batches like `read_dmx` and
//! routes packets by PID to its outputs. It is driven only by messages.

use crate::bandwidth::Bandwidth;
use crate::describe::{DescribeInput, describe_adapter};
use crate::handler::{StreamId, StreamType};
use crate::output::{OutputStats, STREAM_TIMER_MS, StreamOutput};
use crate::rtp::DVB_FRAME;
use ms_sched::{AdapterTuner, TuneError};
use ms_ts::TsStage;
use ms_tuner::{PID_ALL, PidSet, SignalStatus, TsBuf, TuneParams, Tuner, TunerError};
use std::collections::BTreeMap;
use std::io;
use std::net::UdpSocket;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Rust-side thread ids for the C++ `tid` in `select_and_execute` messages.
/// `AtomicUsize` rather than `AtomicU64`, which 32-bit MIPS lacks.
static NEXT_THREAD_ID: AtomicUsize = AtomicUsize::new(0);

/// Adapter buffer size (`ADAPTER_BUFFER`, `-B` default).
pub const ADAPTER_BUFFER: usize = 2000 * DVB_FRAME;
/// Longest wait before a batch is forced out (the PCR interval).
pub const FORCE_SEND_MS: i64 = 40;
/// Time after a tune during which data is dropped until a new PAT appears
/// (`read_dmx`, stream.cpp:1223-1244).
pub const NEW_TRANSPONDER_WINDOW_MS: i64 = 200;
/// Minimum interval between continuity-error reports to the control plane.
const CC_REPORT_MS: i64 = 1000;
/// Signal change that triggers a `signal` event (5% of the 0..255 range).
const DIAG_SIGNAL_STEP: u16 = 13;
/// Minimum interval between `signal` events for one adapter.
const DIAG_SIGNAL_PERIOD_MS: i64 = 10_000;
/// DVB frontend status bit the tuner abstraction can report.
const DIAG_FE_HAS_LOCK: i64 = 0x10;
/// Minimum interval between `Event::Signal` reports for one adapter.
const SIGNAL_REPORT_MS: i64 = 1000;
/// Command poll interval while the tuner has no data.
const IDLE_POLL: Duration = Duration::from_millis(5);
/// Null-packet PID skipped by `check_cc` (stream.cpp:905).
const PID_NULL: u16 = 8191;
/// Upper bound the control plane waits for a tune result.
///
/// C++ tunes synchronously in the request path, so SETUP/PLAY cannot answer
/// before the frontend ioctls finish. The bound only guards against a wedged
/// adapter thread: expiry is reported as a hardware failure (`-500`).
pub const TUNE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);

/// Locks a shared counter; a `Mutex` because 32-bit MIPS lacks `AtomicU64`.
fn lock_counter(counter: &Mutex<u64>) -> MutexGuard<'_, u64> {
    counter.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Clamps a threshold to `1..200` ms, with 200 for out-of-range values.
pub fn clamp_threshold(threshold: i32) -> i64 {
    if threshold <= 0 || threshold >= 200 {
        200
    } else {
        i64::from(threshold)
    }
}

/// Port of `adapter_update_threshold`; `any_live_udp` covers every stream, not one adapter.
pub fn adapter_threshold(
    udp_threshold: i32,
    tcp_threshold: i32,
    sid_cnt: usize,
    master_type: Option<StreamType>,
    any_live_udp: bool,
) -> i64 {
    let mut threshold = udp_threshold;
    if sid_cnt == 1 && master_type.is_some() {
        if matches!(master_type, Some(StreamType::RtspTcp | StreamType::Http)) {
            threshold = tcp_threshold;
        }
    } else if sid_cnt > 0 && !any_live_udp {
        threshold = tcp_threshold;
    }
    clamp_threshold(threshold)
}

/// Data-plane settings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DataPlaneConfig {
    /// Adapter index used in descriptions (`aid`).
    pub adapter_id: i32,
    /// Adapter buffer in bytes (`opts.adapter_buffer`).
    pub adapter_buffer: usize,
    /// Initial send threshold in ms (`ad->threshold`).
    pub threshold_ms: i64,
    /// `-k`: a `pids=all` output never opens pid 8192 on the tuner.
    pub emulate_pids_all: bool,
}

impl Default for DataPlaneConfig {
    fn default() -> Self {
        Self {
            adapter_id: 0,
            adapter_buffer: ADAPTER_BUFFER,
            threshold_ms: clamp_threshold(25),
            emulate_pids_all: false,
        }
    }
}

/// Finds the start of the first PAT carrying a transponder id other than
/// `wait_transponder_id`, updating the remembered id (`check_new_transponder`,
/// stream.cpp:864-889). Returns the offset of that PAT in `data`, if any.
pub fn check_new_transponder(data: &[u8], wait_transponder_id: &mut i32) -> Option<usize> {
    check_new_transponder_elapsed(data, wait_transponder_id, 0)
}

/// [`check_new_transponder`] with the time since the tune for the C++ logs
/// (stream.cpp:877,883).
fn check_new_transponder_elapsed(
    data: &[u8],
    wait_transponder_id: &mut i32,
    elapsed_ms: i64,
) -> Option<usize> {
    for offset in (0..data.len()).step_by(DVB_FRAME) {
        let packet = &data[offset..];
        if packet.len() < 10 {
            break;
        }
        // PAT: payload-unit start, PID 0.
        if packet[0] == 0x47 && packet[1] == 0x40 && packet[2] == 0 {
            let tid = i32::from(packet[8]) * 256 + i32::from(packet[9]);
            if tid != *wait_transponder_id {
                *wait_transponder_id = tid;
                // stream.cpp:877 (`LOG`, general module).
                ms_log::log!(
                    "Got the new transponder {:04X} {}, position {}, {} ms after tune",
                    tid,
                    tid,
                    offset,
                    elapsed_ms
                );
                return Some(offset);
            // stream.cpp:883.
            } else {
                ms_log::logl!(
                    ms_log::LOG_DMX,
                    "Got old transponder id {:04X} {}, position {}, {} ms after tune",
                    tid,
                    tid,
                    offset,
                    elapsed_ms
                );
            }
        }
    }
    None
}

/// Applies the post-tune window to one read buffer (`read_dmx`,
/// stream.cpp:1223-1244).
///
/// Within [`NEW_TRANSPONDER_WINDOW_MS`] of the tune the buffer is kept only
/// from the first PAT carrying a new transponder id; `None` means no such PAT
/// was present and the whole read is dropped. After the window the buffer is
/// used unchanged (`Some(0)`).
pub fn post_tune_filter(
    data: &[u8],
    wait_transponder_id: &mut i32,
    elapsed_ms: i64,
) -> Option<usize> {
    if elapsed_ms >= NEW_TRANSPONDER_WINDOW_MS {
        return Some(0);
    }
    check_new_transponder_elapsed(data, wait_transponder_id, elapsed_ms)
}

/// Per-PID continuity counters (`check_cc`, stream.cpp:891-974).
///
/// Only the parts that feed the web UI are kept: counters per active PID and
/// the number of packets whose continuity counter was out of sequence. The
/// missing-PID accounting of C++ is not reproduced because the tuner only
/// delivers active PIDs (DEV-112).
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ContinuityCounter {
    last: BTreeMap<u16, i16>,
    packets: u64,
    errors: u64,
    /// Adapter index printed by the continuity-error log (`ad->id`).
    adapter_id: i32,
}

impl ContinuityCounter {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
    }

    /// Records the adapter index used in log lines (stream.cpp:937).
    pub fn with_adapter(mut self, adapter_id: i32) -> Self {
        self.adapter_id = adapter_id;
        self
    }

    /// Total packets checked for an active PID (`p->packets`).
    pub fn packets(&self) -> u64 {
        self.packets
    }

    /// Packets with an unexpected continuity counter (`p->cc_err`).
    pub fn errors(&self) -> u64 {
        self.errors
    }

    /// Checks one 188-byte packet; `true` when its counter was out of sequence.
    pub fn check_packet(&mut self, packet: &[u8]) -> bool {
        self.check_packet_at(packet, 0)
    }

    /// [`Self::check_packet`] with the packet position for the C++ log.
    fn check_packet_at(&mut self, packet: &[u8], position: i64) -> bool {
        if packet.len() < DVB_FRAME || packet[1] & 0x80 != 0 {
            return false;
        }
        let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
        if pid == PID_NULL {
            return false;
        }
        self.packets += 1;
        if packet[3] & 0x10 == 0 {
            return false;
        }
        let cc = i16::from(packet[3] & 0x0f);
        let cc_before = self.last.get(&pid).copied().unwrap_or(-1);
        let expected = if !(0..=15).contains(&cc_before) {
            cc
        } else {
            (cc_before + 1) % 16
        };
        let error = expected != cc;
        if error {
            self.errors += 1;
            ms_log::diag::count_cc(i64::from(self.adapter_id), pid);
            // stream.cpp:937 (`LOG`, general module).
            ms_log::log!(
                "PID Continuity error (adapter {}, pos {}): pid: {:03}, Expected CC: {:X}, Actual CC: {:X}, CC Before {:X} {}",
                self.adapter_id,
                position,
                pid,
                expected,
                cc,
                cc_before,
                if packet[3] & 0x80 != 0 {
                    "encrypted"
                } else {
                    ""
                }
            );
        }
        self.last.insert(pid, cc);
        error
    }

    /// Checks every complete packet in `data` whose PID is in `pids`.
    ///
    /// Like C++, checking is skipped entirely while `PID_ALL` is active.
    pub fn check_buffer(&mut self, data: &[u8], pids: &PidSet) {
        self.check_buffer_from(data, pids, 0);
    }

    /// [`Self::check_buffer`] for a slice that starts `offset` bytes into the
    /// adapter buffer, so `pos` and `_dump_packets` keep the C++ offsets.
    fn check_buffer_from(&mut self, data: &[u8], pids: &PidSet, offset: usize) {
        if pids.contains(PID_ALL) {
            return;
        }
        for (index, packet) in data.as_chunks::<DVB_FRAME>().0.iter().enumerate() {
            // stream.cpp:912: `_dump_packets("check_cc -> ", b, 188, i)`.
            if packet[1] & 0x80 != 0 {
                continue;
            }
            let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
            if pid == PID_NULL {
                continue;
            }
            if ms_log::debug_enabled(ms_log::LOG_DMX) {
                ms_log::dump_packets("check_cc -> ", packet, (offset + index * DVB_FRAME) as i64);
            }
            if pids.contains(pid) {
                self.check_packet_at(packet, ((offset + index * DVB_FRAME) / DVB_FRAME) as i64);
            }
        }
    }

    /// Drops state for PIDs no longer routed (`update_pids` resets a re-added PID).
    pub fn retain_pids(&mut self, pids: &PidSet) {
        if pids.contains(PID_ALL) {
            return;
        }
        self.last.retain(|pid, _| pids.contains(*pid));
    }

    /// Forgets every PID's counter.
    pub fn reset(&mut self) {
        self.last.clear();
    }
}

/// Messages from the control plane.
pub enum Command {
    /// Tunes the tuner; the parameters also feed the RTCP description.
    Tune {
        /// Tuning parameters.
        params: Box<TuneParams>,
        /// When set, the thread reports the backend result before continuing.
        reply: Option<Sender<Result<(), TunerError>>>,
    },
    /// Starts sending `pids` to a new output.
    StartOutput {
        /// Stream that owns the output.
        stream_id: StreamId,
        /// Generation reported back in output events.
        epoch: u64,
        /// The output.
        output: Box<StreamOutput>,
        /// PIDs routed to it.
        pids: PidSet,
        /// Milliseconds between SETUP (`streams_add`) and PLAY.
        setup_age_ms: i64,
    },
    /// Updates the RTP SSRC (`sid->ssrc = id`, stream.cpp:1409-1416).
    SetSsrc {
        /// Target stream.
        stream_id: StreamId,
        /// New SSRC.
        ssrc: u32,
    },
    /// Replaces an output's PID set.
    SetPids {
        /// Target stream.
        stream_id: StreamId,
        /// New PIDs.
        pids: PidSet,
    },
    /// Replaces the PID set owned by the scheduler (adapter table, defaults
    /// included). The tuner filters the union of this set and the outputs.
    SetBasePids(PidSet),
    /// Removes an output; its final stats are reported as [`Event::OutputStopped`].
    StopOutput(StreamId),
    /// Queues an RTSP reply on an interleaved output's priority lane.
    WritePriority {
        /// Target stream.
        stream_id: StreamId,
        /// Reply bytes.
        data: Vec<u8>,
    },
    /// Moves a UDP output onto the sockets of a new transport (re-SETUP).
    ReplaceUdpSockets {
        /// Target stream.
        stream_id: StreamId,
        /// New RTP socket, connected to the new client port.
        rtp: UdpSocket,
        /// New RTCP socket.
        rtcp: UdpSocket,
        /// `--output-buffer` for the new sockets.
        send_buffer: i32,
    },
    /// Replaces the send threshold (see [`adapter_threshold`]).
    SetThreshold(i64),
    /// PIDs with section filters; their packets are reported as [`Event::TablePackets`].
    SetSectionPids(PidSet),
    /// PIDs the tuner opens as PSI (`dvb_is_psi`), sent by `update_pids`
    /// ahead of [`Command::SetBasePids`].
    SetPsiPids(PidSet),
    /// Reads the tuner even without outputs (a DDCI adapter feeding its CAM FIFO).
    KeepReading(bool),
    /// Replies with an output's counters.
    Query {
        /// Target stream.
        stream_id: StreamId,
        /// Reply channel.
        reply: Sender<Option<OutputStats>>,
    },
    /// Ends the thread.
    Stop,
}

/// Notifications to the control plane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// An RTCP packet arrived from the client (`rtcp_confirm`).
    RtcpReceived(StreamId),
    /// A write failed; the output was removed (C++ closes the stream).
    OutputClosed(StreamId, u64),
    /// An output was removed by [`Command::StopOutput`].
    OutputStopped(StreamId, OutputStats, u64),
    /// Total continuity errors seen by this adapter (`ad->ccerr`).
    ContinuityErrors(u64),
    /// The tuner failed to tune or read.
    TunerError(String),
    /// The frontend reading after a tune (`ad->strength`, `snr`, `db`, `ber`,
    /// `status`); `None` when a new tune clears the previous reading.
    Signal(Option<SignalStatus>),
    /// Packets of a batch on section PIDs, before the stages ran (`process_filters`).
    TablePackets {
        /// Tunes handled before this batch; older batches belong to a previous mux.
        generation: u64,
        /// Whole TS packets in stream order.
        packets: Vec<u8>,
    },
}

/// Control-plane handle to one adapter's data-plane thread.
pub struct AdapterHandle {
    commands: Sender<Command>,
    events: Receiver<Event>,
    // A mutex rather than `AtomicU64`, which 32-bit MIPS does not provide.
    epoch: Mutex<u64>,
    tune_generation: Arc<Mutex<u64>>,
    thread: Option<JoinHandle<()>>,
}

impl AdapterHandle {
    /// Spawns the data-plane thread for `tuner`.
    pub fn spawn(tuner: Box<dyn Tuner + Send>, config: DataPlaneConfig) -> io::Result<Self> {
        Self::spawn_with_bandwidth(tuner, config, None)
    }

    /// Spawns the data-plane thread, feeding process-wide bandwidth counters.
    pub fn spawn_with_bandwidth(
        tuner: Box<dyn Tuner + Send>,
        config: DataPlaneConfig,
        bandwidth: Option<Arc<Bandwidth>>,
    ) -> io::Result<Self> {
        Self::spawn_with_stages(tuner, config, bandwidth, Vec::new())
    }

    /// Spawns the data-plane thread with in-place stages run on every batch
    /// before the outputs (`pmt_process_stream`: DDCI, decrypt, null marking).
    pub fn spawn_with_stages(
        tuner: Box<dyn Tuner + Send>,
        config: DataPlaneConfig,
        bandwidth: Option<Arc<Bandwidth>>,
        stages: Vec<Box<dyn TsStage>>,
    ) -> io::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let tune_generation = Arc::new(Mutex::new(0));
        let generation = Arc::clone(&tune_generation);
        let thread = thread::Builder::new()
            // adapter.cpp:341: the data-plane thread is `AD%d`.
            .name(format!("AD{}", config.adapter_id))
            .spawn(move || {
                let thread_name = format!("AD{}", config.adapter_id);
                ms_log::set_thread_name(&thread_name);
                let thread_id = NEXT_THREAD_ID.fetch_add(1, Ordering::Relaxed);
                // socketworks.cpp:769-770.
                ms_log::log!(
                    "Starting select_and_execute on thread ID {thread_id:x}, \
                     thread_name {thread_name}"
                );
                let mut plane = DataPlane::new(tuner, config, command_rx, event_tx, bandwidth);
                plane.stages = stages;
                plane.tune_generation = generation;
                plane.run()
            })?;
        Ok(Self {
            commands,
            events,
            epoch: Mutex::new(0),
            tune_generation,
            thread: Some(thread),
        })
    }

    /// Sends a raw command; `false` when the thread has exited.
    pub fn send(&self, command: Command) -> bool {
        self.commands.send(command).is_ok()
    }

    /// Clone of the command sender, e.g. for an [`ms_sched::AdapterTuner`] bridge.
    pub fn command_sender(&self) -> Sender<Command> {
        self.commands.clone()
    }

    /// Tunes the adapter without waiting for the backend result.
    pub fn tune(&self, params: TuneParams) -> bool {
        self.send(Command::Tune {
            params: Box::new(params),
            reply: None,
        })
    }

    /// Tunes the adapter and waits up to `timeout` for the backend result.
    ///
    /// The C++ request path tunes synchronously; the control plane uses this to
    /// turn a failing `FE_SET_PROPERTY` into the matching RTSP error. A
    /// stopped thread or timeout is reported as a hardware failure.
    pub fn tune_wait(&self, params: TuneParams, timeout: Duration) -> Result<(), TunerError> {
        let (reply, rx) = mpsc::channel();
        if !self.send(Command::Tune {
            params: Box::new(params),
            reply: Some(reply),
        }) {
            return Err(TunerError::Backend(-500));
        }
        rx.recv_timeout(timeout)
            .unwrap_or(Err(TunerError::Backend(-500)))
    }

    /// Starts an output for `stream_id`, returning its generation.
    pub fn start_output(
        &self,
        stream_id: StreamId,
        output: StreamOutput,
        pids: PidSet,
    ) -> Option<u64> {
        self.start_output_with_age(stream_id, output, pids, 0)
    }

    /// Starts an output whose SETUP happened `setup_age_ms` ago.
    pub fn start_output_with_age(
        &self,
        stream_id: StreamId,
        output: StreamOutput,
        pids: PidSet,
        setup_age_ms: i64,
    ) -> Option<u64> {
        let epoch = {
            let mut epoch = lock_counter(&self.epoch);
            *epoch = epoch.wrapping_add(1);
            *epoch
        };
        self.send(Command::StartOutput {
            stream_id,
            epoch,
            output: Box::new(output),
            pids,
            setup_age_ms,
        })
        .then_some(epoch)
    }

    /// Replaces the PIDs of `stream_id`.
    pub fn set_pids(&self, stream_id: StreamId, pids: PidSet) -> bool {
        self.send(Command::SetPids { stream_id, pids })
    }

    /// Replaces the scheduler-owned (adapter table) PID set.
    pub fn set_base_pids(&self, pids: PidSet) -> bool {
        self.send(Command::SetBasePids(pids))
    }

    /// Stops the output of `stream_id`.
    pub fn stop_output(&self, stream_id: StreamId) -> bool {
        self.send(Command::StopOutput(stream_id))
    }

    /// Queries counters; blocks until the thread replies.
    pub fn query(&self, stream_id: StreamId) -> Option<OutputStats> {
        let (reply, rx) = mpsc::channel();
        if !self.send(Command::Query { stream_id, reply }) {
            return None;
        }
        rx.recv().ok().flatten()
    }

    /// Tunes the thread has handled; it is raised before the tune is answered.
    pub fn tune_generation(&self) -> u64 {
        *lock_counter(&self.tune_generation)
    }

    /// Event receiver.
    pub fn events(&self) -> &Receiver<Event> {
        &self.events
    }

    /// Stops the thread and waits for it.
    pub fn stop(mut self) {
        self.shutdown();
    }

    fn shutdown(&mut self) {
        let _ = self.commands.send(Command::Stop);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

impl Drop for AdapterHandle {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// [`AdapterTuner`] that forwards to an adapter's data-plane thread.
///
/// Tuning blocks until the thread reports the backend result, matching the
/// synchronous C++ request path; the other commands stay asynchronous.
pub struct DataPlaneTuner {
    commands: Sender<Command>,
    timeout: Duration,
}

impl DataPlaneTuner {
    /// Creates a bridge that waits up to [`TUNE_REPLY_TIMEOUT`].
    pub fn new(commands: Sender<Command>) -> Self {
        Self {
            commands,
            timeout: TUNE_REPLY_TIMEOUT,
        }
    }

    /// Creates a bridge with an explicit reply timeout.
    pub fn with_timeout(commands: Sender<Command>, timeout: Duration) -> Self {
        Self { commands, timeout }
    }
}

impl AdapterTuner for DataPlaneTuner {
    fn tune(&mut self, params: &TuneParams) -> Result<(), TuneError> {
        let (reply, rx) = mpsc::channel();
        if self
            .commands
            .send(Command::Tune {
                params: Box::new(params.clone()),
                reply: Some(reply),
            })
            .is_err()
        {
            return Err(TuneError::Hardware);
        }
        match rx.recv_timeout(self.timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(TunerError::Backend(-403))) => Err(TuneError::OutOfRange),
            Ok(Err(TunerError::Backend(-503))) => Err(TuneError::Pids),
            Ok(Err(_)) | Err(_) => Err(TuneError::Hardware),
        }
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<(), TuneError> {
        self.commands
            .send(Command::SetBasePids(pids.clone()))
            .map_err(|_| TuneError::Pids)
    }

    fn set_section_pids(&mut self, pids: &PidSet) {
        let _ = self.commands.send(Command::SetPsiPids(pids.clone()));
    }
}

struct Subscriber {
    epoch: u64,
    output: StreamOutput,
    pids: PidSet,
    last_timer_ms: i64,
}

struct DataPlane {
    tuner: Box<dyn Tuner + Send>,
    config: DataPlaneConfig,
    commands: Receiver<Command>,
    events: Sender<Event>,
    outputs: BTreeMap<StreamId, Subscriber>,
    base_pids: PidSet,
    tune: TuneParams,
    buf: TsBuf,
    /// Bytes of `buf` already fed to the continuity checker.
    cc_checked: usize,
    active_pids: PidSet,
    continuity: ContinuityCounter,
    reported_cc_errors: u64,
    last_cc_report_ms: i64,
    wait_new_stream: bool,
    /// Start of the 200 ms new-transponder window; `None` when it is not
    /// running yet.
    tune_time_ms: Option<i64>,
    wait_transponder_id: i32,
    threshold_ms: i64,
    last_send_ms: i64,
    /// Consecutive full-buffer reads (`read_dmx`'s static `cnt`).
    full_reads: i32,
    origin: Instant,
    running: bool,
    bandwidth: Option<Arc<Bandwidth>>,
    /// Last octets/packets/failed writes/FIFO-copied/FIFO-dropped per output.
    bandwidth_marks: BTreeMap<StreamId, (u64, u32, u32, u64, u64)>,
    stages: Vec<Box<dyn TsStage>>,
    section_pids: PidSet,
    tune_generation: Arc<Mutex<u64>>,
    keep_reading: bool,
    /// Whether a tune was applied; lock events start after it.
    diag_tuned: bool,
    /// A tuner read has succeeded since the last tune, so `status()` is a real reading.
    signal_read: bool,
    /// Last reading sent as `Event::Signal`; `None` after a tune.
    reported_signal: Option<SignalStatus>,
    /// When the last `Event::Signal` was sent.
    last_signal_ms: Option<i64>,
    /// Last reported lock state (`None` before the first poll after a tune).
    diag_lock: Option<bool>,
    /// Last reported signal values.
    diag_signal: Option<(u16, u16, u32)>,
    /// Time of the last `signal` event.
    diag_signal_ms: i64,
}

impl DataPlane {
    fn new(
        tuner: Box<dyn Tuner + Send>,
        config: DataPlaneConfig,
        commands: Receiver<Command>,
        events: Sender<Event>,
        bandwidth: Option<Arc<Bandwidth>>,
    ) -> Self {
        Self {
            tuner,
            config,
            commands,
            events,
            outputs: BTreeMap::new(),
            base_pids: PidSet::new(),
            tune: TuneParams::default(),
            buf: TsBuf::new(config.adapter_buffer),
            cc_checked: 0,
            active_pids: PidSet::new(),
            continuity: ContinuityCounter::new().with_adapter(config.adapter_id),
            reported_cc_errors: 0,
            last_cc_report_ms: 0,
            wait_new_stream: false,
            tune_time_ms: None,
            wait_transponder_id: 0,
            threshold_ms: config.threshold_ms,
            last_send_ms: 0,
            full_reads: 0,
            origin: Instant::now(),
            running: true,
            bandwidth,
            bandwidth_marks: BTreeMap::new(),
            stages: Vec::new(),
            section_pids: PidSet::new(),
            tune_generation: Arc::new(Mutex::new(0)),
            keep_reading: false,
            diag_tuned: false,
            diag_lock: None,
            diag_signal: None,
            diag_signal_ms: 0,
            signal_read: false,
            reported_signal: None,
            last_signal_ms: None,
        }
    }

    /// Sends `Event::Signal` for a reading that changed. Untuned adapters and
    /// readings not yet taken after a tune stay silent (the default status
    /// is not a measurement), and reports are at most one per
    /// [`SIGNAL_REPORT_MS`]. A pending change is sent by a later poll.
    fn report_signal(&mut self, signal: SignalStatus, now: i64) {
        if !self.diag_tuned || !self.signal_read || self.reported_signal == Some(signal) {
            return;
        }
        if self
            .last_signal_ms
            .is_some_and(|last| now - last < SIGNAL_REPORT_MS)
        {
            return;
        }
        self.reported_signal = Some(signal);
        self.last_signal_ms = Some(now);
        let _ = self.events.send(Event::Signal(Some(signal)));
    }

    /// Emits `lock` on a change and `signal` beyond the threshold, at most
    /// once per adapter per 10 s.
    fn diag_status(&mut self, signal: &SignalStatus, now: i64) {
        let adapter = i64::from(self.config.adapter_id);
        if self.diag_tuned && self.diag_lock != Some(signal.locked) {
            self.diag_lock = Some(signal.locked);
            // Only the lock bit is known to the tuner abstraction.
            let status = if signal.locked { DIAG_FE_HAS_LOCK } else { 0 };
            ms_log::diag::events::lock(adapter, signal.locked, status);
        }
        let current = (signal.strength, signal.snr, signal.ber);
        let Some(previous) = self.diag_signal else {
            self.diag_signal = Some(current);
            return;
        };
        let changed = previous.0.abs_diff(current.0) >= DIAG_SIGNAL_STEP
            || previous.1.abs_diff(current.1) >= DIAG_SIGNAL_STEP
            || previous.2 != current.2;
        if changed && now - self.diag_signal_ms >= DIAG_SIGNAL_PERIOD_MS {
            self.diag_signal = Some(current);
            self.diag_signal_ms = now;
            ms_log::diag::events::signal(adapter, current.0, current.1, current.2);
        }
    }

    fn mono_us(&self) -> i64 {
        self.origin.elapsed().as_micros() as i64
    }

    fn run(mut self) {
        while self.running {
            while let Ok(command) = self.commands.try_recv() {
                self.handle(command);
            }
            if !self.running {
                break;
            }
            let got_data = (self.keep_reading || !self.outputs.is_empty()) && self.read_once();
            self.timers();
            self.flush_queues();
            self.sync_bandwidth();
            if !got_data {
                let wait = if self.outputs.is_empty() && !self.keep_reading {
                    Duration::from_millis(STREAM_TIMER_MS as u64)
                } else {
                    IDLE_POLL
                };
                match self.commands.recv_timeout(wait) {
                    Ok(command) => self.handle(command),
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => self.running = false,
                }
            }
        }
    }

    fn handle(&mut self, command: Command) {
        match command {
            Command::Tune { params, reply } => {
                {
                    let mut generation = lock_counter(&self.tune_generation);
                    *generation = generation.wrapping_add(1);
                }
                let result = self.tuner.tune(&params);
                match reply {
                    Some(reply) => {
                        let _ = reply.send(result);
                    }
                    None => {
                        if let Err(error) = result {
                            let _ = self.events.send(Event::TunerError(error.to_string()));
                        }
                    }
                }
                self.tune = *params;
                // adapter.cpp:1074-1135: a tune restarts the new-transponder wait.
                self.buf.clear();
                self.cc_checked = 0;
                self.wait_new_stream = true;
                self.tune_time_ms = Some(self.mono_us() / 1000);
                // The first poll after a tune reports the lock state.
                self.diag_tuned = true;
                self.diag_lock = None;
                // adapter.cpp:1092-1097: a tune resets status, strength, snr and db
                // until the next reading; the session drops its stored reading too.
                self.signal_read = false;
                self.reported_signal = None;
                self.last_signal_ms = None;
                let _ = self.events.send(Event::Signal(None));
                self.update_tuner_pids();
            }
            Command::StartOutput {
                stream_id,
                epoch,
                mut output,
                pids,
                setup_age_ms,
            } => {
                let now = self.mono_us() / 1000;
                // streams_add set `wtime`/`rtcp_wtime` at SETUP
                // (stream.cpp:630-636); the output starts at PLAY.
                output.set_clocks(now.saturating_sub(setup_age_ms));
                // Log lines use `sid->sid` (stream.cpp:785,838,845,...).
                output.set_stream_id(stream_id);
                self.outputs.insert(
                    stream_id,
                    Subscriber {
                        epoch,
                        output: *output,
                        pids,
                        last_timer_ms: now,
                    },
                );
                self.update_tuner_pids();
            }
            Command::SetSsrc { stream_id, ssrc } => {
                if let Some(subscriber) = self.outputs.get_mut(&stream_id) {
                    subscriber.output.set_ssrc(ssrc);
                }
            }
            Command::SetPids { stream_id, pids } => {
                if let Some(subscriber) = self.outputs.get_mut(&stream_id) {
                    subscriber.pids = pids;
                    self.update_tuner_pids();
                }
            }
            Command::SetBasePids(pids) => {
                self.base_pids = pids;
                self.update_tuner_pids();
            }
            Command::StopOutput(stream_id) => {
                if let Some(subscriber) = self.outputs.remove(&stream_id) {
                    let mut output = subscriber.output;
                    // stream.cpp:376,385: the UDP sockets close_stream forces
                    // shut. (StopOutput also runs for a transport change.)
                    output.log_closing_sockets();
                    // close_stream drops queued bytes up to the next RTSP
                    // header before the pending reply (stream.cpp:374).
                    output.set_flush_enqueued_data();
                    let _ = output.flush_queue();
                    let stats = output.stats();
                    let _ =
                        self.events
                            .send(Event::OutputStopped(stream_id, stats, subscriber.epoch));
                    self.update_tuner_pids();
                    self.standby_if_idle();
                }
            }
            Command::WritePriority { stream_id, data } => {
                let failed = self
                    .outputs
                    .get_mut(&stream_id)
                    .is_some_and(|subscriber| subscriber.output.write_priority(&data).is_err());
                if failed {
                    self.close(stream_id);
                }
            }
            Command::ReplaceUdpSockets {
                stream_id,
                rtp,
                rtcp,
                send_buffer,
            } => {
                if let Some(subscriber) = self.outputs.get_mut(&stream_id)
                    && let Err(error) =
                        subscriber
                            .output
                            .replace_udp_sockets(rtp, rtcp, send_buffer)
                {
                    ms_log::log!(
                        "cannot change the transport of sid {}: {}",
                        stream_id,
                        error
                    );
                }
            }
            Command::SetThreshold(threshold) => self.threshold_ms = threshold,
            Command::SetSectionPids(pids) => self.section_pids = pids,
            Command::SetPsiPids(pids) => self.tuner.set_section_pids(&pids),
            Command::KeepReading(on) => self.keep_reading = on,
            Command::Query { stream_id, reply } => {
                let stats = self
                    .outputs
                    .get(&stream_id)
                    .map(|subscriber| subscriber.output.stats());
                let _ = reply.send(stats);
            }
            Command::Stop => self.running = false,
        }
    }

    /// Folds output counters into the process-wide bandwidth window and asks it
    /// to update the snapshot (`calculate_bw`, stream.cpp:1287-1327).
    fn sync_bandwidth(&mut self) {
        let Some(bandwidth) = self.bandwidth.clone() else {
            return;
        };
        for (stream_id, subscriber) in &self.outputs {
            let stats = subscriber.output.stats();
            let mark = self
                .bandwidth_marks
                .entry(*stream_id)
                .or_insert((0, 0, 0, 0, 0));
            // `bw`/`writes` advance with `sid->sb`/`sid->sp`
            // (stream.cpp:825-826, socketworks.cpp:1321).
            bandwidth.record_writes(
                u64::from(stats.octets).wrapping_sub(mark.0),
                stats.packets.wrapping_sub(mark.1),
            );
            bandwidth.record_failed_writes(stats.failed_writes.wrapping_sub(mark.2));
            // `buffered_bytes += copied` when data enters the FIFO and
            // `dropped_bytes += len - offset` when it does not fit; both are
            // per-window accumulators reset by `calculate_bw`
            // (socketworks.cpp:1496,1505-1506; stream.cpp:1308-1309,1325-1326).
            bandwidth.record_buffered(stats.fifo_written.wrapping_sub(mark.3));
            bandwidth.record_dropped(stats.fifo_dropped.wrapping_sub(mark.4));
            *mark = (
                u64::from(stats.octets),
                stats.packets,
                stats.failed_writes,
                stats.fifo_written,
                stats.fifo_dropped,
            );
        }
        self.bandwidth_marks
            .retain(|stream_id, _| self.outputs.contains_key(stream_id));
        bandwidth.calculate();
    }

    fn update_tuner_pids(&mut self) {
        // The scheduler owns the adapter table (defaults included); outputs add
        // their own PIDs so a START between two scheduler updates keeps them.
        let mut pids = self.base_pids.clone();
        for subscriber in self.outputs.values() {
            pids.extend(subscriber.pids.iter());
        }
        // adapter.cpp:997: the scheduler opens the emulated pids instead; the
        // data plane keeps PID_ALL, so check_cc stays off as in stream.cpp:899.
        let mut tuner_pids = pids.clone();
        if self.config.emulate_pids_all {
            tuner_pids.remove(PID_ALL);
        }
        if let Err(error) = self.tuner.set_pids(&tuner_pids) {
            let _ = self.events.send(Event::TunerError(error.to_string()));
            return;
        }
        self.continuity.retain_pids(&pids);
        self.active_pids = pids;
    }

    fn read_once(&mut self) -> bool {
        let started = Instant::now();
        // The satipc driver raises this when PLAY follows a tune
        // (src_satipc.cpp:211-216); read_dmx's new-stream flush follows.
        if self.tuner.take_wait_new_stream() {
            self.wait_new_stream = true;
        }
        let read = match self.tuner.read(&mut self.buf) {
            Ok(read) => {
                // The tuner refreshes its cached status inside `read`.
                self.signal_read = true;
                read
            }
            Err(error) => {
                let _ = self.events.send(Event::TunerError(error.to_string()));
                return false;
            }
        };
        // stream.cpp:1172,1193-1194: `bw_dmx += rlen`, `nsecs += ...`, `reads++`.
        if let Some(bandwidth) = &self.bandwidth {
            bandwidth.record_read(read, started.elapsed().as_micros() as i64);
        }
        let now = self.mono_us() / 1000;
        if read == 0 {
            // A replay at EOF never reads again, so a pending batch leaves after 40 ms.
            if !self.wait_new_stream
                && !self.buf.is_empty()
                && now - self.last_send_ms > FORCE_SEND_MS
            {
                self.process(now);
            }
            return false;
        }
        // stream.cpp:1216-1219: `cnt` counts consecutive full-buffer reads.
        if read == self.buf.capacity_packets() * DVB_FRAME {
            self.full_reads += 1;
        } else {
            self.full_reads = 0;
        }
        // read_dmx's new-stream flush (src_stream.cpp:1241-1259).
        if !self.after_read(now) {
            return true;
        }
        let since = now - self.last_send_ms;
        let send = since > self.threshold_ms;
        let force = since > FORCE_SEND_MS
            || since > 4 * self.threshold_ms
            || self.buf.remaining_bytes() <= 7 * DVB_FRAME;
        // stream.cpp:1269 (`LOGM` under `#define DEFAULT_LOG LOG_DMX`).
        ms_log::logl!(
            ms_log::LOG_DMX,
            "read_dmx send={}, force_send={}, cnt {} called for adapter {} -> {} out of {} bytes read, {} ms ago ({} {})",
            u8::from(send),
            u8::from(force),
            self.full_reads,
            self.config.adapter_id,
            read,
            self.buf.capacity_packets() * DVB_FRAME,
            since,
            now,
            self.last_send_ms
        );
        if send || force {
            self.process(now);
        }
        true
    }

    /// Applies the post-tune filter and continuity check to freshly read data
    /// (`read_dmx`, stream.cpp:1205-1245). Returns `false` when the whole read
    /// was dropped while waiting for the new transponder.
    fn after_read(&mut self, now_ms: i64) -> bool {
        if self.wait_new_stream {
            // The window starts at the tune, or at the first read when the
            // tuner raised the hook (src_stream.cpp:1241-1259).
            let tune_time = *self.tune_time_ms.get_or_insert(now_ms);
            let elapsed = now_ms - tune_time;
            match post_tune_filter(self.buf.as_bytes(), &mut self.wait_transponder_id, elapsed) {
                Some(offset) => {
                    self.buf.discard_front(offset);
                    self.cc_checked = self.cc_checked.saturating_sub(offset);
                }
                None => {
                    // stream.cpp:1249 (`LOGM` under `#define DEFAULT_LOG LOG_DMX`).
                    ms_log::logl!(
                        ms_log::LOG_DMX,
                        "Flushing adapter {} buffer of {} bytes after the tune {} ms ago",
                        self.config.adapter_id,
                        self.buf.len(),
                        elapsed
                    );
                    self.buf.clear();
                    self.cc_checked = 0;
                    return false;
                }
            }
            self.tune_time_ms = None;
            self.wait_new_stream = false;
        }
        let checked = self.cc_checked.min(self.buf.len()) / DVB_FRAME * DVB_FRAME;
        self.continuity
            .check_buffer(&self.buf.as_bytes()[checked..], &self.active_pids);
        self.cc_checked = self.buf.len();
        true
    }

    fn process(&mut self, now_ms: i64) {
        // stream.cpp:1167-1177: `ms_ago` is relative to the previous
        // process_dmx, which `last_send_ms` tracks.
        let ms_ago = now_ms - self.last_send_ms;
        ms_log::logl!(
            ms_log::LOG_DMX,
            "process_dmx start called for adapter {} -> {} out of {} bytes read, {} ms ago",
            self.config.adapter_id,
            self.buf.len(),
            self.buf.capacity_packets() * DVB_FRAME,
            ms_ago
        );
        self.last_send_ms = now_ms;
        self.tap_sections();
        for stage in &mut self.stages {
            self.buf
                .with_vec(|buf, capacity| stage.process_buf(buf, capacity));
        }
        let mono = self.mono_us();
        let mut closed = Vec::new();
        for (stream_id, subscriber) in &mut self.outputs {
            let result =
                subscriber
                    .output
                    .process(self.buf.as_bytes(), &subscriber.pids, now_ms, mono);
            if result.is_err() {
                closed.push(*stream_id);
            }
        }
        self.buf.clear();
        self.cc_checked = 0;
        for stream_id in closed {
            self.close(stream_id);
        }
    }

    /// Copies packets on filtered PIDs for the control plane's section filters.
    fn tap_sections(&mut self) {
        if self.section_pids.is_empty() {
            return;
        }
        let mut tapped = Vec::new();
        for packet in self.buf.packets() {
            let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
            if packet[0] == 0x47 && self.section_pids.contains_exact(pid) {
                tapped.extend_from_slice(packet);
            }
        }
        if !tapped.is_empty() {
            let generation = *lock_counter(&self.tune_generation);
            let _ = self.events.send(Event::TablePackets {
                generation,
                packets: tapped,
            });
        }
    }

    fn timers(&mut self) {
        let mono = self.mono_us();
        let now = mono / 1000;
        let wall = SystemTime::now();
        let signal = self.tuner.status();
        self.diag_status(&signal, now);
        self.report_signal(signal, now);
        let mut closed = Vec::new();
        for (stream_id, subscriber) in &mut self.outputs {
            // stream.cpp:1337-1340: a failed send (`sid->timeout = 1`) closes now.
            if subscriber.output.forced_close() {
                closed.push(*stream_id);
                continue;
            }
            if now - subscriber.last_timer_ms <= STREAM_TIMER_MS {
                continue;
            }
            subscriber.last_timer_ms = now;
            if subscriber.output.poll_rtcp() {
                // stream.cpp:1455,1459: `rtcp_confirm` logs once per callback.
                match subscriber.output.remote_addr() {
                    Some(addr) => ms_log::log!(
                        "rtcp_confirm: called for stream sid {} from {}:{}",
                        stream_id,
                        addr.ip(),
                        u32::from(addr.port()) + 1
                    ),
                    None => {
                        ms_log::log!("rtcp_confirm: called for stream sid {} from :1", stream_id)
                    }
                }
                ms_log::logl!(
                    ms_log::LOG_STREAM,
                    "Acknowledging stream sid {} via rtcp packet",
                    stream_id
                );
                let _ = self.events.send(Event::RtcpReceived(*stream_id));
            }
            let input = DescribeInput {
                adapter_id: self.config.adapter_id,
                tune: &self.tune,
                signal: Some(signal),
                use_adapter: true,
                pids: &subscriber.pids,
            };
            let result = subscriber.output.on_timer(now, mono, wall, || {
                let description = describe_adapter(&input);
                ms_log::logl!(
                    ms_log::LOG_ADAPTER,
                    "describe_adapter: sid {} aid {} => {}",
                    *stream_id,
                    input.adapter_id,
                    description
                );
                description
            });
            if result.is_err() {
                closed.push(*stream_id);
            }
        }
        for stream_id in closed {
            self.close(stream_id);
        }
        self.report_continuity(now);
    }

    /// Publishes the continuity error count at most once per second.
    fn report_continuity(&mut self, now_ms: i64) {
        let errors = self.continuity.errors();
        if errors == self.reported_cc_errors {
            return;
        }
        if self.last_cc_report_ms != 0 && now_ms - self.last_cc_report_ms < CC_REPORT_MS {
            return;
        }
        self.last_cc_report_ms = now_ms;
        self.reported_cc_errors = errors;
        let _ = self.events.send(Event::ContinuityErrors(errors));
    }

    fn flush_queues(&mut self) {
        let mut closed = Vec::new();
        for (stream_id, subscriber) in &mut self.outputs {
            if subscriber.output.has_pending() && subscriber.output.flush_queue().is_err() {
                closed.push(*stream_id);
            }
        }
        for stream_id in closed {
            self.close(stream_id);
        }
    }

    fn close(&mut self, stream_id: StreamId) {
        if let Some(subscriber) = self.outputs.remove(&stream_id) {
            // The output-initiated closures below all lead to `close_stream`
            // (stream.cpp:376,385).
            subscriber.output.log_closing_sockets();
            let _ = self
                .events
                .send(Event::OutputClosed(stream_id, subscriber.epoch));
            self.update_tuner_pids();
            self.standby_if_idle();
        }
    }

    fn standby_if_idle(&mut self) {
        if self.outputs.is_empty() {
            self.tuner.standby();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ms_tuner::Result as TunerResult;

    struct LockTuner;

    impl Tuner for LockTuner {
        fn tune(&mut self, _params: &TuneParams) -> TunerResult<()> {
            Ok(())
        }

        fn set_pids(&mut self, _pids: &PidSet) -> TunerResult<()> {
            Ok(())
        }

        fn status(&self) -> SignalStatus {
            SignalStatus::default()
        }

        fn read(&mut self, _buf: &mut TsBuf) -> TunerResult<usize> {
            Ok(0)
        }
    }

    fn data_plane() -> DataPlane {
        let (_commands_tx, commands) = mpsc::channel::<Command>();
        let (events, _events_rx) = mpsc::channel::<Event>();
        DataPlane::new(
            Box::new(LockTuner),
            DataPlaneConfig::default(),
            commands,
            events,
            None,
        )
    }

    #[test]
    fn lock_events_start_with_the_first_poll_after_a_tune() {
        let mut plane = data_plane();
        let unlocked = SignalStatus {
            locked: false,
            strength: 0,
            snr: 0,
            ber: 0,
            db: SignalStatus::DB_UNKNOWN,
            status_valid: true,
        };
        // No tune yet: the poll is not reported.
        plane.diag_status(&unlocked, 0);
        plane.diag_status(&unlocked, 10);
        assert_eq!(plane.diag_lock, None, "untuned adapters stay quiet");

        // The first poll after a tune reports the state, even unlocked.
        plane.handle(Command::Tune {
            params: Box::new(TuneParams::default()),
            reply: None,
        });
        plane.diag_status(&unlocked, 20);
        assert_eq!(plane.diag_lock, Some(false));

        // Then only a change emits.
        let locked = SignalStatus {
            locked: true,
            ..unlocked
        };
        plane.diag_status(&locked, 30);
        assert_eq!(plane.diag_lock, Some(true));
        plane.diag_status(&locked, 40);
        assert_eq!(plane.diag_lock, Some(true));
    }

    fn signal_plane() -> (DataPlane, mpsc::Receiver<Event>) {
        let (_commands_tx, commands) = mpsc::channel::<Command>();
        let (events, events_rx) = mpsc::channel::<Event>();
        let plane = DataPlane::new(
            Box::new(LockTuner),
            DataPlaneConfig::default(),
            commands,
            events,
            None,
        );
        (plane, events_rx)
    }

    // ad->strength etc. are reported only from a reading taken after a tune, at
    // most once per second, and a tune clears the previous reading.
    #[test]
    fn signal_events_follow_tunes_and_are_rate_limited() {
        let (mut plane, events) = signal_plane();
        let locked = SignalStatus {
            locked: true,
            strength: 200,
            snr: 120,
            ber: 3,
            db: 1234,
            status_valid: true,
        };
        let changed = SignalStatus {
            strength: 90,
            ..locked
        };

        // Untuned: a read happens but the status is not a measurement yet.
        plane.read_once();
        plane.report_signal(locked, 0);
        assert!(events.try_recv().is_err(), "no report before a tune");

        plane.handle(Command::Tune {
            params: Box::new(TuneParams::default()),
            reply: None,
        });
        assert_eq!(events.try_recv(), Ok(Event::Signal(None)), "a tune clears");

        // Tuned but no read since the tune: the cached status is still the default.
        plane.report_signal(locked, 100);
        assert!(events.try_recv().is_err(), "no report before a read");

        plane.read_once();
        plane.report_signal(locked, 100);
        assert_eq!(events.try_recv(), Ok(Event::Signal(Some(locked))));
        plane.report_signal(locked, 200);
        assert!(
            events.try_recv().is_err(),
            "an unchanged reading is not repeated"
        );

        plane.report_signal(changed, 500);
        assert!(
            events.try_recv().is_err(),
            "a change inside one second waits"
        );
        plane.report_signal(changed, 1_100);
        assert_eq!(events.try_recv(), Ok(Event::Signal(Some(changed))));
    }

    fn pat(tsid: u16) -> [u8; DVB_FRAME] {
        let mut packet = [0xff; DVB_FRAME];
        packet[0] = 0x47;
        packet[1] = 0x40;
        packet[2] = 0x00;
        packet[8] = (tsid >> 8) as u8;
        packet[9] = tsid as u8;
        packet
    }

    #[test]
    fn new_transponder_keeps_its_suffix() {
        let mut data = Vec::new();
        data.extend_from_slice(&pat(1));
        data.extend_from_slice(&pat(1));
        data.extend_from_slice(&pat(2));
        data.extend_from_slice(&pat(2));
        let mut id: i32 = 1;
        let position = check_new_transponder(&data, &mut id).expect("new transponder");
        assert_eq!(position, 2 * DVB_FRAME);
        assert_eq!(id, 2);
        // The caller drops the prefix and keeps the matching PAT and its suffix.
        let suffix = &data[position..];
        assert_eq!(suffix.len(), 2 * DVB_FRAME);
        assert_eq!(u16::from(suffix[8]) * 256 + u16::from(suffix[9]), 2);
        // An already-recorded transponder is not new.
        assert_eq!(check_new_transponder(suffix, &mut id), None);
    }
}
