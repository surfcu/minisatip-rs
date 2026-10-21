//! End-to-end data plane: file-replay tuner -> adapter thread -> RTP/UDP on localhost.

use ms_net::StreamType;
use ms_net::bandwidth::Bandwidth;
use ms_net::dataplane::{
    AdapterHandle, ContinuityCounter, DataPlaneConfig, Event, NEW_TRANSPONDER_WINDOW_MS,
    adapter_threshold, check_new_transponder, clamp_threshold, post_tune_filter,
};
use ms_net::output::{OutputConfig, StreamOutput, UdpTarget};
use ms_tuner::{
    DeliverySystem, FileReplayConfig, FileReplayTuner, PID_ALL, PidSet, Polarization, Result,
    SignalStatus, TsBuf, TuneParams, Tuner,
};
use std::collections::{BTreeMap, VecDeque};
use std::io::{self, Write};
use std::net::{SocketAddr, UdpSocket};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const PACKETS: usize = 3000;
const PIDS: [u16; 3] = [0x100, 0x101, 0x200];

fn ts_packet(pid: u16, counter: u32) -> [u8; 188] {
    let mut packet = [0xFFu8; 188];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1F;
    packet[2] = pid as u8;
    packet[3] = 0x10 | (counter & 0x0F) as u8;
    packet[4..8].copy_from_slice(&counter.to_be_bytes());
    packet
}

fn write_capture(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ms-net-{name}-{}.ts", std::process::id()));
    let data: Vec<u8> = (0..PACKETS)
        .flat_map(|index| ts_packet(PIDS[index % PIDS.len()], index as u32))
        .collect();
    std::fs::write(&path, data).unwrap();
    path
}

fn udp_pair() -> (UdpSocket, UdpSocket) {
    for _ in 0..200 {
        let rtp = UdpSocket::bind("127.0.0.1:0").unwrap();
        let port = rtp.local_addr().unwrap().port();
        if !port.is_multiple_of(2) || port == u16::MAX {
            continue;
        }
        if let Ok(rtcp) = UdpSocket::bind(("127.0.0.1", port + 1)) {
            rtp.set_read_timeout(Some(Duration::from_millis(500)))
                .unwrap();
            rtcp.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
            return (rtp, rtcp);
        }
    }
    panic!("no free UDP port pair");
}

fn free_local_pair() -> SocketAddr {
    let (rtp, rtcp) = udp_pair();
    let addr = rtp.local_addr().unwrap();
    drop((rtp, rtcp));
    addr
}

fn output_to(dest: SocketAddr, ssrc: u32) -> StreamOutput {
    let target = UdpTarget {
        local: free_local_pair(),
        dest,
        ttl: None,
    };
    let config = OutputConfig {
        ssrc,
        seq: 100,
        fifo_size: 1 << 16,
    };
    StreamOutput::udp(target, config, 0).unwrap()
}

/// Collects TS packets per PID until `expected` packets arrived or the socket idles.
fn collect(socket: &UdpSocket, expected: usize) -> (BTreeMap<u16, Vec<u32>>, Vec<u16>) {
    let mut by_pid: BTreeMap<u16, Vec<u32>> = BTreeMap::new();
    let mut seqs = Vec::new();
    let mut total = 0;
    let mut buf = [0u8; 2048];
    while total < expected {
        let Ok(len) = socket.recv(&mut buf) else {
            break;
        };
        assert!(
            len >= 12 && (len - 12) % 188 == 0,
            "datagram of {len} bytes"
        );
        assert_eq!(&buf[0..2], &[0x80, 0x21]);
        seqs.push(u16::from_be_bytes([buf[2], buf[3]]));
        for packet in buf[12..len].chunks(188) {
            let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
            let counter = u32::from_be_bytes(packet[4..8].try_into().unwrap());
            by_pid.entry(pid).or_default().push(counter);
            total += 1;
        }
    }
    (by_pid, seqs)
}

#[test]
fn output_clocks_start_at_setup_not_play() {
    // streams_add sets wtime/rtcp_wtime at SETUP (stream.cpp:630-636). An
    // output created five seconds later must still treat SETUP as its clock
    // origin, so the first 200 ms stream timer sends a keepalive.
    let path = write_capture("setup-age");
    let replay = FileReplayConfig {
        looping: true,
        pacing: ms_tuner::Pacing::None,
    };
    let tuner = FileReplayTuner::open(&path, replay).unwrap();
    let adapter = AdapterHandle::spawn(Box::new(tuner), DataPlaneConfig::default()).unwrap();
    let (rx, _rtcp) = udp_pair();
    rx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
    adapter
        .start_output_with_age(
            0,
            output_to(rx.local_addr().unwrap(), 9),
            PidSet::new(),
            5_000,
        )
        .unwrap();
    let mut buf = [0u8; 2048];
    let len = rx.recv(&mut buf).expect("keepalive datagram");
    assert_eq!(len, 12, "bare RTP header from the keepalive path");
}

#[derive(Clone, Default)]
struct BlockableSink {
    blocked: Arc<AtomicBool>,
    data: Arc<Mutex<Vec<u8>>>,
}

impl Write for BlockableSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.blocked.load(Ordering::SeqCst) {
            return Err(io::Error::from(io::ErrorKind::WouldBlock));
        }
        self.data.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn bandwidth_counts_fifo_copies_per_window() {
    // `buffered_bytes += copied` whenever data enters a socket FIFO, and
    // `calculate_bw` copies the window accumulator into `c_buffered`
    // (socketworks.cpp:1505-1506; stream.cpp:1308,1325). A client that blocks
    // and then drains must still move `c_buffered`, so the counter cannot be
    // the queue's instantaneous occupancy.
    let path = write_capture("bandwidth");
    let replay = FileReplayConfig {
        looping: true,
        pacing: ms_tuner::Pacing::None,
    };
    let tuner = FileReplayTuner::open(&path, replay).unwrap();
    let bandwidth = Arc::new(Bandwidth::new());
    let adapter = AdapterHandle::spawn_with_bandwidth(
        Box::new(tuner),
        DataPlaneConfig::default(),
        Some(bandwidth.clone()),
    )
    .unwrap();
    adapter.tune(TuneParams {
        freq: 11_362_000,
        pol: Polarization::Horizontal,
        msys: DeliverySystem::DvbS2,
        sr: 22_000_000,
        src: 1,
        ..TuneParams::default()
    });

    let sink = BlockableSink {
        blocked: Arc::new(AtomicBool::new(true)),
        data: Arc::new(Mutex::new(Vec::new())),
    };
    let blocked = sink.blocked.clone();
    let output = StreamOutput::rtsp_tcp(
        Box::new(sink),
        None,
        OutputConfig {
            ssrc: 1,
            seq: 0,
            fifo_size: 1 << 20,
        },
        0,
    );
    adapter
        .start_output(0, output, PidSet::from_pids([0x100]))
        .expect("output started");

    let deadline = Instant::now() + Duration::from_secs(3);
    while !adapter.query(0).is_some_and(|stats| stats.fifo_written > 0) {
        assert!(
            Instant::now() < deadline,
            "nothing was copied into the FIFO"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    blocked.store(false, Ordering::SeqCst);
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let stats = adapter.query(0).expect("running output");
        if stats.buffered == 0 {
            break;
        }
        assert!(Instant::now() < deadline, "the FIFO did not drain");
        std::thread::sleep(Duration::from_millis(10));
    }
    // Drop the output and the data source so no further FIFO copies happen;
    // the queued data was already drained above. The folded snapshot must
    // still report the copies made in this window, even though the queue is
    // empty by now.
    adapter.stop_output(0);
    adapter.stop();

    // The data-plane thread owns the once-per-second fold; with it stopped,
    // run `calculate_bw` explicitly after a full window.
    std::thread::sleep(Duration::from_millis(1100));
    bandwidth.calculate();
    assert!(bandwidth.snapshot().buffered > 0);

    let _ = std::fs::remove_file(path);
}

#[test]
fn ssrc_command_moves_the_live_output() {
    // stream.cpp:1409-1416: a later `set_session_id` changes the SSRC used by
    // the running output.
    let path = write_capture("set-ssrc");
    let replay = FileReplayConfig {
        looping: true,
        pacing: ms_tuner::Pacing::None,
    };
    let tuner = FileReplayTuner::open(&path, replay).unwrap();
    let adapter = AdapterHandle::spawn(Box::new(tuner), DataPlaneConfig::default()).unwrap();
    let (rx, _rtcp) = udp_pair();
    adapter
        .start_output(0, output_to(rx.local_addr().unwrap(), 1), PidSet::new())
        .unwrap();
    adapter.send(ms_net::dataplane::Command::SetSsrc {
        stream_id: 0,
        ssrc: 0x0BAD_F00D,
    });
    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if let Some(stats) = adapter.query(0)
            && stats.ssrc == 0x0BAD_F00D
        {
            break;
        }
        assert!(Instant::now() < deadline, "SSRC was not updated");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn threshold_port() {
    assert_eq!(clamp_threshold(25), 25);
    assert_eq!(clamp_threshold(0), 200);
    assert_eq!(clamp_threshold(200), 200);
    assert_eq!(adapter_threshold(25, 50, 0, None, false), 25);
    assert_eq!(
        adapter_threshold(25, 50, 1, Some(StreamType::RtspUdp), true),
        25
    );
    assert_eq!(
        adapter_threshold(25, 50, 1, Some(StreamType::RtspTcp), true),
        50
    );
    assert_eq!(
        adapter_threshold(25, 50, 1, Some(StreamType::Http), true),
        50
    );
    assert_eq!(
        adapter_threshold(25, 50, 2, Some(StreamType::RtspUdp), true),
        25
    );
    assert_eq!(
        adapter_threshold(25, 50, 2, Some(StreamType::RtspTcp), false),
        50
    );
    assert_eq!(
        adapter_threshold(25, 300, 1, Some(StreamType::Http), false),
        200
    );
}

#[test]
fn file_replay_to_udp_routes_only_subscribed_pids() {
    let path = write_capture("route");
    let replay = FileReplayConfig {
        looping: true,
        pacing: ms_tuner::Pacing::FixedBitrate(8 * 188 * 3000),
    };
    let tuner = FileReplayTuner::open(&path, replay).unwrap();
    let config = DataPlaneConfig {
        adapter_id: 0,
        adapter_buffer: 100 * 188,
        threshold_ms: 25,
    };
    let adapter = AdapterHandle::spawn(Box::new(tuner), config).unwrap();
    adapter.tune(TuneParams {
        freq: 11_362_000,
        pol: Polarization::Horizontal,
        msys: DeliverySystem::DvbS2,
        sr: 22_000_000,
        src: 1,
        ..TuneParams::default()
    });

    let (rx_a, rtcp_a) = udp_pair();
    let (rx_b, _rtcp_b) = udp_pair();
    let out_a = output_to(rx_a.local_addr().unwrap(), 0xA);
    let out_b = output_to(rx_b.local_addr().unwrap(), 0xB);
    adapter.start_output(1, out_b, PidSet::from_pids([0x101, 0x200]));
    adapter.start_output(0, out_a, PidSet::from_pids([0x100]));

    let per_pid = 300;
    let (a, seqs_a) = collect(&rx_a, per_pid);
    let (b, _) = collect(&rx_b, 2 * per_pid);

    assert_eq!(a.keys().copied().collect::<Vec<_>>(), vec![0x100]);
    assert_eq!(b.keys().copied().collect::<Vec<_>>(), vec![0x101, 0x200]);
    for (pid, counters) in a.iter().chain(b.iter()) {
        assert!(!counters.is_empty(), "pid {pid:#x} missing");
        assert!(
            counters
                .windows(2)
                .all(|pair| pair[1] == pair[0] + 3 || pair[1] < pair[0]),
            "pid {pid:#x} reordered or gapped"
        );
    }
    let seqs_ok = seqs_a
        .windows(2)
        .all(|pair| pair[1] == pair[0].wrapping_add(1));
    assert!(seqs_ok, "RTP sequence not consecutive: {seqs_a:?}");
    assert!(seqs_a.first().is_some_and(|seq| *seq >= 100));

    // The data plane also emits RTCP with the SES1 description.
    let mut report = [0u8; 2048];
    let len = rtcp_a.recv(&mut report).expect("RTCP report");
    let desc = String::from_utf8_lossy(&report[68..68 + usize::from(report[67])]).into_owned();
    assert!(len >= 68);
    assert_eq!(
        desc,
        "ver=1.0;src=1;tuner=1,255,1,15,11362,h,dvbs2,qpsk,,,22000,none;pids=256"
    );

    let stats = adapter.query(0).expect("stats for stream 0");
    assert_eq!(stats.ssrc, 0xA);
    assert!(stats.packets > 0);
    assert!(adapter.query(7).is_none());

    adapter.stop_output(0);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut stopped = None;
    while Instant::now() < deadline && stopped.is_none() {
        if let Ok(Event::OutputStopped(id, stats, _)) =
            adapter.events().recv_timeout(Duration::from_millis(100))
        {
            stopped = Some((id, stats));
        }
    }
    let (id, final_stats) = stopped.expect("OutputStopped event");
    assert_eq!(id, 0);
    assert!(final_stats.packets >= stats.packets);
    assert!(adapter.query(0).is_none());

    adapter.stop();
    let _ = std::fs::remove_file(path);
}

/// A tuner that emits scripted chunks once, optionally sleeping first.
struct ScriptedTuner {
    chunks: VecDeque<(Duration, Vec<u8>)>,
    wait: bool,
}

impl ScriptedTuner {
    fn new(wait: bool, chunks: Vec<(Duration, Vec<u8>)>) -> Self {
        Self {
            chunks: chunks.into(),
            wait,
        }
    }
}

impl Tuner for ScriptedTuner {
    fn tune(&mut self, _params: &TuneParams) -> Result<()> {
        Ok(())
    }

    fn set_pids(&mut self, _pids: &PidSet) -> Result<()> {
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        SignalStatus {
            locked: true,
            strength: 255,
            snr: 15,
        }
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        let Some((delay, data)) = self.chunks.pop_front() else {
            return Ok(0);
        };
        std::thread::sleep(delay);
        Ok(buf.push_data(&data))
    }

    fn take_wait_new_stream(&mut self) -> bool {
        std::mem::take(&mut self.wait)
    }
}

fn chunk(pid: u16, count: u32) -> Vec<u8> {
    (0..count).flat_map(|index| ts_packet(pid, index)).collect()
}

fn scripted_adapter(
    tuner: ScriptedTuner,
    rx: &UdpSocket,
    pids: impl IntoIterator<Item = u16>,
) -> AdapterHandle {
    let config = DataPlaneConfig {
        adapter_buffer: 100 * 188,
        ..DataPlaneConfig::default()
    };
    let adapter = AdapterHandle::spawn(Box::new(tuner), config).unwrap();
    adapter.start_output(
        1,
        output_to(rx.local_addr().unwrap(), 1),
        PidSet::from_pids(pids),
    );
    adapter
}

// src_satipc.cpp:211-216 + src_stream.cpp:1241-1259: after PLAY the data plane
// drops the old transponder until the new PAT transport_stream_id arrives.
#[test]
fn satipc_wait_new_stream_flushes_old_transponder() {
    let (rx, _rtcp) = udp_pair();
    let mut new = pat_packet(0x2222).to_vec();
    new.extend_from_slice(&chunk(0x100, 10));
    let tuner = ScriptedTuner::new(
        true,
        vec![(Duration::ZERO, chunk(0x101, 10)), (Duration::ZERO, new)],
    );
    let adapter = scripted_adapter(tuner, &rx, [0, 0x100, 0x101]);

    let (received, _) = collect(&rx, 11);
    assert!(
        !received.contains_key(&0x101),
        "old-transponder packets must be flushed"
    );
    assert_eq!(received.get(&0).map(Vec::len), Some(1), "new PAT must flow");
    assert_eq!(
        received.get(&0x100).map(Vec::len),
        Some(10),
        "packets after the new PAT must flow"
    );
    adapter.stop();
}

// src_stream.cpp:1244: the flush window is only 200 ms.
#[test]
fn satipc_wait_new_stream_gives_up_after_200ms() {
    let (rx, _rtcp) = udp_pair();
    let tuner = ScriptedTuner::new(
        true,
        vec![
            (Duration::ZERO, chunk(0x101, 10)),
            (Duration::from_millis(250), chunk(0x102, 10)),
        ],
    );
    let adapter = scripted_adapter(tuner, &rx, [0x101, 0x102]);

    let (received, _) = collect(&rx, 10);
    assert_eq!(
        received.keys().copied().collect::<Vec<_>>(),
        vec![0x102],
        "data after the 200 ms window must flow even without a new PAT"
    );
    adapter.stop();
}

// Only satipc sets `wait_new_stream`; other tuners must not flush.
#[test]
fn tuners_without_wait_new_stream_forward_immediately() {
    let (rx, _rtcp) = udp_pair();
    let tuner = ScriptedTuner::new(false, vec![(Duration::ZERO, chunk(0x103, 10))]);
    let adapter = scripted_adapter(tuner, &rx, [0x103]);

    let (received, _) = collect(&rx, 10);
    assert_eq!(
        received.keys().copied().collect::<Vec<_>>(),
        vec![0x103],
        "the default must forward data"
    );
    adapter.stop();
}

#[test]
fn set_pids_changes_routing() {
    let path = write_capture("setpids");
    let replay = FileReplayConfig {
        looping: true,
        pacing: ms_tuner::Pacing::FixedBitrate(8 * 188 * 2000),
    };
    let tuner = FileReplayTuner::open(&path, replay).unwrap();
    let config = DataPlaneConfig {
        adapter_buffer: 100 * 188,
        ..DataPlaneConfig::default()
    };
    let adapter = AdapterHandle::spawn(Box::new(tuner), config).unwrap();

    let (rx, _rtcp) = udp_pair();
    adapter.start_output(
        3,
        output_to(rx.local_addr().unwrap(), 3),
        PidSet::from_pids([0x100]),
    );
    let (first, _) = collect(&rx, 50);
    assert_eq!(first.keys().copied().collect::<Vec<_>>(), vec![0x100]);

    adapter.set_pids(3, PidSet::from_pids([0x200]));
    // Drain what was in flight, then only 0x200 must arrive.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut switched = false;
    while Instant::now() < deadline {
        let (batch, _) = collect(&rx, 20);
        if batch.keys().eq([0x200u16].iter()) {
            switched = true;
            break;
        }
    }
    assert!(switched, "set_pids did not switch to 0x200");
    adapter.stop();
    let _ = std::fs::remove_file(path);
}

fn pat_packet(transponder_id: u16) -> [u8; 188] {
    let mut packet = ts_packet(0, 0);
    packet[1] = 0x40; // payload unit start, PID 0
    packet[2] = 0;
    packet[8] = (transponder_id >> 8) as u8;
    packet[9] = transponder_id as u8;
    packet
}

#[test]
fn new_transponder_is_found_at_packet_offset() {
    let mut wait = 0x1111;
    let mut data = Vec::new();
    data.extend_from_slice(&ts_packet(0x100, 0));
    data.extend_from_slice(&ts_packet(0x100, 1));
    data.extend_from_slice(&pat_packet(0x1234));
    data.extend_from_slice(&pat_packet(0x5678));

    // stream.cpp:864-889: the buffer is kept from the first PAT with a new id.
    assert_eq!(check_new_transponder(&data, &mut wait), Some(2 * 188));
    assert_eq!(wait, 0x1234);
    // Old PATs are skipped, the next new one is found.
    assert_eq!(check_new_transponder(&data, &mut wait), Some(3 * 188));
    assert_eq!(wait, 0x5678);
    let only_latest = pat_packet(0x5678);
    assert_eq!(check_new_transponder(&only_latest, &mut wait), None);
}

#[test]
fn post_tune_filter_drops_only_inside_the_window() {
    let mut data = Vec::new();
    data.extend_from_slice(&ts_packet(0x100, 0));
    data.extend_from_slice(&pat_packet(0x2222));

    let mut wait = 0x1111;
    assert_eq!(
        post_tune_filter(&data, &mut wait, NEW_TRANSPONDER_WINDOW_MS - 1),
        Some(188)
    );
    assert_eq!(wait, 0x2222);

    let mut wait = 0x1111;
    assert_eq!(
        post_tune_filter(&data, &mut wait, NEW_TRANSPONDER_WINDOW_MS),
        Some(0),
        "the window expired: keep the whole buffer"
    );
    assert_eq!(wait, 0x1111, "outside the window the id is not updated");

    let mut wait = 0x1111;
    let no_pat = ts_packet(0x100, 0);
    assert_eq!(post_tune_filter(&no_pat, &mut wait, 0), None);

    let mut wait = 0x2222;
    let old_pat = pat_packet(0x2222);
    assert_eq!(post_tune_filter(&old_pat, &mut wait, 0), None);
}

#[test]
fn continuity_counter_flags_gaps_and_skips_tei_and_null_packets() {
    let mut cc = ContinuityCounter::new();
    for counter in [0u32, 1, 2] {
        assert!(!cc.check_packet(&ts_packet(0x100, counter)));
    }
    assert!(cc.check_packet(&ts_packet(0x100, 4)), "CC 4 after 2");
    assert_eq!(cc.errors(), 1);
    assert!(!cc.check_packet(&ts_packet(0x100, 5)));
    assert_eq!(cc.packets(), 5);

    let mut tei = ts_packet(0x100, 9);
    tei[1] |= 0x80;
    assert!(!cc.check_packet(&tei));
    assert_eq!(cc.packets(), 5, "TEI packets are skipped");

    assert!(!cc.check_packet(&ts_packet(8191, 9)));
    assert_eq!(cc.packets(), 5, "null packets are skipped");
    assert_eq!(cc.errors(), 1);
}

#[test]
fn continuity_counter_skips_all_pid_mode_and_forgets_removed_pids() {
    let all = PidSet::from_pids([PID_ALL]);
    let mut cc = ContinuityCounter::new();
    let mut data = Vec::new();
    data.extend_from_slice(&ts_packet(0x100, 0));
    data.extend_from_slice(&ts_packet(0x100, 5));
    cc.check_buffer(&data, &all);
    assert_eq!(cc.packets(), 0, "PID 8192 disables checking");
    assert_eq!(cc.errors(), 0);

    let only_100 = PidSet::from_pids([0x100]);
    cc.check_buffer(&data, &only_100);
    assert_eq!(cc.packets(), 2);
    assert_eq!(cc.errors(), 1);

    cc.retain_pids(&PidSet::from_pids([0x200]));
    assert!(
        !cc.check_packet(&ts_packet(0x100, 9)),
        "re-added PID starts over"
    );
}

/// Marks every packet it sees, to check that stages run before the outputs.
struct MarkStage(std::sync::Arc<std::sync::atomic::AtomicU64>);

impl ms_ts::TsStage for MarkStage {
    fn process(&mut self, buf: &mut [u8]) {
        for packet in buf.chunks_mut(188) {
            packet[187] = 0xA5;
        }
        self.0
            .fetch_add(buf.len() as u64 / 188, std::sync::atomic::Ordering::SeqCst);
    }
}

#[test]
fn section_pids_are_tapped_and_stages_rewrite_the_batch() {
    let path = write_capture("stage");
    let tuner = FileReplayTuner::open(
        &path,
        FileReplayConfig {
            looping: true,
            pacing: ms_tuner::Pacing::FixedBitrate(4_000_000),
        },
    )
    .unwrap();
    let seen = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    let handle = AdapterHandle::spawn_with_stages(
        Box::new(tuner),
        DataPlaneConfig::default(),
        None,
        vec![Box::new(MarkStage(std::sync::Arc::clone(&seen)))],
    )
    .unwrap();
    assert_eq!(handle.tune_generation(), 0);
    handle
        .tune_wait(TuneParams::default(), Duration::from_secs(2))
        .unwrap();
    assert_eq!(
        handle.tune_generation(),
        1,
        "raised before the tune answers"
    );
    handle.send(ms_net::dataplane::Command::SetSectionPids(
        PidSet::from_pids([0x100]),
    ));
    let (socket, _) = udp_pair();
    socket
        .set_read_timeout(Some(Duration::from_secs(2)))
        .unwrap();
    let dest = socket.local_addr().unwrap();
    handle.start_output(1, output_to(dest, 7), PidSet::from_pids([0x100, 0x101]));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    let mut tapped = None;
    while tapped.is_none() && std::time::Instant::now() < deadline {
        if let Ok(Event::TablePackets {
            generation,
            packets,
        }) = handle.events().recv_timeout(Duration::from_millis(100))
        {
            tapped = Some((generation, packets));
        }
    }
    let (generation, packets) = tapped.expect("no TablePackets event");
    assert_eq!(generation, 1);
    assert!(!packets.is_empty());
    for packet in packets.chunks(188) {
        assert_eq!(
            (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]),
            0x100
        );
        assert_ne!(packet[187], 0xA5, "tapped before the stages ran");
    }
    let mut buf = [0u8; 2048];
    let len = socket.recv(&mut buf).unwrap();
    for packet in buf[12..len].chunks(188) {
        assert_eq!(packet[187], 0xA5, "outputs see the stage's rewrite");
    }
    assert!(seen.load(std::sync::atomic::Ordering::SeqCst) > 0);
    handle.stop();
    let _ = std::fs::remove_file(path);
}
