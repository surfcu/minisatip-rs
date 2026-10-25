//! RTP packetizing, RTCP layout and SAT>IP description strings.

use ms_net::describe::{DescribeInput, adapter_pid_list, describe_adapter, pid_list};
use ms_net::rtcp::{NTP_UNIX_OFFSET, RtcpInfo, build_rtcp, ntp_seconds};
use ms_net::rtp::{Batch, Framing, RtpState, Segment, TCP_MAX_IOV, packetize, rtp_timestamp};
use ms_tuner::{DeliverySystem, PID_ALL, PidSet, Polarization, SignalStatus, TuneParams};
use std::time::{Duration, UNIX_EPOCH};

fn ts_packet(pid: u16, fill: u8) -> [u8; 188] {
    let mut packet = [fill; 188];
    packet[0] = 0x47;
    packet[1] = (pid >> 8) as u8 & 0x1F;
    packet[2] = pid as u8;
    packet[3] = 0x10;
    packet
}

fn buffer(pids: &[u16]) -> Vec<u8> {
    pids.iter()
        .enumerate()
        .flat_map(|(index, pid)| ts_packet(*pid, index as u8))
        .collect()
}

fn header_count(batch: &Batch) -> usize {
    batch
        .segments
        .iter()
        .filter(|segment| matches!(segment, Segment::Header { .. }))
        .count()
}

#[test]
fn rtp_header_fields() {
    let mut rtp = RtpState::new(0xDEAD_BEEF, 0x1234);
    let mut out = Vec::new();
    let len = rtp.write_header(&mut out, Framing::RtpUdp, 1316, 2_500_000);
    assert_eq!(len, 12);
    assert_eq!(
        &out[0..2],
        &[0x80, 0x21],
        "V=2, no padding/extension/CSRC, M=0, PT=33"
    );
    assert_eq!(&out[2..4], &[0x12, 0x34]);
    assert_eq!(u32::from_be_bytes(out[4..8].try_into().unwrap()), 225_000);
    assert_eq!(&out[8..12], &[0xDE, 0xAD, 0xBE, 0xEF]);
    assert_eq!(rtp.seq, 0x1235);
}

#[test]
fn rtp_seq_wraps_around() {
    let mut rtp = RtpState::new(1, 0xFFFF);
    let mut out = Vec::new();
    rtp.write_header(&mut out, Framing::RtpUdp, 0, 0);
    rtp.write_header(&mut out, Framing::RtpUdp, 0, 0);
    assert_eq!(&out[2..4], &[0xFF, 0xFF]);
    assert_eq!(&out[14..16], &[0x00, 0x00]);
    assert_eq!(rtp.seq, 1);
}

#[test]
fn rtp_timestamp_matches_cpp_formula() {
    // 90000 * whole seconds + 9 * remaining microseconds / 100, truncated to 32 bits.
    assert_eq!(rtp_timestamp(0), 0);
    assert_eq!(rtp_timestamp(1_000_000), 90_000);
    assert_eq!(rtp_timestamp(1_000_011), 90_000);
    assert_eq!(rtp_timestamp(1_000_012), 90_001);
    assert_eq!(rtp_timestamp(1_500_000), 135_000);
    let secs: i64 = 50_000;
    let expected = (90_000 * secs + 9 * 999_999 / 100) as u32;
    assert_eq!(rtp_timestamp(secs * 1_000_000 + 999_999), expected);
}

#[test]
fn interleaved_header_prefix() {
    let mut rtp = RtpState::new(7, 0);
    let mut out = Vec::new();
    let len = rtp.write_header(&mut out, Framing::RtpTcp, 42 * 188, 0);
    assert_eq!(len, 16);
    assert_eq!(out[0], b'$');
    assert_eq!(out[1], 0, "RTP always uses channel 0");
    assert_eq!(u16::from_be_bytes([out[2], out[3]]) as usize, 42 * 188 + 12);
    assert_eq!(&out[4..6], &[0x80, 0x21]);
}

#[test]
fn udp_groups_seven_packets_per_rtp() {
    let buf = buffer(&[0x100; 15]);
    let mut rtp = RtpState::new(1, 10);
    let batches = packetize(&mut rtp, Framing::RtpUdp, &buf, |_| true, 0);
    assert_eq!(batches.len(), 1);
    let datagrams = batches[0].datagrams(&buf);
    let sizes: Vec<usize> = datagrams.iter().map(Vec::len).collect();
    assert_eq!(sizes, vec![12 + 7 * 188, 12 + 7 * 188, 12 + 188]);
    let seqs: Vec<u16> = datagrams
        .iter()
        .map(|datagram| u16::from_be_bytes([datagram[2], datagram[3]]))
        .collect();
    assert_eq!(seqs, vec![10, 11, 12]);
    assert_eq!(&datagrams[0][12..12 + 188], &buf[..188]);
    assert_eq!(&datagrams[2][12..], &buf[14 * 188..]);
}

#[test]
fn tcp_groups_forty_two_packets_per_rtp() {
    let buf = buffer(&[0x100; 43]);
    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::RtpTcp, &buf, |_| true, 0);
    assert_eq!(batches.len(), 1);
    let bytes: Vec<u8> = batches[0].chunks(&buf).concat();
    assert_eq!(bytes.len(), 16 + 42 * 188 + 16 + 188);
    assert_eq!(
        &bytes[0..4],
        &[b'$', 0, 0x1E, 0xE4],
        "length 42 * 188 + 12 = 7908"
    );
    let second = 16 + 42 * 188;
    assert_eq!(&bytes[second..second + 4], &[b'$', 0, 0x00, 0xC8]);
    assert_eq!(
        u16::from_be_bytes([bytes[second + 6], bytes[second + 7]]),
        1
    );
}

#[test]
fn http_is_raw_ts_without_headers() {
    let buf = buffer(&[0x100, 0x200, 0x100]);
    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::Raw, &buf, |pid| pid == 0x100, 0);
    assert_eq!(batches.len(), 1);
    assert_eq!(header_count(&batches[0]), 0);
    let bytes: Vec<u8> = batches[0].chunks(&buf).concat();
    assert_eq!(bytes, [&buf[..188], &buf[376..]].concat());
    assert_eq!(rtp.seq, 0);
}

#[test]
fn pid_filter_and_contiguous_runs() {
    let buf = buffer(&[0x100, 0x101, 0x200, 0x100, 0x101]);
    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::RtpUdp, &buf, |pid| pid != 0x200, 0);
    assert_eq!(
        batches[0].segments[1..],
        [
            Segment::Data { start: 0, len: 376 },
            Segment::Data {
                start: 564,
                len: 376
            }
        ]
    );
    let datagram = &batches[0].datagrams(&buf)[0];
    assert_eq!(datagram.len(), 12 + 4 * 188);
}

#[test]
fn non_ts_packets_are_skipped() {
    let mut buf = buffer(&[0x100, 0x100]);
    buf[0] = 0x00;
    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::RtpUdp, &buf, |_| true, 0);
    assert_eq!(batches[0].datagrams(&buf)[0].len(), 12 + 188);
}

#[test]
fn rtp_without_matching_packets_sends_bare_header() {
    let buf = buffer(&[0x200; 3]);
    let mut rtp = RtpState::new(1, 5);
    let batches = packetize(&mut rtp, Framing::RtpUdp, &buf, |pid| pid == 0x100, 0);
    let datagrams = batches[0].datagrams(&buf);
    assert_eq!(datagrams.len(), 1);
    assert_eq!(datagrams[0].len(), 12);
    assert_eq!(rtp.seq, 6);

    let batches = packetize(&mut rtp, Framing::Raw, &buf, |pid| pid == 0x100, 0);
    assert!(batches[0].is_empty(&buf));
}

#[test]
fn intermediate_flush_at_max_iov() {
    // Alternating PIDs give one iovec per packet: a header plus 42 packets uses 43 slots.
    let pids: Vec<u16> = (0..4000)
        .map(|i| if i % 2 == 0 { 0x100 } else { 0x200 })
        .collect();
    let buf = buffer(&pids);
    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::RtpTcp, &buf, |pid| pid == 0x100, 0);
    assert_eq!(batches.len(), 2);
    // The check runs only when a header is added: 24 * 43 + 1 = 1033 >= 1008 (DEV-041).
    assert_eq!(header_count(&batches[0]), 24);
    assert_eq!(batches[0].segments.len(), 24 * 43);
    assert_eq!(header_count(&batches[1]), 24);
    assert_eq!(batches[1].segments.len(), 1 + 992 + 23);
    let total: usize = batches.iter().map(|batch| batch.len(&buf)).sum();
    assert_eq!(total, 2000 * 188 + 48 * 16);
    assert_eq!(rtp.seq, 48);

    let mut rtp = RtpState::new(1, 0);
    let batches = packetize(&mut rtp, Framing::Raw, &buf, |pid| pid == 0x100, 0);
    assert_eq!(batches.len(), 2, "raw TS checks before every packet");
    assert_eq!(batches[0].segments.len(), TCP_MAX_IOV);
    assert_eq!(batches[1].segments.len(), 2000 - TCP_MAX_IOV);
}

fn cpp_rtcp(info: &RtcpInfo, desc: &[u8]) -> Vec<u8> {
    // Literal transcription of send_rtcp's byte writes for a short description.
    let la = desc.len();
    let len = (la + 16).div_ceil(4) * 4;
    let mut b = vec![0u8; len + 52];
    let ssrc = info.ssrc.to_be_bytes();
    b[..4].copy_from_slice(&[0x80, 0xC8, 0, 6]);
    b[4..8].copy_from_slice(&ssrc);
    b[8..12].copy_from_slice(&(ntp_seconds(info.wall)).to_be_bytes());
    b[12..16].copy_from_slice(&(info.ctime_ms as u32).to_be_bytes());
    b[16..20].copy_from_slice(&(info.wtime_ms as u32).to_be_bytes());
    b[20..24].copy_from_slice(&info.packets.to_be_bytes());
    b[24..28].copy_from_slice(&info.octets.to_be_bytes());
    b[28..32].copy_from_slice(&[0x81, 0xCA, 0, 5]);
    b[32..36].copy_from_slice(&ssrc);
    b[36..47].copy_from_slice(b"\x01\x0Aminisatip");
    b[52..54].copy_from_slice(&[0x80, 0xCC]);
    #[allow(clippy::manual_div_ceil)]
    let words = ((la + 16 + 3) / 4 - 1) as u16;
    b[54..56].copy_from_slice(&words.to_be_bytes());
    b[56..60].copy_from_slice(&ssrc);
    b[60..64].copy_from_slice(b"SES1");
    b[66..68].copy_from_slice(&(la as u16).to_be_bytes());
    b[68..68 + la].copy_from_slice(desc);
    b
}

fn sample_info() -> RtcpInfo {
    RtcpInfo {
        ssrc: 0x0102_0304,
        wall: UNIX_EPOCH + Duration::from_secs(1_700_000_000),
        ctime_ms: 0x1_2345_6789,
        wtime_ms: 0x1_2345_6700,
        packets: 77,
        octets: 99_999,
    }
}

#[test]
fn rtcp_bytes_match_cpp_layout() {
    let desc = "ver=1.0;src=1;tuner=1,0,0,0,0,,,,,,,;pids=none";
    assert_eq!(desc.len(), 46);
    let info = sample_info();
    let packet = build_rtcp(&info, desc, false);
    assert_eq!(packet.len(), 52 + 64);
    assert_eq!(packet, cpp_rtcp(&info, desc.as_bytes()));
    // Spot checks against hand-computed values.
    assert_eq!(
        &packet[8..12],
        &((1_700_000_000u64 + NTP_UNIX_OFFSET) as u32).to_be_bytes()
    );
    assert_eq!(&packet[12..16], &[0x23, 0x45, 0x67, 0x89]);
    assert_eq!(&packet[16..20], &[0x23, 0x45, 0x67, 0x00]);
    assert_eq!(&packet[20..24], &[0, 0, 0, 77]);
    assert_eq!(&packet[54..56], &[0, 15]);
    assert_eq!(&packet[66..68], &[0, 46]);
    assert_eq!(
        &packet[114..116],
        &[0, 0],
        "padding is zero as dad is memset"
    );
}

#[test]
fn rtcp_length_rounding() {
    let info = sample_info();
    for la in 40..48 {
        let desc = "x".repeat(la);
        let packet = build_rtcp(&info, &desc, false);
        assert_eq!(packet, cpp_rtcp(&info, desc.as_bytes()), "la={la}");
        assert_eq!(packet.len() % 4, 0);
        let words = u16::from_be_bytes([packet[54], packet[55]]) as usize + 1;
        assert_eq!(52 + words * 4, packet.len());
    }
}

#[test]
fn rtcp_interleaved_uses_channel_one() {
    let info = sample_info();
    let desc = "ver=1.0;src=1;tuner=1,0,0,0,0,,,,,,,;pids=none";
    let plain = build_rtcp(&info, desc, false);
    let framed = build_rtcp(&info, desc, true);
    assert_eq!(&framed[..4], &[b'$', 1, 0, plain.len() as u8]);
    assert_eq!(&framed[4..], &plain[..]);
}

fn dvbs2_params() -> TuneParams {
    TuneParams {
        freq: 11_362_000,
        pol: Polarization::Horizontal,
        msys: DeliverySystem::DvbS2,
        sr: 22_000_000,
        src: 1,
        fec: 2,
        mtype: 9,
        plts: Some(true),
        ro: Some(0),
        ..TuneParams::default()
    }
}

#[test]
fn describe_dvbs2() {
    let tune = dvbs2_params();
    let pids = PidSet::from_pids([0, 16, 17]);
    let input = DescribeInput {
        adapter_id: 0,
        tune: &tune,
        signal: Some(SignalStatus::locked(200, 240)),
        use_adapter: true,
        pids: &pids,
    };
    assert_eq!(
        describe_adapter(&input),
        "ver=1.0;src=1;tuner=1,200,1,15,11362,h,dvbs2,8psk,on,0.35,22000,23;pids=0,16,17"
    );
}

#[test]
fn rtcp_ses1_description_uses_khz_frequency() {
    let mut tune = dvbs2_params();
    tune.freq = 11_361_750;
    let pids = PidSet::from_pids([0, 16]);
    let input = DescribeInput {
        adapter_id: 0,
        tune: &tune,
        signal: Some(SignalStatus::locked(200, 240)),
        use_adapter: true,
        pids: &pids,
    };
    let desc = describe_adapter(&input);
    assert_eq!(
        desc,
        "ver=1.0;src=1;tuner=1,200,1,15,11361,h,dvbs2,8psk,on,0.35,22000,23;pids=0,16"
    );

    let packet = build_rtcp(&sample_info(), &desc, false);
    assert_eq!(
        usize::from(u16::from_be_bytes([packet[66], packet[67]])),
        desc.len()
    );
    assert_eq!(&packet[68..68 + desc.len()], desc.as_bytes());
}

#[test]
fn describe_signal_defaults_and_clamps() {
    let tune = dvbs2_params();
    let pids = PidSet::new();
    let mut input = DescribeInput {
        adapter_id: 2,
        tune: &tune,
        signal: None,
        use_adapter: true,
        pids: &pids,
    };
    assert!(describe_adapter(&input).starts_with("ver=1.0;src=1;tuner=3,255,1,15,"));
    assert!(describe_adapter(&input).ends_with(";pids=none"));
    let unlocked = SignalStatus {
        locked: false,
        strength: 300,
        snr: 10,
        ber: 0,
    };
    input.signal = Some(unlocked);
    assert!(describe_adapter(&input).starts_with("ver=1.0;src=1;tuner=3,1,0,10,"));
}

#[test]
fn describe_undefined_and_stream_pids() {
    let tune = TuneParams {
        msys: DeliverySystem::Undefined,
        pids: PidSet::all(),
        ..TuneParams::default()
    };
    let input = DescribeInput {
        adapter_id: 0,
        tune: &tune,
        signal: None,
        use_adapter: false,
        pids: &tune.pids,
    };
    assert_eq!(
        describe_adapter(&input),
        "ver=1.0;src=1;tuner=1,0,0,0,0,,,,,,,;pids=all"
    );
}

// Ported from tests/test_adapter.cpp:149
#[test]
fn test_describe_adapter_large_plp_ds() {
    let pids = PidSet::new();
    let tune = TuneParams {
        msys: DeliverySystem::DvbT2,
        plp: Some(2_147_483_647),
        ..TuneParams::default()
    };
    let input = DescribeInput {
        adapter_id: 0,
        tune: &tune,
        signal: None,
        use_adapter: false,
        pids: &pids,
    };
    assert!(
        describe_adapter(&input).contains(",2147483647,"),
        "DVB-T2 plp printed"
    );

    let tune = TuneParams {
        msys: DeliverySystem::DvbC2,
        ds: Some(2_147_483_646),
        plp: Some(2_147_483_647),
        ..TuneParams::default()
    };
    let input = DescribeInput {
        adapter_id: 0,
        tune: &tune,
        signal: None,
        use_adapter: false,
        pids: &pids,
    };
    assert!(
        describe_adapter(&input).contains(",2147483646,2147483647,"),
        "DVB-C2 ds and plp printed"
    );
}

#[test]
fn describe_dvbt_and_dvbc() {
    let tune = TuneParams {
        freq: 506_000,
        msys: DeliverySystem::DvbT2,
        mtype: 5,
        bw: Some(8_000_000),
        tmode: Some(5),
        gi: Some(0),
        fec: 2,
        plp: Some(0),
        ..TuneParams::default()
    };
    let pids = PidSet::from_pids([0, PID_ALL]);
    let input = DescribeInput {
        adapter_id: 1,
        tune: &tune,
        signal: Some(SignalStatus::locked(255, 15)),
        use_adapter: true,
        pids: &pids,
    };
    assert_eq!(
        describe_adapter(&input),
        "ver=1.1;tuner=2,255,1,15,506.00,8,dvbt2,16k,256qam,132,23,0,0,0;pids=0,all"
    );

    let tune = TuneParams {
        freq: 346_000,
        msys: DeliverySystem::DvbC,
        mtype: 3,
        sr: 6_900_000,
        specinv: Some(0),
        ..TuneParams::default()
    };
    let input = DescribeInput {
        tune: &tune,
        ..input
    };
    assert_eq!(
        describe_adapter(&input),
        "ver=1.2;tuner=2,255,1,15,346.00,8,dvbc,64qam,6900,0,,,0;pids=0,all"
    );
}

#[test]
fn pid_lists() {
    assert_eq!(pid_list(&PidSet::new()), "none");
    assert_eq!(pid_list(&PidSet::from_pids([5, 1])), "1,5");
    assert_eq!(pid_list(&PidSet::from_pids([5, PID_ALL])), "all");
    assert_eq!(adapter_pid_list(&PidSet::from_pids([5, PID_ALL])), "5,all");
}
