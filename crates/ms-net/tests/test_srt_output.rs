//! SRT output tests with a fake library handle.
//!
//! The C++ test this behaviour comes from (`tests/test_stream.cpp`) is not
//! available in the upstream tree used for this port, so these are new tests;
//! they are named after what they check rather than presented as a port.

#![cfg(feature = "srt")]

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use ms_net::output::{OutputConfig, StreamOutput};
use ms_net::rtp::DVB_FRAME;
use ms_net::srt::{MAX_UDP_PACKET_SIZE, SrtOps};
use ms_tuner::PidSet;

/// Records send sizes and returns scripted results (or the full length).
struct FakeOps {
    calls: Mutex<Vec<usize>>,
    plan: Mutex<VecDeque<i32>>,
}

impl FakeOps {
    fn new(plan: &[i32]) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            plan: Mutex::new(plan.iter().copied().collect()),
        })
    }

    fn calls(&self) -> Vec<usize> {
        self.calls.lock().expect("calls").clone()
    }
}

impl SrtOps for FakeOps {
    fn create_socket(&self) -> i32 {
        -1
    }

    fn bind_acquire(&self, _sock: i32, _udp_fd: i32) -> bool {
        false
    }

    fn set_accept_callback(
        &self,
        _sock: i32,
        _pending: Arc<Mutex<ms_net::srt::PendingPool>>,
    ) -> bool {
        false
    }

    fn listen(&self, _sock: i32, _backlog: i32) -> bool {
        false
    }

    fn accept(&self, _sock: i32) -> i32 {
        -1
    }

    fn close(&self, _sock: i32) {}

    fn is_connected(&self, _sock: i32) -> bool {
        true
    }

    fn send(&self, _sock: i32, data: &[u8]) -> i32 {
        self.calls.lock().expect("calls").push(data.len());
        let scripted = self.plan.lock().expect("plan").pop_front();
        scripted.unwrap_or(data.len() as i32)
    }
}

fn ts_packet(pid: u16, cc: u8) -> [u8; DVB_FRAME] {
    let mut packet = [0xff; DVB_FRAME];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1f;
    packet[2] = pid as u8;
    packet[3] = 0x10 | (cc & 0x0f);
    packet
}

fn ts_buffer(packets: usize, pid: u16) -> Vec<u8> {
    let mut buf = Vec::with_capacity(packets * DVB_FRAME);
    for index in 0..packets {
        buf.extend_from_slice(&ts_packet(pid, index as u8));
    }
    buf
}

fn output(ops: Arc<FakeOps>) -> StreamOutput {
    StreamOutput::srt(7, ops, None, OutputConfig::new(1), 0)
}

#[test]
fn srt_send_chunks_at_max_udp_packet_size() {
    let ops = FakeOps::new(&[]);
    let mut output = output(Arc::clone(&ops));
    let mut pids = PidSet::new();
    pids.insert(0);
    let buf = ts_buffer(16, 0);

    output.process(&buf, &pids, 0, 0).expect("send");

    // stream.cpp:800-823: chunks of at most MAX_UDP_PACKET_SIZE.
    assert_eq!(ops.calls(), vec![1316, 1316, 376]);
    assert!(ops.calls().iter().all(|len| *len <= MAX_UDP_PACKET_SIZE));
    let stats = output.stats();
    assert_eq!(stats.packets, 1, "one flush_stream call");
    assert_eq!(stats.octets, 16 * DVB_FRAME as u32);
    assert!(!output.forced_close());
}

#[test]
fn srt_partial_send_accounts_sent_bytes_and_forces_close() {
    // First send takes 100 bytes, the retry fails.
    let ops = FakeOps::new(&[100, -1]);
    let mut output = output(Arc::clone(&ops));
    let mut pids = PidSet::new();
    pids.insert(0);
    let buf = ts_buffer(16, 0);

    output.process(&buf, &pids, 0, 0).expect("partial send");

    // stream.cpp:810-822: the first 1316-byte chunk takes 100 bytes, the
    // retry of the remaining 1216 bytes fails, so no later chunk is attempted.
    assert_eq!(
        ops.calls(),
        vec![MAX_UDP_PACKET_SIZE, MAX_UDP_PACKET_SIZE - 100]
    );
    assert!(output.forced_close(), "failed send sets sid->timeout = 1");
    let stats = output.stats();
    // rv > 0 still counts one flush and the bytes that went out.
    assert_eq!(stats.packets, 1);
    assert_eq!(stats.octets, 100);
}

#[test]
fn srt_failed_first_send_counts_no_flush() {
    let ops = FakeOps::new(&[-1]);
    let mut output = output(Arc::clone(&ops));
    let mut pids = PidSet::new();
    pids.insert(0);

    output
        .process(&ts_buffer(1, 0), &pids, 0, 0)
        .expect("failed send");

    assert!(output.forced_close());
    let stats = output.stats();
    assert_eq!(stats.packets, 0);
    assert_eq!(stats.octets, 0);
}

#[test]
fn srt_sends_each_iovec_and_counts_one_flush_per_buffer() {
    let ops = FakeOps::new(&[]);
    let mut output = output(Arc::clone(&ops));
    let mut pids = PidSet::new();
    pids.insert(0);
    // Selected/unselected packets interleave, so raw framing produces two iovecs.
    let mut buf = Vec::new();
    for index in 0..4 {
        buf.extend_from_slice(&ts_packet(0, index));
    }
    for index in 0..2 {
        buf.extend_from_slice(&ts_packet(16, index));
    }
    for index in 4..8 {
        buf.extend_from_slice(&ts_packet(0, index));
    }

    output.process(&buf, &pids, 0, 0).expect("send");

    // stream.cpp:1148: one flush_stream call per adapter buffer, with one
    // send per iovec (here two 752-byte runs).
    assert_eq!(ops.calls(), vec![4 * DVB_FRAME, 4 * DVB_FRAME]);
    let stats = output.stats();
    assert_eq!(stats.packets, 1, "one flush_stream call");
    assert_eq!(stats.octets, 8 * DVB_FRAME as u32);
}
