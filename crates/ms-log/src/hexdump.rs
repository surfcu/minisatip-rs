//! `_hexdump`, `_dump_packets` and `crc_32` (utils.cpp:569-697).

use std::fmt::Write as _;

/// CRC-32/MPEG-2 table, built like `crc_tab` in utils.cpp:624-667.
const CRC_TABLE: [u32; 256] = build_crc_table();

const fn build_crc_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;
    while index < 256 {
        let mut crc = (index as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[index] = crc;
        index += 1;
    }
    table
}

/// `crc_32` (utils.cpp:669-678): CRC-32/MPEG-2 without a final complement.
///
/// The C++ function also hexdumps the input when the `utils` module is at
/// debug level; that side effect is not reproduced here.
pub fn crc_32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let index = ((crc >> 24) ^ u32::from(byte)) & 0xff;
        crc = (crc << 8) ^ CRC_TABLE[index as usize];
    }
    crc
}

/// `_hexdump` (utils.cpp:569-622): hex and ASCII dump, one line per 16 bytes.
///
/// The message ends with the newline C++ includes in the format string, so a
/// stdout backend prints one blank line after the dump. `None` selects the
/// unnamed form `"\n%s"`.
pub fn hexdump(desc: Option<&str>, data: &[u8]) {
    hexdump_into(super::logger(), desc, data);
}

/// [`hexdump`] against an explicit logger (used by the unit tests).
pub(crate) fn hexdump_into(logger: &super::Logger, desc: Option<&str>, data: &[u8]) {
    if data.is_empty() {
        logger.write(
            file!(),
            line!(),
            format_args!("{}: ZERO LENGTH", desc.unwrap_or("(null)")),
        );
        return;
    }

    // `buff[17]` holds the printable form of the current 16-byte line.
    let mut buff = [0u8; 17];
    let mut buf = String::new();
    let mut i = 0;
    while i < data.len() {
        if i % 16 == 0 {
            if i != 0 {
                buf.push_str("  ");
                buf.push_str(ascii(&buff));
                buf.push('\n');
            }
            // Output the offset (`strlcatf(buf, bl, pos, "  %04x ", i)`).
            let _ = write!(buf, "  {i:04x} ");
        }

        // Hex code for the byte, then remember its printable form.
        let _ = write!(buf, " {:02x}", data[i]);
        buff[i % 16] = if data[i].is_ascii_graphic() || data[i] == b' ' {
            data[i]
        } else {
            b'.'
        };
        buff[(i % 16) + 1] = 0;
        i += 1;
    }

    // Pad the last line to 16 bytes (`"   "` per missing byte).
    while i % 16 != 0 {
        buf.push_str("   ");
        i += 1;
    }
    buf.push_str("  ");
    buf.push_str(ascii(&buff));
    buf.push('\n');

    let message = match desc {
        Some(desc) => format!("{desc}:\n{buf}"),
        None => format!("\n{buf}"),
    };
    logger.write(file!(), line!(), format_args!("{message}"));
}

/// Reads the NUL-terminated ASCII column of [`hexdump`].
fn ascii(buff: &[u8; 17]) -> &str {
    let end = buff.iter().position(|&byte| byte == 0).unwrap_or(16);
    std::str::from_utf8(&buff[..end]).unwrap_or("")
}

/// `_dump_packets` (utils.cpp:680-697): one line per 188-byte packet.
///
/// Buffers shorter than a packet are dumped from the bytes present; C++ reads
/// past the end there.
pub fn dump_packets(message: &str, data: &[u8], offset: i64) {
    dump_packets_into(super::logger(), message, data, offset);
}

/// [`dump_packets`] against an explicit logger (used by the unit tests).
pub(crate) fn dump_packets_into(logger: &super::Logger, message: &str, data: &[u8], offset: i64) {
    let mut i = 0;
    while i < data.len() {
        let Some(head) = data.get(i..i + 8) else {
            break;
        };
        let end = (i + 188).min(data.len());
        let crc = crc_32(data.get(i + 4..end).unwrap_or_default());
        let pid = (u16::from(head[1] & 0x1f) << 8) | u16::from(head[2]);
        let cc = head[3] & 0x0f;
        let encrypted = if head[3] & 0x80 != 0 { "encrypted" } else { "" };
        let pos = i as i64 + offset;
        let packet = pos / 188;
        logger.write(
            file!(),
            line!(),
            format_args!(
                "{message}: pid {pid:04} ({pid:04X}) CC={cc:X} CRC={crc:08X} {encrypted} \
                 pos: {pos} packet {packet} : [{:02X} {:02X} {:02X} {:02X}] {:02X} {:02X} \
                 {:02X} {:02X}",
                head[0], head[1], head[2], head[3], head[4], head[5], head[6], head[7]
            ),
        );
        i += 188;
    }
}
