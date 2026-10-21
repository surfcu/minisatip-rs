//! Per-adapter data plane: one thread reads a tuner, batches like `read_dmx` and
//! routes packets by PID to its outputs. It is driven only by messages.

use crate::describe::{DescribeInput, describe_adapter};
use crate::handler::{StreamId, StreamType};
use crate::output::{OutputStats, STREAM_TIMER_MS, StreamOutput};
use crate::rtp::DVB_FRAME;
use ms_sched::{AdapterTuner, TuneError};
use ms_tuner::{PID_ALL, PidSet, TsBuf, TuneParams, Tuner, TunerError};
use std::collections::BTreeMap;
use std::io;
use std::net::SocketAddr;
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime};

/// Adapter buffer size (`ADAPTER_BUFFER`, `-B` default).
pub const ADAPTER_BUFFER: usize = 2000 * DVB_FRAME;
/// Longest wait before a batch is forced out (the PCR interval).
pub const FORCE_SEND_MS: i64 = 40;
/// Time after a tune during which data is dropped until a new PAT appears
/// (`read_dmx`, stream.cpp:1223-1244).
pub const NEW_TRANSPONDER_WINDOW_MS: i64 = 200;
/// Minimum interval between continuity-error reports to the control plane.
const CC_REPORT_MS: i64 = 1000;
/// Command poll interval while the tuner has no data.
const IDLE_POLL: Duration = Duration::from_millis(5);
/// Upper bound the control plane waits for a tune result.
///
/// C++ tunes synchronously in the request path, so SETUP/PLAY cannot answer
/// before the frontend ioctls finish. The bound only guards against a wedged
/// adapter thread: expiry is reported as a hardware failure (`-500`).
pub const TUNE_REPLY_TIMEOUT: Duration = Duration::from_secs(5);
/// Null-packet PID skipped by `check_cc` (stream.cpp:905).
const PID_NULL: u16 = 8191;

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
}

impl Default for DataPlaneConfig {
    fn default() -> Self {
        Self {
            adapter_id: 0,
            adapter_buffer: ADAPTER_BUFFER,
            threshold_ms: clamp_threshold(25),
        }
    }
}

/// Finds the start of the first PAT carrying a transponder id other than
/// `wait_transponder_id`, updating the remembered id (`check_new_transponder`,
/// stream.cpp:864-889). Returns the offset of that PAT in `data`, if any.
pub fn check_new_transponder(data: &[u8], wait_transponder_id: &mut i32) -> Option<usize> {
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
                return Some(offset);
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
    check_new_transponder(data, wait_transponder_id)
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
}

impl ContinuityCounter {
    /// Creates an empty tracker.
    pub fn new() -> Self {
        Self::default()
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
        let last = self.last.entry(pid).or_insert(-1);
        let expected = if *last < 0 || *last > 15 {
            cc
        } else {
            (*last + 1) % 16
        };
        let error = expected != cc;
        if error {
            self.errors += 1;
        }
        *last = cc;
        error
    }

    /// Checks every complete packet in `data` whose PID is in `pids`.
    ///
    /// Like C++, checking is skipped entirely while `PID_ALL` is active.
    pub fn check_buffer(&mut self, data: &[u8], pids: &PidSet) {
        if pids.contains(PID_ALL) {
            return;
        }
        for packet in data.as_chunks::<DVB_FRAME>().0 {
            let pid = (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2]);
            if pids.contains(pid) {
                self.check_packet(packet);
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
        /// The output.
        output: Box<StreamOutput>,
        /// PIDs routed to it.
        pids: PidSet,
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
    /// Moves a UDP output to a new client address.
    SetDestination {
        /// Target stream.
        stream_id: StreamId,
        /// New RTP destination.
        dest: SocketAddr,
    },
    /// Replaces the send threshold (see [`adapter_threshold`]).
    SetThreshold(i64),
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
    OutputClosed(StreamId),
    /// An output was removed by [`Command::StopOutput`].
    OutputStopped(StreamId, OutputStats),
    /// Total continuity errors seen by this adapter (`ad->ccerr`).
    ContinuityErrors(u64),
    /// The tuner failed to tune or read.
    TunerError(String),
}

/// Control-plane handle to one adapter's data-plane thread.
pub struct AdapterHandle {
    commands: Sender<Command>,
    events: Receiver<Event>,
    thread: Option<JoinHandle<()>>,
}

impl AdapterHandle {
    /// Spawns the data-plane thread for `tuner`.
    pub fn spawn(tuner: Box<dyn Tuner + Send>, config: DataPlaneConfig) -> io::Result<Self> {
        let (commands, command_rx) = mpsc::channel();
        let (event_tx, events) = mpsc::channel();
        let thread = thread::Builder::new()
            .name(format!("adapter{}", config.adapter_id))
            .spawn(move || DataPlane::new(tuner, config, command_rx, event_tx).run())?;
        Ok(Self {
            commands,
            events,
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

    /// Starts an output for `stream_id`.
    pub fn start_output(&self, stream_id: StreamId, output: StreamOutput, pids: PidSet) -> bool {
        self.send(Command::StartOutput {
            stream_id,
            output: Box::new(output),
            pids,
        })
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
}

struct Subscriber {
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
    tune_time_ms: i64,
    wait_transponder_id: i32,
    threshold_ms: i64,
    last_send_ms: i64,
    origin: Instant,
    running: bool,
}

impl DataPlane {
    fn new(
        tuner: Box<dyn Tuner + Send>,
        config: DataPlaneConfig,
        commands: Receiver<Command>,
        events: Sender<Event>,
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
            continuity: ContinuityCounter::new(),
            reported_cc_errors: 0,
            last_cc_report_ms: 0,
            wait_new_stream: false,
            tune_time_ms: 0,
            wait_transponder_id: 0,
            threshold_ms: config.threshold_ms,
            last_send_ms: 0,
            origin: Instant::now(),
            running: true,
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
            let got_data = !self.outputs.is_empty() && self.read_once();
            self.timers();
            self.flush_queues();
            if !got_data {
                let wait = if self.outputs.is_empty() {
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
                self.tune_time_ms = self.mono_us() / 1000;
                self.update_tuner_pids();
            }
            Command::StartOutput {
                stream_id,
                output,
                pids,
            } => {
                let now = self.mono_us() / 1000;
                self.outputs.insert(
                    stream_id,
                    Subscriber {
                        output: *output,
                        pids,
                        last_timer_ms: now,
                    },
                );
                self.update_tuner_pids();
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
                    let _ = output.flush_queue();
                    let stats = output.stats();
                    let _ = self.events.send(Event::OutputStopped(stream_id, stats));
                    self.update_tuner_pids();
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
            Command::SetDestination { stream_id, dest } => {
                if let Some(subscriber) = self.outputs.get_mut(&stream_id) {
                    let _ = subscriber.output.set_udp_destination(dest);
                }
            }
            Command::SetThreshold(threshold) => self.threshold_ms = threshold,
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

    fn update_tuner_pids(&mut self) {
        // The scheduler owns the adapter table (defaults included); outputs add
        // their own PIDs so a START between two scheduler updates keeps them.
        let mut pids = self.base_pids.clone();
        for subscriber in self.outputs.values() {
            pids.extend(subscriber.pids.iter());
        }
        if let Err(error) = self.tuner.set_pids(&pids) {
            let _ = self.events.send(Event::TunerError(error.to_string()));
            return;
        }
        self.continuity.retain_pids(&pids);
        self.active_pids = pids;
    }

    fn read_once(&mut self) -> bool {
        let read = match self.tuner.read(&mut self.buf) {
            Ok(read) => read,
            Err(error) => {
                let _ = self.events.send(Event::TunerError(error.to_string()));
                return false;
            }
        };
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
        if !self.after_read(now) {
            return true;
        }
        let since = now - self.last_send_ms;
        let send = since > self.threshold_ms;
        let force = since > FORCE_SEND_MS
            || since > 4 * self.threshold_ms
            || self.buf.remaining_bytes() <= 7 * DVB_FRAME;
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
            if self.tune_time_ms == 0 {
                self.tune_time_ms = now_ms;
            }
            let elapsed = now_ms - self.tune_time_ms;
            match post_tune_filter(self.buf.as_bytes(), &mut self.wait_transponder_id, elapsed) {
                Some(offset) => {
                    self.buf.discard_front(offset);
                    self.cc_checked = self.cc_checked.saturating_sub(offset);
                }
                None => {
                    self.buf.clear();
                    self.cc_checked = 0;
                    return false;
                }
            }
            self.tune_time_ms = 0;
            self.wait_new_stream = false;
        }
        let checked = self.cc_checked.min(self.buf.len()) / DVB_FRAME * DVB_FRAME;
        self.continuity
            .check_buffer(&self.buf.as_bytes()[checked..], &self.active_pids);
        self.cc_checked = self.buf.len();
        true
    }

    fn process(&mut self, now_ms: i64) {
        self.last_send_ms = now_ms;
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

    fn timers(&mut self) {
        let mono = self.mono_us();
        let now = mono / 1000;
        let wall = SystemTime::now();
        let signal = self.tuner.status();
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
                let _ = self.events.send(Event::RtcpReceived(*stream_id));
            }
            let input = DescribeInput {
                adapter_id: self.config.adapter_id,
                tune: &self.tune,
                signal: Some(signal),
                use_adapter: true,
                pids: &subscriber.pids,
            };
            let result = subscriber
                .output
                .on_timer(now, mono, wall, || describe_adapter(&input));
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
        if self.outputs.remove(&stream_id).is_some() {
            let _ = self.events.send(Event::OutputClosed(stream_id));
            self.update_tuner_pids();
        }
    }
}
