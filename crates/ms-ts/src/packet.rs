//! Zero-copy view over a 188-byte MPEG-2 transport-stream packet.

/// Size of an MPEG-2 transport-stream packet in bytes.
pub const TS_PACKET_SIZE: usize = 188;

/// The sync byte that starts every transport-stream packet.
pub const TS_SYNC_BYTE: u8 = 0x47;

/// Adaptation field control values from the TS header.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Afc {
    /// Reserved value, no defined payload layout.
    Reserved,
    /// Payload follows the header.
    PayloadOnly,
    /// Only an adaptation field follows the header.
    AdaptationOnly,
    /// Both an adaptation field and a payload follow the header.
    AdaptationAndPayload,
}

/// Borrowed, validated reference to one 188-byte TS packet.
#[derive(Clone, Copy, Debug)]
pub struct TsPacket<'a> {
    bytes: &'a [u8; TS_PACKET_SIZE],
}

impl<'a> TsPacket<'a> {
    /// Wraps the first 188 bytes when the sync byte is present.
    pub fn from_bytes(bytes: &'a [u8]) -> Option<Self> {
        let raw: &[u8; TS_PACKET_SIZE] = bytes.get(..TS_PACKET_SIZE)?.try_into().ok()?;
        (raw[0] == TS_SYNC_BYTE).then_some(Self { bytes: raw })
    }

    /// The underlying packet bytes.
    pub fn as_bytes(&self) -> &'a [u8; TS_PACKET_SIZE] {
        self.bytes
    }

    /// 13-bit packet identifier.
    pub fn pid(&self) -> u16 {
        (((self.bytes[1] & 0x1F) as u16) << 8) | self.bytes[2] as u16
    }

    /// Payload unit start indicator.
    pub fn pusi(&self) -> bool {
        self.bytes[1] & 0x40 != 0
    }

    /// Transport error indicator.
    pub fn tei(&self) -> bool {
        self.bytes[1] & 0x80 != 0
    }

    /// Two-bit transport scrambling control field.
    pub fn scrambling_control(&self) -> u8 {
        self.bytes[3] >> 6
    }

    /// Adaptation field control bits.
    pub fn adaptation_field_control(&self) -> Afc {
        match (self.bytes[3] >> 4) & 0x03 {
            0 => Afc::Reserved,
            1 => Afc::PayloadOnly,
            2 => Afc::AdaptationOnly,
            _ => Afc::AdaptationAndPayload,
        }
    }

    /// Four-bit continuity counter.
    pub fn continuity_counter(&self) -> u8 {
        self.bytes[3] & 0x0F
    }

    /// Adaptation field bytes (excluding the length byte), when present.
    pub fn adaptation_field(&self) -> Option<&'a [u8]> {
        match self.adaptation_field_control() {
            Afc::AdaptationOnly | Afc::AdaptationAndPayload => {
                let end = 5 + self.bytes[4] as usize;
                (end <= TS_PACKET_SIZE).then(|| &self.bytes[5..end])
            }
            Afc::Reserved | Afc::PayloadOnly => None,
        }
    }

    /// Payload bytes copied out of the packet by reference, when present.
    pub fn payload(&self) -> Option<&'a [u8]> {
        match self.adaptation_field_control() {
            Afc::PayloadOnly => Some(&self.bytes[4..]),
            Afc::AdaptationAndPayload => {
                let start = 5 + self.bytes[4] as usize;
                (start <= TS_PACKET_SIZE).then(|| &self.bytes[start..])
            }
            Afc::Reserved | Afc::AdaptationOnly => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_header_fields() {
        let mut raw = [0u8; TS_PACKET_SIZE];
        raw[0] = TS_SYNC_BYTE;
        raw[1] = 0xE1;
        raw[2] = 0x23;
        raw[3] = 0xB7;
        let ts = TsPacket::from_bytes(&raw).unwrap();
        assert_eq!(ts.pid(), 0x0123);
        assert!(ts.pusi());
        assert!(ts.tei());
        assert_eq!(ts.scrambling_control(), 2);
        assert_eq!(ts.adaptation_field_control(), Afc::AdaptationAndPayload);
        assert_eq!(ts.continuity_counter(), 7);
    }

    #[test]
    fn rejects_bad_sync_and_short_input() {
        let raw = [0u8; TS_PACKET_SIZE];
        assert!(TsPacket::from_bytes(&raw).is_none());
        assert!(TsPacket::from_bytes(&[TS_SYNC_BYTE; 10]).is_none());
    }
}
