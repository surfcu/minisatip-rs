//! CA_PMT transport over the EN 50221 link layer, framed like `ca_write_apdu` in ca.cpp.

use ms_ca::{CaCaps, CaDriver};

pub const TAG_CA_PMT: u32 = 0x9F8032;
pub const ST_SESSION_NUMBER: u8 = 0x90;
pub const T_DATA_LAST: u8 = 0xA0;
/// `slot_id` 0 and `connection_id = slot_id + 1`, fixed in `ca_write_tpdu`.
const SLOT: u8 = 0;
const TCID: u8 = 1;

/// `asn_1_encode`.
pub fn asn_1_encode(len: usize, out: &mut Vec<u8>) {
    if len < 0x80 {
        out.push(len as u8);
    } else if len < 0x100 {
        out.extend_from_slice(&[0x81, len as u8]);
    } else {
        out.extend_from_slice(&[0x82, (len >> 8) as u8, len as u8]);
    }
}

/// `ca_write_apdu` → `ca_write_spdu` → `ca_write_tpdu(T_DATA_LAST)`: the bytes written to the CA fd.
pub fn apdu_tpdu(session: u16, tag: u32, body: &[u8]) -> Vec<u8> {
    let mut apdu = vec![(tag >> 16) as u8, (tag >> 8) as u8, tag as u8];
    asn_1_encode(body.len(), &mut apdu);
    apdu.extend_from_slice(body);
    let mut spdu = vec![ST_SESSION_NUMBER];
    asn_1_encode(2, &mut spdu);
    spdu.extend_from_slice(&session.to_be_bytes());
    spdu.extend_from_slice(&apdu);
    let mut tpdu = vec![SLOT, TCID, T_DATA_LAST];
    asn_1_encode(spdu.len() + 1, &mut tpdu);
    tpdu.push(TCID);
    tpdu.extend_from_slice(&spdu);
    tpdu
}

/// Where TPDUs go: the CA device fd, or a simulator.
pub trait TpduLink {
    /// Writes one TPDU; false on a short or failed write.
    fn write_tpdu(&mut self, tpdu: &[u8]) -> bool;
}

/// A CAM reached over EN 50221 with an open CA support session.
pub struct En50221Driver<L: TpduLink> {
    pub link: L,
    /// Session number of the CA support resource, if open.
    pub ca_session: Option<u16>,
    pub caps: CaCaps,
}

impl<L: TpduLink> En50221Driver<L> {
    pub fn new(link: L, ca_session: Option<u16>, caps: CaCaps) -> Self {
        Self {
            link,
            ca_session,
            caps,
        }
    }
}

impl<L: TpduLink> CaDriver for En50221Driver<L> {
    fn capabilities(&self) -> CaCaps {
        self.caps
    }

    fn has_ca_session(&self) -> bool {
        self.ca_session.is_some()
    }

    fn send_ca_pmt(&mut self, body: &[u8]) -> bool {
        match self.ca_session {
            Some(s) => self.link.write_tpdu(&apdu_tpdu(s, TAG_CA_PMT, body)),
            None => false,
        }
    }
}
