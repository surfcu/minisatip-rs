//! PSI section reassembly and scrambled-parity tracking.

use crate::crc::crc32_mpeg2;
use crate::packet::{TS_PACKET_SIZE, TsPacket};

/// `DEFAULT_LOG` of the `pmt.cpp` sites ported here (`#define DEFAULT_LOG LOG_PMT`).
const DEFAULT_LOG: u32 = ms_log::LOG_PMT;

/// Maximum bytes buffered for one section, matching the C++ filter buffer.
pub const MAX_SECTION_SIZE: usize = 4000;

const PID_INIT: u8 = 1;
const PID_SET: u8 = 2;
const PID_PARITY_CHANGE: u8 = 0x80;

/// Reassembles PSI sections for a single PID.
pub struct SectionAssembler {
    check_crc: bool,
    buf: [u8; MAX_SECTION_SIZE],
    pos: usize,
    expected: usize,
    synced: bool,
    last_len: usize,
}

impl SectionAssembler {
    /// Creates an assembler, optionally validating the trailing CRC.
    pub const fn new(check_crc: bool) -> Self {
        Self {
            check_crc,
            buf: [0; MAX_SECTION_SIZE],
            pos: 0,
            expected: 0,
            synced: false,
            last_len: 0,
        }
    }

    /// Clears all section and synchronisation state.
    pub fn reset(&mut self) {
        self.clear_section();
        self.synced = false;
    }

    /// Bytes of the section returned by the last `assemble_packet` call.
    pub fn data(&self) -> &[u8] {
        &self.buf[..self.last_len]
    }

    /// Returns whether a complete section is currently buffered.
    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Feeds one packet and returns the first completed section, like the C++ helper.
    pub fn assemble_packet(&mut self, packet: &[u8]) -> Option<&[u8]> {
        let ts = TsPacket::from_bytes(packet)?;
        if ts.pusi() {
            self.clear_section();
            self.synced = true;
        } else if !self.synced {
            return None;
        }

        let payload = ts.payload()?;
        let start = if ts.pusi() {
            let pointer = *payload.first()? as usize;
            1 + pointer
        } else {
            0
        };
        let bytes = payload.get(start..)?;
        if bytes.len() < 4 || self.pos + bytes.len() > MAX_SECTION_SIZE {
            ms_log::log_and_return!(
                None,
                "assemble_packet: len {} not valid for pid {} [{:02X} {:02X} {:02X} {:02X} {:02X} {:02X}]",
                self.pos,
                ts.pid(),
                packet[0],
                packet[1],
                packet[2],
                packet[3],
                packet[4],
                packet[5]
            );
        }
        self.buf[self.pos..self.pos + bytes.len()].copy_from_slice(bytes);
        self.pos += bytes.len();

        if self.expected == 0 {
            if self.pos < 3 {
                return None;
            }
            self.expected = 3 + (((self.buf[1] & 0x0F) as usize) << 8) + self.buf[2] as usize;
            if self.expected > MAX_SECTION_SIZE {
                return None;
            }
        }
        if self.pos < self.expected {
            return None;
        }
        if self.check_crc && !crc_ok(&self.buf, self.expected) {
            // The C++ `len < 4` branch logs the filter flags, which the
            // assembler does not keep (DEV-251); only the CRC mismatch is
            // reproduced here.
            if self.expected >= 4 {
                let len = self.expected;
                let crc = crc32_mpeg2(&self.buf[..len - 4]);
                let current = u32::from_be_bytes([
                    self.buf[len - 4],
                    self.buf[len - 3],
                    self.buf[len - 2],
                    self.buf[len - 1],
                ]);
                ms_log::log!(
                    "pid {} ({:04X}) CRC failed {:08X} != {:08X} len {}",
                    ts.pid(),
                    ts.pid(),
                    crc,
                    current,
                    len
                );
                ms_log::hexdump!("packet failed CRC ", &self.buf[..len]);
            }
            return None;
        }
        self.last_len = self.expected;
        Some(&self.buf[..self.expected])
    }

    /// Feeds one packet and emits every complete section through `on_section`.
    pub fn push<F: FnMut(&[u8])>(&mut self, packet: &[u8], on_section: &mut F) {
        let Some(ts) = TsPacket::from_bytes(packet) else {
            return;
        };
        if ts.pusi() {
            self.clear_section();
            self.synced = true;
        } else if !self.synced {
            return;
        }

        let Some(payload) = ts.payload() else {
            return;
        };
        let mut index = if ts.pusi() {
            // A PUSI packet whose adaptation field fills it has no pointer_field.
            let Some(&pointer) = payload.first() else {
                return;
            };
            let pointer = pointer as usize;
            let start = 1 + pointer;
            if start > payload.len() {
                return;
            }
            start
        } else {
            0
        };

        while index < payload.len() {
            let byte = payload[index];
            if self.pos == 0 && byte == 0xFF {
                self.clear_section();
                self.synced = false;
                return;
            }
            if self.pos >= MAX_SECTION_SIZE {
                self.reset();
                return;
            }
            self.buf[self.pos] = byte;
            self.pos += 1;
            index += 1;

            if self.expected == 0 && self.pos >= 3 {
                self.expected = 3 + (((self.buf[1] & 0x0F) as usize) << 8) + self.buf[2] as usize;
                if self.expected > MAX_SECTION_SIZE {
                    self.reset();
                    return;
                }
            }
            if self.expected != 0 && self.pos == self.expected {
                let len = self.expected;
                let valid = !self.check_crc || crc_ok(&self.buf, len);
                if valid {
                    on_section(&self.buf[..len]);
                }
                self.clear_section();
            }
        }
    }

    fn clear_section(&mut self) {
        self.pos = 0;
        self.expected = 0;
        self.last_len = 0;
    }
}

fn crc_ok(buf: &[u8], len: usize) -> bool {
    if len < 4 {
        return false;
    }
    let stored = u32::from_be_bytes([buf[len - 4], buf[len - 3], buf[len - 2], buf[len - 1]]);
    crc32_mpeg2(&buf[..len - 4]) == stored
}

/// Reports whether a scrambled active PID changed parity without a PUSI.
///
/// Callers pass only active PIDs whose PMT is assigned, mirroring the C++ check.
pub fn wait_pusi(packets: &[u8], active_pids: &[u16]) -> bool {
    let mut state = [0u8; 8192];
    let mut parity = [0u8; 8192];
    for &pid in active_pids {
        if let Some(slot) = state.get_mut(pid as usize) {
            *slot = PID_INIT;
        }
    }

    for chunk in packets.as_chunks::<TS_PACKET_SIZE>().0 {
        let Some(ts) = TsPacket::from_bytes(chunk) else {
            continue;
        };
        let pid = ts.pid() as usize;
        if ts.scrambling_control() == 0 || state[pid] == 0 {
            continue;
        }
        let current = (chunk[3] & 0x40) != 0;
        if state[pid] == PID_INIT {
            state[pid] = PID_SET;
            parity[pid] = current as u8;
        }
        if current as u8 != parity[pid] {
            parity[pid] = current as u8;
            state[pid] |= PID_PARITY_CHANGE;
        }
        if ts.pusi() {
            state[pid] &= !PID_PARITY_CHANGE;
        }
    }

    let flagged = state
        .iter()
        .position(|&value| value & PID_PARITY_CHANGE != 0);
    if let Some(pid) = flagged {
        ms_log::logm!(
            "wait_pusi: found pid {} with parity change without start indicator",
            pid
        );
        return true;
    }
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_input_has_no_parity_change() {
        assert!(!wait_pusi(&[], &[0x1FFF]));
    }

    // Fuzzer crash: PUSI set, adaptation field (183 bytes) leaves an empty payload.
    #[test]
    fn pusi_with_empty_payload_does_not_panic() {
        let mut packet = [0xFFu8; 188];
        packet[..5].copy_from_slice(&[0x47, 0x40, 0x11, 0x30, 183]);
        packet[5] = 0x00;
        let mut pushed = SectionAssembler::new(true);
        let mut sections = 0;
        pushed.push(&packet, &mut |_| sections += 1);
        assert_eq!(sections, 0);
        let mut assembled = SectionAssembler::new(true);
        assert!(assembled.assemble_packet(&packet).is_none());
    }
}
