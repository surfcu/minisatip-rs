//! Output stream summaries: PID set, packet counts, PSI sections, CC continuity.
//!
//! Counts cover exactly one replay period (between two arrivals of the lowest
//! ES index seen), so they do not depend on when the capture started.

use std::collections::{BTreeMap, BTreeSet};

use crate::tsgen::{MAGIC, TS, crc32};

/// Captured data from one output.
pub enum Units<'a> {
    /// RTP datagrams or interleaved frames.
    Rtp(&'a [Vec<u8>]),
    /// Raw TS bytes (HTTP streaming), starting at a packet boundary.
    Ts(&'a [u8]),
}

/// Extracts TS packets and summarises RTP framing.
fn packets<'a>(units: &Units<'a>, lines: &mut Vec<String>) -> Vec<&'a [u8]> {
    match units {
        Units::Ts(bytes) => bytes.as_chunks::<TS>().0.iter().map(|c| &c[..]).collect(),
        Units::Rtp(datagrams) => {
            let mut out = Vec::new();
            let mut versions = BTreeSet::new();
            let mut types = BTreeSet::new();
            let mut ssrcs = BTreeSet::new();
            let mut gaps = 0usize;
            let mut bad_payload = 0usize;
            let mut last_seq: Option<u16> = None;
            for datagram in *datagrams {
                if datagram.len() < 12 {
                    bad_payload += 1;
                    continue;
                }
                versions.insert(datagram[0] >> 6);
                types.insert(datagram[1] & 0x7f);
                ssrcs.insert(u32::from_be_bytes([
                    datagram[8],
                    datagram[9],
                    datagram[10],
                    datagram[11],
                ]));
                let seq = u16::from_be_bytes([datagram[2], datagram[3]]);
                if last_seq.is_some_and(|last| last.wrapping_add(1) != seq) {
                    gaps += 1;
                }
                last_seq = Some(seq);
                let header = 12 + 4 * usize::from(datagram[0] & 0x0f);
                let payload = datagram.get(header..).unwrap_or_default();
                if payload.len() % TS != 0 {
                    bad_payload += 1;
                }
                out.extend(payload.as_chunks::<TS>().0.iter().map(|c| &c[..]));
            }
            if datagrams.is_empty() {
                lines.push("rtp: none".to_owned());
            } else {
                lines.push(format!(
                    "rtp: version={} pt={} ssrc={} seq_gaps={gaps} bad_payload={bad_payload}",
                    join(&versions),
                    join(&types),
                    if ssrcs.len() == 1 {
                        "constant"
                    } else {
                        "varying"
                    },
                ));
            }
            out
        }
    }
}

fn join<T: std::fmt::Display>(set: &BTreeSet<T>) -> String {
    let items: Vec<String> = set.iter().map(ToString::to_string).collect();
    items.join(",")
}

fn pid(packet: &[u8]) -> u16 {
    (u16::from(packet[1] & 0x1f) << 8) | u16::from(packet[2])
}

fn payload(packet: &[u8]) -> Option<&[u8]> {
    let afc = (packet[3] >> 4) & 3;
    if afc & 1 == 0 {
        return None;
    }
    let start = if afc == 3 {
        5 + usize::from(packet[4])
    } else {
        4
    };
    packet.get(start..)
}

fn es_index(packet: &[u8]) -> Option<u32> {
    let data = payload(packet)?;
    if data.len() >= 8 && &data[..4] == MAGIC {
        Some(u32::from_be_bytes([data[4], data[5], data[6], data[7]]))
    } else {
        None
    }
}

/// Section carried by a PSI packet; identical bytes repeat every replay cycle.
fn psi_section(packet: &[u8]) -> Option<Vec<u8>> {
    if packet[0] != 0x47 || packet[1] & 0x40 == 0 {
        return None;
    }
    let data = payload(packet)?;
    let pointer = usize::from(*data.first()?);
    let section = data.get(1 + pointer..)?;
    (section.len() >= 8).then(|| section.to_vec())
}

/// Start and end of the first complete replay period, if one was received.
fn period(packets: &[&[u8]]) -> Option<(usize, usize)> {
    let indexed: Vec<(usize, u32)> = packets
        .iter()
        .enumerate()
        .filter(|(_, packet)| packet[0] == 0x47)
        .filter_map(|(position, packet)| es_index(packet).map(|index| (position, index)))
        .collect();
    if let Some(anchor) = indexed.iter().map(|(_, index)| *index).min() {
        return span(&indexed, |index| *index == anchor);
    }
    // PID-filtered captures may carry only PSI: anchor on the section with the
    // longest repeat, so the window covers whole cycles whatever the start.
    let sections: Vec<(usize, Vec<u8>)> = packets
        .iter()
        .enumerate()
        .filter_map(|(position, packet)| psi_section(packet).map(|key| (position, key)))
        .collect();
    let mut best: Option<(&Vec<u8>, usize, usize)> = None;
    for (_, key) in &sections {
        let mut hits = sections
            .iter()
            .filter(|(_, other)| other == key)
            .map(|(position, _)| *position);
        let (Some(start), Some(end)) = (hits.next(), hits.next()) else {
            continue;
        };
        let longer = match best {
            None => true,
            Some((best_key, best_start, best_end)) => {
                end - start > best_end - best_start
                    || (end - start == best_end - best_start && key < best_key)
            }
        };
        if longer {
            best = Some((key, start, end));
        }
    }
    best.map(|(_, start, end)| (start, end))
}

fn span<T>(hits: &[(usize, T)], matches: impl Fn(&T) -> bool) -> Option<(usize, usize)> {
    let mut hits = hits.iter().filter(|(_, item)| matches(item));
    let start = hits.next()?.0;
    let end = hits.next()?.0;
    Some((start, end))
}

/// True when the units hold one full period, or `budget` packets without ES indexes.
pub fn complete(units: &Units<'_>, budget: usize) -> bool {
    let packets = packets(units, &mut Vec::new());
    period(&packets).is_some()
        || (packets.len() >= budget && !packets.iter().any(|p| es_index(p).is_some()))
}

/// Normalised summary lines for the captured units.
pub fn summarize(units: &Units<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    let packets = packets(units, &mut lines);
    let sync_errors = packets.iter().filter(|packet| packet[0] != 0x47).count();
    let good: Vec<&[u8]> = packets.iter().copied().filter(|p| p[0] == 0x47).collect();

    let mut cc_errors = 0usize;
    let mut last_cc: BTreeMap<u16, u8> = BTreeMap::new();
    for packet in &good {
        let pid = pid(packet);
        if pid == 0x1fff || payload(packet).is_none() {
            continue;
        }
        let cc = packet[3] & 0x0f;
        if let Some(last) = last_cc.insert(pid, cc)
            && (last + 1) & 0x0f != cc
        {
            cc_errors += 1;
        }
    }

    let (window, state) = match period(&good) {
        Some((start, end)) => (&good[start..end], "complete"),
        None => (&good[..], "incomplete"),
    };
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for packet in window {
        for byte in pid(packet).to_be_bytes() {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x0100_0000_01b3);
        }
    }
    lines.push(format!(
        "ts: period={state} packets={} sync_errors={sync_errors} cc_errors={cc_errors} order={:016x}",
        window.len(),
        if window.is_empty() { 0 } else { hash }
    ));

    let mut per_pid: BTreeMap<u16, (usize, usize)> = BTreeMap::new();
    for packet in window {
        let entry = per_pid.entry(pid(packet)).or_default();
        entry.0 += 1;
        if packet[3] & 0xc0 != 0 {
            entry.1 += 1;
        }
    }
    for (pid, (count, scrambled)) in &per_pid {
        lines.push(format!(
            "pid {pid:#06x} packets={count} scrambled={scrambled}"
        ));
    }
    for line in sections(window) {
        lines.push(line);
    }
    lines
}

/// Distinct PSI sections on PID 0, 0x11 and the PMT PIDs listed in any PAT.
fn sections(window: &[&[u8]]) -> Vec<String> {
    let mut psi_pids: BTreeSet<u16> = [0u16, 0x11].into_iter().collect();
    let mut buffers: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    let mut found: BTreeMap<(u16, Vec<u8>), usize> = BTreeMap::new();
    for packet in window {
        let pid = pid(packet);
        if !psi_pids.contains(&pid) {
            continue;
        }
        let Some(data) = payload(packet) else {
            continue;
        };
        let buffer = buffers.entry(pid).or_default();
        if packet[1] & 0x40 != 0 {
            let pointer = usize::from(*data.first().unwrap_or(&0));
            buffer.clear();
            buffer.extend_from_slice(data.get(1 + pointer..).unwrap_or_default());
        } else if !buffer.is_empty() {
            buffer.extend_from_slice(data);
        }
        if buffer.len() >= 3 && buffer[0] != 0xff {
            let length = 3 + ((usize::from(buffer[1] & 0x0f) << 8) | usize::from(buffer[2]));
            if buffer.len() >= length {
                let section = buffer[..length].to_vec();
                buffer.clear();
                if section[0] == 0 && section.len() >= 12 {
                    for entry in section[8..section.len() - 4].as_chunks::<4>().0 {
                        let program = u16::from_be_bytes([entry[0], entry[1]]);
                        if program != 0 {
                            psi_pids.insert(u16::from_be_bytes([entry[2], entry[3]]) & 0x1fff);
                        }
                    }
                }
                *found.entry((pid, section)).or_default() += 1;
            }
        }
    }
    found
        .into_iter()
        .map(|((pid, section), count)| {
            let ext = section
                .get(3..5)
                .map_or(0, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
            let version = section.get(5).map_or(0, |byte| (byte >> 1) & 0x1f);
            let crc_ok = section.len() >= 4 && crc32(&section) == 0;
            let crc = section
                .get(section.len().saturating_sub(4)..)
                .map(|bytes| bytes.iter().map(|b| format!("{b:02x}")).collect::<String>())
                .unwrap_or_default();
            format!(
                "section pid={pid:#06x} table={:#04x} ext={ext:#06x} v={version} len={} crc={crc}{} count={count}",
                section[0],
                section.len(),
                if crc_ok { "" } else { " BAD-CRC" },
            )
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tsgen::generate;

    fn rtp(seq: u16, payload: &[u8]) -> Vec<u8> {
        let mut datagram = vec![0x80, 33];
        datagram.extend_from_slice(&seq.to_be_bytes());
        datagram.extend_from_slice(&[0, 0, 0, 0, 1, 2, 3, 4]);
        datagram.extend_from_slice(payload);
        datagram
    }

    #[test]
    fn summary_is_independent_of_start_offset() {
        let file = generate("pmtver").expect("profile");
        let mut first = file[188 * 7..].to_vec();
        first.extend_from_slice(&file);
        first.extend_from_slice(&file[..188 * 30]);
        let mut second = file[188 * 111..].to_vec();
        second.extend_from_slice(&file);
        second.extend_from_slice(&file[..188 * 200]);
        let a = summarize(&Units::Ts(&first));
        let b = summarize(&Units::Ts(&second));
        assert_eq!(a, b);
        assert!(a[0].contains("period=complete"), "{a:?}");
        assert!(
            a.iter()
                .any(|line| line.contains("table=0x02") && line.contains("v=1"))
        );
        assert!(a.iter().all(|line| !line.contains("BAD-CRC")));
        assert!(a[0].contains("cc_errors=0"), "{a:?}");
    }

    #[test]
    fn psi_only_capture_period_is_start_independent() {
        let file = generate("fta").expect("fta");
        let psi: Vec<u8> = file
            .chunks(TS)
            .filter(|packet| matches!(pid(packet), 0 | 0x11 | 0x100))
            .flatten()
            .copied()
            .collect();
        let shifted = |skip: usize| {
            let mut out = psi[skip * TS..].to_vec();
            out.extend_from_slice(&psi);
            out.extend_from_slice(&psi[..skip * TS]);
            out
        };
        let a = summarize(&Units::Ts(&shifted(1)));
        let b = summarize(&Units::Ts(&shifted(5)));
        assert_eq!(a, b);
        assert!(a[0].contains("period=complete"), "{a:?}");
        assert!(a.iter().any(|line| line.contains("table=0x02")), "{a:?}");
    }

    #[test]
    fn rtp_gaps_and_cc_errors_are_counted() {
        let file = generate("fta").expect("profile");
        let mut datagrams = Vec::new();
        for (seq, chunk) in file.chunks(188 * 7).enumerate() {
            if seq != 3 {
                datagrams.push(rtp(seq as u16, chunk));
            }
        }
        let lines = summarize(&Units::Rtp(&datagrams));
        assert!(lines[0].contains("seq_gaps=1"), "{lines:?}");
        assert!(!lines[1].contains("cc_errors=0"), "{lines:?}");
        assert!(complete(&Units::Rtp(&datagrams), 10_000) || lines[1].contains("incomplete"));
    }
}
