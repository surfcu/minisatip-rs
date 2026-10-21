//! EN 50221 length fields (`asn_1_encode` / `asn_1_decode` in ca.cpp).

/// Appends `len` in the length_field encoding; lengths above 0xFFFF are not used.
pub fn encode_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
}

/// Decodes a length field as `(value, bytes used)`; `None` where C++ returns -1.
pub fn decode_len(data: &[u8]) -> Option<(usize, usize)> {
    match *data.first()? {
        b if b < 0x80 => Some((usize::from(b), 1)),
        0x81 => Some((usize::from(*data.get(1)?), 2)),
        0x82 => Some((
            (usize::from(*data.get(1)?) << 8) | usize::from(*data.get(2)?),
            3,
        )),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enc(len: usize) -> Vec<u8> {
        let mut v = Vec::new();
        encode_len(len, &mut v);
        v
    }

    #[test]
    fn round_trips() {
        for (len, bytes) in [
            (0usize, vec![0u8]),
            (0x7F, vec![0x7F]),
            (0x80, vec![0x81, 0x80]),
            (0xCE, vec![0x81, 0xCE]),
            (0x1234, vec![0x82, 0x12, 0x34]),
        ] {
            assert_eq!(enc(len), bytes);
            assert_eq!(decode_len(&bytes), Some((len, bytes.len())));
        }
        assert_eq!(decode_len(&[0x83, 0, 0, 0]), None);
        assert_eq!(decode_len(&[0x82, 1]), None);
        assert_eq!(decode_len(&[]), None);
    }
}
