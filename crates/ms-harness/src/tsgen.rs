//! Deterministic TS capture generator (PAT/PMT/SDT + indexed ES packets).
//!
//! Every ES packet carries `MSRS`, its file index and its PID so the analysis
//! can find one full replay period in any received window.

/// TS packet size in bytes.
pub const TS: usize = 188;
/// Marker at the start of every generated ES payload.
pub const MAGIC: &[u8; 4] = b"MSRS";
/// Transport stream id of generated muxes.
pub const TSID: u16 = 0x0457;
const ONID: u16 = 0x0001;
const CYCLES: usize = 32;

/// Names of the built-in capture profiles.
pub const PROFILES: &[&str] = &["fta", "multi", "pmtver"];

struct Service {
    number: u16,
    pmt_pid: u16,
    pcr_pid: u16,
    name: &'static str,
    /// (stream_type, pid, packets per cycle)
    streams: Vec<(u8, u16, usize)>,
}

/// Generates the capture for `profile`, or `None` for an unknown name.
pub fn generate(profile: &str) -> Option<Vec<u8>> {
    match profile {
        "fta" => Some(build(&[fta_service()], &[], false)),
        "multi" => Some(build(&multi_services(), &[(0x1ff, 1), (0x1fff, 1)], false)),
        "pmtver" => Some(build(&[fta_service()], &[], true)),
        _ => None,
    }
}

fn fta_service() -> Service {
    Service {
        number: 1,
        pmt_pid: 0x100,
        pcr_pid: 0x101,
        name: "Harness One",
        streams: vec![(0x02, 0x101, 4), (0x03, 0x102, 1)],
    }
}

fn multi_services() -> Vec<Service> {
    (1..=3u16)
        .map(|n| Service {
            number: n,
            pmt_pid: 0x100 * n,
            pcr_pid: 0x100 * n + 1,
            name: ["Harness One", "Harness Two", "Harness Three"][usize::from(n - 1)],
            streams: vec![(0x02, 0x100 * n + 1, 2), (0x03, 0x100 * n + 2, 1)],
        })
        .collect()
}

/// Lays out `CYCLES` cycles; every PID's packet count is a multiple of 16 so
/// continuity counters stay continuous when the replay loops.
fn build(services: &[Service], extra: &[(u16, usize)], pmt_change: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cc = [0u8; 8192];
    let mut index = 0u32;
    let pat = pat_section(services);
    let sdt = sdt_section(services);
    for cycle in 0..CYCLES {
        let second_half = pmt_change && cycle >= CYCLES / 2;
        push_psi(&mut out, &mut cc, &mut index, 0, &pat);
        for service in services {
            let section = if second_half {
                let mut changed = service.streams.clone();
                changed.push((0x03, service.pcr_pid + 2, 1));
                pmt_section(service, &changed, 1)
            } else {
                pmt_section(service, &service.streams, 0)
            };
            push_psi(&mut out, &mut cc, &mut index, service.pmt_pid, &section);
        }
        if cycle % 2 == 0 {
            push_psi(&mut out, &mut cc, &mut index, 0x11, &sdt);
        }
        for service in services {
            let mut streams = service.streams.clone();
            if second_half {
                streams.push((0x03, service.pcr_pid + 2, 1));
            }
            for (_, pid, per_cycle) in streams {
                for _ in 0..per_cycle {
                    push_es(&mut out, &mut cc, &mut index, pid);
                }
            }
        }
        for &(pid, per_cycle) in extra {
            for _ in 0..per_cycle {
                push_es(&mut out, &mut cc, &mut index, pid);
            }
        }
    }
    out
}

fn header(pid: u16, pusi: bool, cc: &mut [u8; 8192]) -> [u8; 4] {
    let counter = cc[usize::from(pid)];
    cc[usize::from(pid)] = (counter + 1) & 0x0f;
    [
        0x47,
        (if pusi { 0x40 } else { 0 }) | ((pid >> 8) as u8 & 0x1f),
        pid as u8,
        0x10 | counter,
    ]
}

fn push_psi(out: &mut Vec<u8>, cc: &mut [u8; 8192], index: &mut u32, pid: u16, section: &[u8]) {
    let mut packet = [0xffu8; TS];
    packet[..4].copy_from_slice(&header(pid, true, cc));
    packet[4] = 0;
    packet[5..5 + section.len()].copy_from_slice(section);
    out.extend_from_slice(&packet);
    *index += 1;
}

fn push_es(out: &mut Vec<u8>, cc: &mut [u8; 8192], index: &mut u32, pid: u16) {
    let mut packet = [0u8; TS];
    packet[..4].copy_from_slice(&header(pid, false, cc));
    packet[4..8].copy_from_slice(MAGIC);
    packet[8..12].copy_from_slice(&index.to_be_bytes());
    packet[12..14].copy_from_slice(&pid.to_be_bytes());
    for (offset, byte) in packet[14..].iter_mut().enumerate() {
        *byte = (*index as usize + offset) as u8;
    }
    out.extend_from_slice(&packet);
    *index += 1;
}

/// Builds a long-form PSI section with CRC from table id, extension, version and body.
pub fn section(table_id: u8, ext: u16, version: u8, body: &[u8]) -> Vec<u8> {
    let length = 5 + body.len() + 4;
    let mut s = vec![
        table_id,
        0xb0 | ((length >> 8) as u8 & 0x0f),
        length as u8,
        (ext >> 8) as u8,
        ext as u8,
        0xc1 | ((version & 0x1f) << 1),
        0,
        0,
    ];
    s.extend_from_slice(body);
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

fn pat_section(services: &[Service]) -> Vec<u8> {
    let mut body = Vec::new();
    for service in services {
        body.extend_from_slice(&service.number.to_be_bytes());
        body.extend_from_slice(&(0xe000 | service.pmt_pid).to_be_bytes());
    }
    section(0x00, TSID, 0, &body)
}

fn pmt_section(service: &Service, streams: &[(u8, u16, usize)], version: u8) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&(0xe000 | service.pcr_pid).to_be_bytes());
    body.extend_from_slice(&0xf000u16.to_be_bytes());
    for &(stream_type, pid, _) in streams {
        body.push(stream_type);
        body.extend_from_slice(&(0xe000 | pid).to_be_bytes());
        body.extend_from_slice(&0xf000u16.to_be_bytes());
    }
    section(0x02, service.number, version, &body)
}

fn sdt_section(services: &[Service]) -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&ONID.to_be_bytes());
    body.push(0xff);
    for service in services {
        let provider = b"Harness";
        let mut descriptor = vec![0x48, 0, 0x01, provider.len() as u8];
        descriptor.extend_from_slice(provider);
        descriptor.push(service.name.len() as u8);
        descriptor.extend_from_slice(service.name.as_bytes());
        descriptor[1] = (descriptor.len() - 2) as u8;
        body.extend_from_slice(&service.number.to_be_bytes());
        body.push(0xfc);
        let loop_len = descriptor.len() as u16;
        // running_status 4 (running), free_CA_mode 0.
        body.extend_from_slice(&(0x8000 | loop_len).to_be_bytes());
        body.extend_from_slice(&descriptor);
    }
    section(0x42, TSID, 0, &body)
}

/// MPEG-2 CRC32 (polynomial 0x04C11DB7, no reflection).
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xffff_ffffu32;
    for &byte in data {
        crc ^= u32::from(byte) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pid(packet: &[u8]) -> u16 {
        (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
    }

    #[test]
    fn crc_of_section_with_crc_is_zero() {
        let s = section(0x00, TSID, 0, &[0, 1, 0xe1, 0]);
        assert_eq!(crc32(&s), 0);
    }

    #[test]
    fn profiles_are_deterministic_and_loop_continuous() {
        for name in PROFILES {
            let first = generate(name).expect("profile");
            assert_eq!(first, generate(name).expect("profile"));
            assert_eq!(first.len() % TS, 0);
            let mut counts = std::collections::HashMap::new();
            for packet in first.chunks(TS) {
                assert_eq!(packet[0], 0x47);
                *counts.entry(pid(packet)).or_insert(0usize) += 1;
            }
            for (pid, count) in counts {
                assert_eq!(count % 16, 0, "{name}: pid {pid:#x} count {count}");
            }
        }
    }

    #[test]
    fn es_packets_carry_their_index() {
        let data = generate("fta").expect("fta");
        for (index, packet) in data.chunks(TS).enumerate() {
            if &packet[4..8] == MAGIC {
                let stored = u32::from_be_bytes(packet[8..12].try_into().expect("4 bytes"));
                assert_eq!(stored as usize, index);
            }
        }
    }
}
