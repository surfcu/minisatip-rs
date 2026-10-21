//! Per-adapter data plane: one thread reads a tuner, batches like `read_dmx` and
//! routes packets by PID to its outputs. It is driven only by messages.

use crate::describe::{DescribeInput, describe_adapter};
use crate::handler::{StreamId, StreamType};
use crate::output::{OutputStats, STREAM_TIMER_MS, StreamOutput};
use crate::rtp::DVB_FRAME;
use ms_tuner::{PidSet, TsBuf, TuneParams, Tuner};
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
/// Command poll interval while the tuner has no data.
const IDLE_POLL: Duration = Duration::from_millis(5);

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

/// Messages from the control plane.
pub enum Command {
    /// Tunes the tuner; the parameters also feed the RTCP description.
    Tune(Box<TuneParams>),
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

    /// Tunes the adapter.
    pub fn tune(&self, params: TuneParams) -> bool {
        self.send(Command::Tune(Box::new(params)))
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
    tune: TuneParams,
    buf: TsBuf,
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
            tune: TuneParams::default(),
            buf: TsBuf::new(config.adapter_buffer),
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
            Command::Tune(params) => {
                if let Err(error) = self.tuner.tune(&params) {
                    let _ = self.events.send(Event::TunerError(error.to_string()));
                }
                self.tune = *params;
                self.buf.clear();
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
        let mut pids = PidSet::new();
        for subscriber in self.outputs.values() {
            pids.extend(subscriber.pids.iter());
        }
        if let Err(error) = self.tuner.set_pids(&pids) {
            let _ = self.events.send(Event::TunerError(error.to_string()));
        }
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
            if !self.buf.is_empty() && now - self.last_send_ms > FORCE_SEND_MS {
                self.process(now);
            }
            return false;
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
