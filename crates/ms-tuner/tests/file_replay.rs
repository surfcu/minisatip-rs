//! Tests for the file-replay tuner backend.

use ms_tuner::{
    DeliverySystem, FileReplayConfig, FileReplayTuner, PID_ALL, Pacing, PidSet, Polarization,
    SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner,
};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

struct TempTs {
    dir: PathBuf,
    path: PathBuf,
}

impl Drop for TempTs {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn temp_ts(contents: &[u8]) -> TempTs {
    let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("ms-tuner-test-{}-{id}", std::process::id()));
    fs::create_dir_all(&dir).unwrap();
    let path = dir.join("capture.ts");
    fs::write(&path, contents).unwrap();
    TempTs { dir, path }
}

fn packet(pid: u16, cc: u8, fill: u8) -> [u8; TS_PACKET_SIZE] {
    let mut packet = [fill; TS_PACKET_SIZE];
    packet[0] = 0x47;
    packet[1] = ((pid >> 8) as u8) & 0x1F;
    packet[2] = pid as u8;
    packet[3] = 0x10 | (cc & 0x0F);
    packet
}

fn packet_pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2])
}

fn packet_cc(packet: &[u8]) -> u8 {
    packet[3] & 0x0F
}

fn read_packets(tuner: &mut FileReplayTuner, capacity: usize) -> Vec<u8> {
    let mut buf = TsBuf::with_packets(capacity);
    let written = tuner.read(&mut buf).unwrap();
    assert_eq!(written, buf.len(), "read must report the appended bytes");
    buf.as_bytes().to_vec()
}

#[test]
fn filters_by_pid() {
    let contents: Vec<u8> = [
        packet(0x100, 0, 0xAA),
        packet(0x200, 0, 0xBB),
        packet(0x100, 1, 0xCC),
        packet(0x1FFF, 0, 0xDD),
    ]
    .concat();
    let temp = temp_ts(&contents);
    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    let mut pids = PidSet::new();
    pids.insert(0x100);
    tuner.set_pids(&pids).unwrap();

    let data = read_packets(&mut tuner, 8);
    assert_eq!(data.len(), 2 * TS_PACKET_SIZE);
    let packets = data.as_chunks::<TS_PACKET_SIZE>().0;
    assert_eq!(packet_pid(&packets[0]), 0x100);
    assert_eq!(packet_cc(&packets[0]), 0);
    assert_eq!(packet_pid(&packets[1]), 0x100);
    assert_eq!(packet_cc(&packets[1]), 1);

    let mut buf = TsBuf::with_packets(8);
    assert_eq!(tuner.read(&mut buf).unwrap(), 0, "file must be at EOF");
}

#[test]
fn all_pids_sentinel_passes_everything() {
    let contents: Vec<u8> = [
        packet(0x100, 0, 0x01),
        packet(0x200, 0, 0x02),
        packet(0x0100, 1, 0x03),
        packet(0x1FFF, 0, 0x04),
    ]
    .concat();
    let temp = temp_ts(&contents);
    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    let pids = PidSet::all();
    assert!(pids.is_all());
    assert!(pids.contains(PID_ALL));
    assert!(pids.contains(0x1FFF));
    tuner.set_pids(&pids).unwrap();

    let data = read_packets(&mut tuner, 8);
    assert_eq!(data.len(), 4 * TS_PACKET_SIZE);
    let pids_read: Vec<u16> = data
        .as_chunks::<TS_PACKET_SIZE>()
        .0
        .iter()
        .map(|packet| packet_pid(packet))
        .collect();
    assert_eq!(pids_read, vec![0x100, 0x200, 0x100, 0x1FFF]);
}

#[test]
fn stops_or_loops_at_eof() {
    let contents: Vec<u8> = [
        packet(0x100, 0, 0x11),
        packet(0x100, 1, 0x12),
        packet(0x100, 2, 0x13),
    ]
    .concat();
    let temp = temp_ts(&contents);

    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    tuner.set_pids(&PidSet::all()).unwrap();
    assert_eq!(read_packets(&mut tuner, 8).len(), 3 * TS_PACKET_SIZE);
    let mut buf = TsBuf::with_packets(8);
    assert_eq!(tuner.read(&mut buf).unwrap(), 0, "non-looping file stops");

    let mut looping = FileReplayTuner::open(
        &temp.path,
        FileReplayConfig {
            looping: true,
            pacing: Pacing::None,
        },
    )
    .unwrap();
    looping.set_pids(&PidSet::all()).unwrap();
    let first = read_packets(&mut looping, 8);
    let second = read_packets(&mut looping, 8);
    assert_eq!(first.len(), 3 * TS_PACKET_SIZE);
    assert_eq!(second.len(), 3 * TS_PACKET_SIZE);
    assert_eq!(first, second, "looping replays the same packets");
    assert_eq!(packet_cc(&second[..TS_PACKET_SIZE]), 0);
}

#[test]
fn read_returns_whole_packets_only() {
    let mut contents: Vec<u8> = [
        packet(0x100, 0, 0x21),
        packet(0x100, 1, 0x22),
        packet(0x100, 2, 0x23),
    ]
    .concat();
    contents.extend_from_slice(&[0xEE; 50]);
    let temp = temp_ts(&contents);

    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    tuner.set_pids(&PidSet::all()).unwrap();
    let data = read_packets(&mut tuner, 8);
    assert_eq!(data.len(), 3 * TS_PACKET_SIZE);
    assert_eq!(
        data.len() % TS_PACKET_SIZE,
        0,
        "partial packet must be dropped"
    );
    assert_eq!(data.as_chunks::<TS_PACKET_SIZE>().0.len(), 3);

    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    tuner.set_pids(&PidSet::all()).unwrap();
    let mut buf = TsBuf::with_packets(2);
    assert_eq!(tuner.read(&mut buf).unwrap(), 2 * TS_PACKET_SIZE);
    assert_eq!(buf.packets().count(), 2);
    let mut buf = TsBuf::with_packets(2);
    assert_eq!(tuner.read(&mut buf).unwrap(), TS_PACKET_SIZE);
    assert_eq!(buf.packets().count(), 1);
    let mut buf = TsBuf::with_packets(2);
    assert_eq!(tuner.read(&mut buf).unwrap(), 0);
}

#[test]
fn tune_is_recorded_and_status_is_configurable() {
    let temp = temp_ts(&packet(0x100, 0, 0));
    let mut tuner = FileReplayTuner::open(&temp.path, FileReplayConfig::default()).unwrap();
    let params = TuneParams {
        freq: 11_000_000,
        pol: Polarization::Vertical,
        msys: DeliverySystem::DvbS2,
        sr: 27_500_000,
        src: 2,
        fec: 3,
        mtype: 8,
        plts: Some(true),
        ro: Some(2),
        bw: Some(8_000_000),
        plp: Some(0),
        pids: PidSet::all(),
        ..TuneParams::default()
    };

    tuner.tune(&params).unwrap();
    assert_eq!(tuner.last_tune(), Some(&params));

    assert_eq!(tuner.status(), SignalStatus::default());
    tuner.set_signal(120, 200);
    assert_eq!(
        tuner.status(),
        SignalStatus {
            locked: true,
            strength: 120,
            snr: 200
        }
    );
    let unlocked = SignalStatus {
        locked: false,
        strength: 1,
        snr: 2,
    };
    tuner.set_status(unlocked);
    assert_eq!(tuner.status(), unlocked);
}
