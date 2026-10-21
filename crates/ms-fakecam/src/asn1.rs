//! EN 50221 length fields (`asn_1_encode` / `asn_1_decode` in ca.cpp).

/// Appends `len` in the EN 50221 length_field encoding.
pub fn encode_len(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
}

/// Decodes a length field: `(value, bytes used)`, like `asn_1_decode`.
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
    fn encodes_like_asn_1_encode() {
        assert_eq!(enc(0x7F), [0x7F]);
        assert_eq!(enc(0x80), [0x81, 0x80]);
        assert_eq!(enc(0xCE), [0x81, 0xCE]);
        assert_eq!(enc(0x1234), [0x82, 0x12, 0x34]);
    }

    #[test]
    fn decodes_like_asn_1_decode() {
        assert_eq!(decode_len(&[0x05]), Some((5, 1)));
        // From tests/test_ca.cpp:1767 (APP_INFO length 206)
        assert_eq!(decode_len(&[0x81, 0xCE]), Some((206, 2)));
        assert_eq!(decode_len(&[0x82, 0x01, 0x00]), Some((256, 3)));
        assert_eq!(decode_len(&[0x83, 0, 0, 0]), None);
        assert_eq!(decode_len(&[0x81]), None);
        assert_eq!(decode_len(&[]), None);
    }
}
