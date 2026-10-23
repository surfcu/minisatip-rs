//! `buffer_to_ts` (utils.cpp:700): PSI sections packed into TS packets.

/// `utils.cpp` includes `logging.h` without overriding `DEFAULT_LOG`.
const DEFAULT_LOG: u32 = ms_log::LOG_UTILS;

/// `buffer_to_ts`: packs a PSI buffer into TS packets on `pid`.
/// Returns the bytes written, or -1 when a packet does not fit (C++ value).
pub fn buffer_to_ts(dest: &mut [u8], src: &[u8], cc: &mut i16, pid: u16) -> isize {
    let mut pos = 0;
    let mut len = 0;
    while pos < src.len() && len < dest.len() {
        if dest.len() - len < 188 {
            // utils.cpp:706-709
            ms_log::log_and_return!(
                -1isize,
                "Not enough space to copy pid {}, len {} from {}, srclen {}",
                pid,
                len,
                dest.len(),
                src.len()
            );
        }
        let b = &mut dest[len..len + 188];
        *cc = (*cc + 1) % 16;
        b[0] = 0x47;
        b[1] = (pid >> 8) as u8;
        if pos == 0 {
            b[1] |= 0x40;
        }
        b[2] = pid as u8;
        b[3] = 0x10 | (*cc as u8 & 0x0F);
        let left = (src.len() - pos).min(184);
        b[4..4 + left].copy_from_slice(&src[pos..pos + left]);
        b[4 + left..].fill(0xFF);
        // utils.cpp:724-726
        if ms_log::debug_enabled(ms_log::LOG_UTILS) {
            ms_log::log!("pid {}, left -> {}, len {}, cc {}", pid, left, len, *cc);
            ms_log::hexdump!("packet -> ", b);
        }
        pos += left;
        len += 188;
    }
    len as isize
}
