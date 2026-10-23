//! Output stream summaries: PID set, packet counts, PSI sections, CC continuity.
//!
//! Counts cover exactly one replay period (between two arrivals of the lowest
//! ES index seen), so they do not depend on when the capture started. Recorded
//! files have no index; their `sets` summaries report only what arrived.

use std::collections::{BTreeMap, BTreeSet};

use crate::tsgen::{MAGIC, TS, crc32};

/// Captured data from one output.
pub enum Units<'a> {
    /// RTP datagrams or interleaved frames.
    Rtp(&'a [Vec<u8>]),
    /// Raw TS bytes (HTTP streaming), starting at a packet boundary.
    Ts(&'a [u8]),
}

/// CC transitions a replayed file makes by itself: the loop seam of every PID
/// and the discontinuities already recorded in the file.
#[derive(Debug, Clone, Default)]
pub struct Seams(BTreeSet<(u16, u8, u8)>);

impl Seams {
    pub fn of(file: &[u8]) -> Self {
        let mut set = BTreeSet::new();
        let mut first = BTreeMap::new();
        let mut last = BTreeMap::new();
        for packet in file.as_chunks::<TS>().0 {
            let Some((pid, cc)) = cc_of(packet) else {
                continue;
            };
            first.entry(pid).or_insert(cc);
            if let Some(previous) = last.insert(pid, cc)
                && (previous + 1) & 0x0f != cc
            {
                set.insert((pid, previous, cc));
            }
        }
        for (pid, previous) in last {
            set.insert((pid, previous, first[&pid]));
        }
        Self(set)
    }
}

/// PID and continuity counter of a packet that carries a payload.
fn cc_of(packet: &[u8]) -> Option<(u16, u8)> {
    let pid = pid(packet);
    (packet[0] == 0x47 && pid != 0x1fff && payload(packet).is_some())
        .then_some((pid, packet[3] & 0x0f))
}

/// CC discontinuities, ignoring the transitions listed in `seams`; a real loss
/// that happens to produce a seam's (pid, last, cc) triple is not counted.
fn cc_errors(packets: &[&[u8]], seams: &Seams) -> usize {
    let mut errors = 0;
    let mut last_cc: BTreeMap<u16, u8> = BTreeMap::new();
    for packet in packets {
        let Some((pid, cc)) = cc_of(packet) else {
            continue;
        };
        if let Some(last) = last_cc.insert(pid, cc)
            && (last + 1) & 0x0f != cc
            && !seams.0.contains(&(pid, last, cc))
        {
            errors += 1;
        }
    }
    errors
}

fn some(count: usize) -> &'static str {
    if count == 0 { "none" } else { "some" }
}

/// Extracts TS packets and summarises RTP framing; `exact` keeps the counts.
fn packets<'a>(units: &Units<'a>, lines: &mut Vec<String>, exact: bool) -> Vec<&'a [u8]> {
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
            let (gaps, bad_payload) = if exact {
                (gaps.to_string(), bad_payload.to_string())
            } else {
                (some(gaps).to_owned(), some(bad_payload).to_owned())
            };
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
    let packets = packets(units, &mut Vec::new(), true);
    period(&packets).is_some()
        || (packets.len() >= budget && !packets.iter().any(|p| es_index(p).is_some()))
}

/// Normalised summary lines for the captured units.
pub fn summarize(units: &Units<'_>) -> Vec<String> {
    let mut lines = Vec::new();
    let packets = packets(units, &mut lines, true);
    let sync_errors = packets.iter().filter(|packet| packet[0] != 0x47).count();
    let good: Vec<&[u8]> = packets.iter().copied().filter(|p| p[0] == 0x47).collect();
    let cc_errors = cc_errors(&good, &Seams::default());

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
    lines.extend(sections(window, true));
    lines
}

/// Summary of a recorded-file capture covering at least one full loop: which
/// PIDs, scrambling states and sections arrived, never how many.
pub fn summarize_sets(units: &Units<'_>, seams: &Seams) -> Vec<String> {
    let mut lines = Vec::new();
    let packets = packets(units, &mut lines, false);
    let sync_errors = packets.iter().filter(|packet| packet[0] != 0x47).count();
    let good: Vec<&[u8]> = packets.iter().copied().filter(|p| p[0] == 0x47).collect();
    lines.push(format!(
        "ts: mode=sets packets={} sync_errors={} cc_errors={}",
        some(good.len()),
        some(sync_errors),
        some(cc_errors(&good, seams)),
    ));
    let mut states: BTreeMap<u16, (bool, bool)> = BTreeMap::new();
    for packet in &good {
        let state = states.entry(pid(packet)).or_default();
        if packet[3] & 0xc0 != 0 {
            state.1 = true;
        } else {
            state.0 = true;
        }
    }
    for (pid, state) in states {
        let state = match state {
            (true, false) => "clear",
            (false, true) => "scrambled",
            _ => "mixed",
        };
        lines.push(format!("pid {pid:#06x} {state}"));
    }
    lines.extend(sections(&good, false));
    lines
}

/// Distinct PSI sections on PID 0, 0x11 and the PMT PIDs listed in any PAT.
fn sections(window: &[&[u8]], counts: bool) -> Vec<String> {
    let mut psi_pids: BTreeSet<u16> = [0u16, 0x11].into_iter().collect();
    let mut buffers: BTreeMap<u16, Vec<u8>> = BTreeMap::new();
    let mut last_cc: BTreeMap<u16, u8> = BTreeMap::new();
    let mut found: BTreeMap<(u16, Vec<u8>), usize> = BTreeMap::new();
    for packet in window {
        let pid = pid(packet);
        if !psi_pids.contains(&pid) {
            continue;
        }
        let Some(data) = payload(packet) else {
            continue;
        };
        let cc = packet[3] & 0x0f;
        let broken = last_cc
            .insert(pid, cc)
            .is_some_and(|last| (last + 1) & 0x0f != cc);
        let buffer = buffers.entry(pid).or_default();
        // A section cut by a CC break (loop seam, loss) cannot be completed.
        if broken {
            buffer.clear();
        }
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
                "section pid={pid:#06x} table={:#04x} ext={ext:#06x} v={version} len={} crc={crc}{}{}",
                section[0],
                section.len(),
                if crc_ok { "" } else { " BAD-CRC" },
                if counts { format!(" count={count}") } else { String::new() },
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

    /// A recorded-like file: no ES index, and a length that breaks CC at the seam.
    fn recorded() -> Vec<u8> {
        let mut file: Vec<u8> = generate("multi").expect("multi")[..TS * 333].to_vec();
        for packet in file.as_chunks_mut::<TS>().0 {
            if let Some(start) = payload(packet).map(|data| TS - data.len())
                && packet[start..].starts_with(MAGIC)
            {
                packet[start..].fill(0xaa);
            }
        }
        file
    }

    fn looped(file: &[u8], skip: usize, loops: usize) -> Vec<u8> {
        let mut out = file[skip * TS..].to_vec();
        for _ in 0..loops {
            out.extend_from_slice(file);
        }
        out
    }

    #[test]
    fn sets_summary_ignores_start_offset_and_loop_seams() {
        let file = recorded();
        let seams = Seams::of(&file);
        assert!(!seams.0.is_empty(), "the cut length breaks CC at the seam");
        let a = summarize_sets(&Units::Ts(&looped(&file, 3, 1)), &seams);
        let b = summarize_sets(&Units::Ts(&looped(&file, 200, 2)), &seams);
        assert_eq!(a, b);
        assert_eq!(
            a[0],
            "ts: mode=sets packets=some sync_errors=none cc_errors=none"
        );
        assert!(
            a.iter().any(|line| line.starts_with("pid 0x0101 clear")),
            "{a:?}"
        );
        assert!(
            a.iter()
                .all(|line| !line.contains("count=") && !line.contains("BAD-CRC"))
        );
        // The seam set only exempts the file's own transitions.
        let mut lossy = looped(&file, 3, 1);
        lossy.drain(TS * 100..TS * 101);
        assert!(summarize_sets(&Units::Ts(&lossy), &seams)[0].contains("cc_errors=some"));
        assert!(!summarize(&Units::Ts(&looped(&file, 3, 1)))[0].contains("cc_errors=0"));
    }

    #[test]
    fn a_cc_break_drops_the_partial_section() {
        let body = vec![0x5a; 300];
        let section = crate::tsgen::section(0x42, 1, 0, &body);
        let packet = |pusi: bool, cc: u8, data: &[u8]| {
            let mut p = vec![0x47, if pusi { 0x40 } else { 0 }, 0x11, 0x10 | cc];
            if pusi {
                p.push(0);
            }
            p.extend_from_slice(data);
            p.resize(TS, 0xff);
            p
        };
        let head = &section[..183];
        let tail = &section[183..];
        let mut stream = Vec::new();
        stream.extend(packet(true, 0, head));
        stream.extend(packet(false, 5, &[0x11; 184]));
        stream.extend(packet(true, 6, head));
        stream.extend(packet(false, 7, tail));
        let lines = summarize_sets(&Units::Ts(&stream), &Seams::default());
        let sections: Vec<_> = lines.iter().filter(|l| l.starts_with("section")).collect();
        assert_eq!(sections.len(), 1, "{lines:?}");
        assert!(!sections[0].contains("BAD-CRC"), "{lines:?}");
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
