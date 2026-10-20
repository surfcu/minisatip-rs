//! MPEG-2 CRC-32 used by PSI sections.

const CRC_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04C1_1DB7
            } else {
                crc << 1
            };
            bit += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

/// Computes the MPEG-2 CRC-32 (poly 0x04C11DB7, init 0xFFFF_FFFF).
pub fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let index = ((crc >> 24) ^ byte as u32) & 0xFF;
        crc = (crc << 8) ^ CRC_TABLE[index as usize];
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_reference_polynomial() {
        assert_eq!(CRC_TABLE[0], 0x0000_0000);
        assert_eq!(CRC_TABLE[1], 0x04C1_1DB7);
        assert_eq!(CRC_TABLE[255], 0xB1F7_40B4);
    }

    #[test]
    fn empty_input_is_initial_value() {
        assert_eq!(crc32_mpeg2(&[]), 0xFFFF_FFFF);
    }
}
