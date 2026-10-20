//! DVB text decoding following EN 300 468 Annex A.

use crate::charset_tables::{
    CONV_8859_TABLE, ISO6937_LONE_ACCENTS, ISO6937_MULTI_BYTE, ISO6937_SINGLE_BYTE,
};

const ISO_INDEX: [i8; 16] = [-1, 0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, -1, 11, 12, 13];

/// Forced charset selection, mirroring the C++ `--sdt-charset` switch.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum CharsetOverride {
    /// Detect the charset from the DVB prefix bytes.
    #[default]
    Auto,
    /// Decode every byte as ISO-8859-1, ignoring any prefix.
    Latin1,
}

#[derive(Clone, Copy)]
enum Encoding {
    Iso8859(usize),
    Iso6937,
    Ucs2,
    Utf8,
}

/// Decodes DVB text into UTF-8 bytes.
///
/// `dst_len` matches the C++ destination buffer size: at most `dst_len - 1`
/// output bytes are produced. Returns `None` on invalid input or overflow.
pub fn dvb_get_string(src: &[u8], dst_len: usize, charset: CharsetOverride) -> Option<Vec<u8>> {
    if src.is_empty() {
        return Some(Vec::new());
    }
    let cap = dst_len.checked_sub(1)?;

    if charset == CharsetOverride::Latin1 {
        return convert(Encoding::Iso8859(0), src, cap);
    }

    let mut data = src;
    let mut encoding = None;
    match data[0] {
        0 => return Some(Vec::new()),
        0x01..=0x0B => {
            encoding = iso_index(data[0] + 4);
            data = &data[1..];
        }
        0x0C..=0x0F | 0x12 | 0x13 | 0x16..=0x1E => {
            data = &data[1..];
        }
        0x10 => {
            if data.len() < 3 || data[1] != 0 || data[2] == 0 || data[2] > 0x0F {
                return None;
            }
            encoding = iso_index(data[2]);
            data = &data[3..];
        }
        0x11 | 0x14 => {
            encoding = Some(Encoding::Ucs2);
            data = &data[1..];
        }
        0x15 => {
            encoding = Some(Encoding::Utf8);
            data = &data[1..];
        }
        0x1F => return None,
        _ => encoding = Some(Encoding::Iso6937),
    }

    if data.is_empty() {
        return Some(Vec::new());
    }
    convert(encoding?, data, cap)
}

fn iso_index(value: u8) -> Option<Encoding> {
    match ISO_INDEX.get(value as usize).copied().unwrap_or(-1) {
        -1 => None,
        conv => Some(Encoding::Iso8859(conv as usize)),
    }
}

fn convert(encoding: Encoding, src: &[u8], cap: usize) -> Option<Vec<u8>> {
    match encoding {
        Encoding::Iso8859(table) => convert_8859(table, src, cap),
        Encoding::Iso6937 => convert_6937(src, cap),
        Encoding::Ucs2 => convert_ucs2(src, cap),
        Encoding::Utf8 => convert_utf8(src, cap),
    }
}

fn convert_utf8(src: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for &byte in src {
        if out.len() >= cap {
            return None;
        }
        if byte <= 0x7F {
            if !encode_utf8(&mut out, cap, byte as u32) {
                return None;
            }
        } else {
            out.push(byte);
        }
    }
    Some(out)
}

fn convert_8859(table: usize, src: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    for &byte in src {
        if out.len() >= cap {
            return None;
        }
        if byte <= 0x7F {
            if !encode_utf8(&mut out, cap, byte as u32) {
                return None;
            }
        } else if byte <= 0x9F {
            if byte == 0x8A {
                out.push(b'\n');
            }
        } else {
            let code = CONV_8859_TABLE[table][(byte - 0xA0) as usize];
            if code != 0 && !encode_utf8(&mut out, cap, code as u32) {
                return None;
            }
        }
    }
    Some(out)
}

fn convert_6937(src: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let mut index = 0;
    while index < src.len() {
        if out.len() >= cap {
            return None;
        }
        let byte = src[index];
        if byte <= 0x7F {
            if !encode_utf8(&mut out, cap, byte as u32) {
                return None;
            }
        } else if byte <= 0x9F {
            if byte == 0x8A {
                out.push(b'\n');
            }
        } else {
            let code = if (0xC0..=0xCF).contains(&byte) {
                if src.len() - index < 2 {
                    return None;
                }
                let next = src[index + 1];
                index += 1;
                match next {
                    0x20 => ISO6937_LONE_ACCENTS[(byte - 0xC0) as usize],
                    0x41..=0x5A => {
                        ISO6937_MULTI_BYTE[(byte - 0xC0) as usize][(next - 0x41) as usize]
                    }
                    0x61..=0x7A => {
                        ISO6937_MULTI_BYTE[(byte - 0xC0) as usize][(next - 0x61 + 26) as usize]
                    }
                    _ => 0,
                }
            } else {
                ISO6937_SINGLE_BYTE[(byte - 0xA0) as usize]
            };
            if code != 0 && !encode_utf8(&mut out, cap, code as u32) {
                return None;
            }
        }
        index += 1;
    }
    Some(out)
}

fn convert_ucs2(src: &[u8], cap: usize) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let len = src.len() & !1;
    let mut index = 0;
    while index < len {
        if out.len() >= cap {
            return None;
        }
        let code = ((src[index] as u16) << 8) | src[index + 1] as u16;
        if (0xE080..=0xE09F).contains(&code) {
            if code == 0xE08A {
                out.push(b'\n');
            }
        } else if !encode_utf8(&mut out, cap, code as u32) {
            return None;
        }
        index += 2;
    }
    Some(out)
}

fn encode_utf8(out: &mut Vec<u8>, cap: usize, code: u32) -> bool {
    let need = if code <= 0x7F {
        1
    } else if code <= 0x7FF {
        2
    } else if code <= 0xFFFF {
        3
    } else if code <= 0x10FFFF {
        4
    } else {
        return false;
    };
    if need == 1 && code < 0x20 && code != 0x0A {
        return true;
    }
    if out.len() + need > cap {
        return false;
    }
    match need {
        1 => out.push(code as u8),
        2 => {
            out.push((((code >> 6) & 0x1F) | 0xC0) as u8);
            out.push(((code & 0x3F) | 0x80) as u8);
        }
        3 => {
            out.push((((code >> 12) & 0x0F) | 0xE0) as u8);
            out.push((((code >> 6) & 0x3F) | 0x80) as u8);
            out.push(((code & 0x3F) | 0x80) as u8);
        }
        _ => {
            out.push((((code >> 18) & 0x07) | 0xF0) as u8);
            out.push((((code >> 12) & 0x3F) | 0x80) as u8);
            out.push((((code >> 6) & 0x3F) | 0x80) as u8);
            out.push(((code & 0x3F) | 0x80) as u8);
        }
    }
    true
}
