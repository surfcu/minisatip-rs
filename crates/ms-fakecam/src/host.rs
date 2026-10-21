//! Host side shaped like ca.cpp, byte for byte, to drive a [`FakeCam`] in tests.

use crate::asn1::{decode_len, encode_len};
use crate::cam::{FakeCam, SS_NOT_FOUND, apdu, parse_apdus};
use crate::tags::*;
use std::collections::BTreeMap;

const SLOT: u8 = 0;
const TCID: u8 = 1;

/// Resources in `application_handler[]` order, as `populate_resources` sends them.
pub const HOST_RESOURCES: [u32; 21] = [
    RESOURCE_MANAGER,
    APPLICATION_INFO,
    CA_SUPPORT,
    HOST_CONTROL,
    DATE_TIME,
    MMI,
    mkrid(1, 1, 2),
    mkrid(2, 1, 2),
    mkrid(2, 1, 3),
    mkrid(32, 1, 2),
    mkrid(32, 1, 3),
    mkrid(140, 64, 1),
    mkrid(140, 64, 2),
    mkrid(140, 64, 4),
    mkrid(141, 64, 1),
    mkrid(142, 64, 1),
    mkrid(143, 64, 1),
    mkrid(143, 64, 2),
    mkrid(143, 64, 3),
    mkrid(65, 1, 1),
    mkrid(65, 1, 2),
];

/// A TPDU as `ca_write_tpdu` writes it (slot 0, connection 1).
pub fn tpdu(tag: u8, data: &[u8]) -> Vec<u8> {
    let mut v = vec![SLOT, TCID, tag];
    match tag {
        T_DATA_LAST | T_DATA_MORE => {
            encode_len(data.len() + 1, &mut v);
            v.push(TCID);
            v.extend_from_slice(data);
        }
        T_NEW_TC | T_TC_ERROR => v.extend_from_slice(&[2, TCID, data[0]]),
        _ => v.extend_from_slice(&[1, TCID]),
    }
    v
}

/// An SPDU as `ca_write_spdu` builds it: tag, length, data, session number, APDU.
pub fn spdu(session: u16, tag: u8, data: &[u8], apdu: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    encode_len(data.len() + 2, &mut v);
    v.extend_from_slice(data);
    v.extend_from_slice(&session.to_be_bytes());
    v.extend_from_slice(apdu);
    v
}

/// The full TPDU `ca_write_apdu` sends for one APDU.
pub fn apdu_tpdu(session: u16, tag: u32, body: &[u8]) -> Vec<u8> {
    tpdu(
        T_DATA_LAST,
        &spdu(session, ST_SESSION_NUMBER, &[], &apdu(tag, body)),
    )
}

/// A module reply split like `ca_read_tpdu`: data objects and the status byte.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RTpdu {
    pub objects: Vec<(u8, Vec<u8>)>,
    pub sb: Option<u8>,
}

/// Splits an R_TPDU; returns `None` where `ca_read_tpdu` would stop on bad lengths.
pub fn parse_r_tpdu(msg: &[u8]) -> Option<RTpdu> {
    let mut d = msg.get(2..)?;
    let mut out = RTpdu::default();
    while !d.is_empty() {
        let tag = d[0];
        let (len, n) = decode_len(d.get(1..)?)?;
        let obj = d.get(1 + n..1 + n + len)?;
        let (_tcid, data) = obj.split_first()?;
        if tag == T_SB {
            out.sb = data.first().copied();
        } else {
            out.objects.push((tag, data.to_vec()));
        }
        d = &d[1 + n + len..];
    }
    Some(out)
}

/// Encodes a DATE_TIME body like `ca_send_datetime` for a Unix time.
pub fn date_time_body(unix: u64) -> [u8; 5] {
    let bcd = |v: u64| (((v / 10) << 4) | (v % 10)) as u8;
    let mjd = (unix / 86400 + 40587) as u16;
    let t = unix % 86400;
    let [hi, lo] = mjd.to_be_bytes();
    [hi, lo, bcd(t / 3600), bcd(t % 3600 / 60), bcd(t % 60)]
}

/// Minimal host reproducing the ca.cpp reactions used during CAM setup.
#[derive(Debug, Clone)]
pub struct HostSim {
    /// Open sessions: number → resource.
    pub sessions: BTreeMap<u16, u32>,
    /// CA_INFO received (`CA_STATE_INITIALIZED`).
    pub initialized: bool,
    pub caids: Vec<u16>,
    pub cam_name: Vec<u8>,
    /// `-ci` mode: the profile stops before the CI+ resources.
    pub force_ci: bool,
    /// Unix time sent in DATE_TIME replies.
    pub unix_time: u64,
    /// APDUs from the module as `(session, tag, body)`.
    pub received: Vec<(u16, u32, Vec<u8>)>,
    rx: Vec<u8>,
}

impl Default for HostSim {
    fn default() -> Self {
        Self {
            sessions: BTreeMap::new(),
            initialized: false,
            caids: Vec::new(),
            cam_name: Vec::new(),
            force_ci: false,
            unix_time: 1_700_000_000,
            received: Vec::new(),
            rx: Vec::new(),
        }
    }
}

impl HostSim {
    pub fn new() -> Self {
        Self::default()
    }

    fn write(&mut self, cam: &mut FakeCam, msg: &[u8]) -> RTpdu {
        let reply = cam.handle_tpdu(msg).expect("module replies to every TPDU");
        parse_r_tpdu(&reply).expect("well-formed R_TPDU")
    }

    /// T_CREATE_TC followed by polling until the module has nothing left.
    pub fn connect(&mut self, cam: &mut FakeCam) {
        let r = self.write(cam, &tpdu(T_CREATE_TC, &[]));
        assert!(r.objects.iter().any(|(t, _)| *t == T_CTC_REPLY));
        self.pump(cam);
    }

    /// Polls (`ca_timeout`) and receives (`T_RCV`) until the module is idle.
    pub fn pump(&mut self, cam: &mut FakeCam) {
        for _ in 0..10_000 {
            let r = self.write(cam, &tpdu(T_DATA_LAST, &[]));
            if r.sb != Some(0x80) {
                return;
            }
            let r = self.write(cam, &tpdu(T_RCV, &[]));
            for (tag, data) in r.objects {
                self.rx.extend_from_slice(&data);
                if tag == T_DATA_LAST {
                    let spdu = std::mem::take(&mut self.rx);
                    self.on_spdu(cam, &spdu);
                }
            }
        }
        panic!("module never went idle");
    }

    fn session_for(&self, resource: u32) -> Option<u16> {
        self.sessions
            .iter()
            .find(|(_, r)| **r == resource)
            .map(|(s, _)| *s)
    }

    /// Sends an APDU on the session of `resource` (`ca_write_apdu`).
    pub fn send(&mut self, cam: &mut FakeCam, resource: u32, tag: u32, body: &[u8]) {
        let session = self.session_for(resource).expect("session open");
        self.write(cam, &apdu_tpdu(session, tag, body));
    }

    /// Sends a CA_PMT body and pumps the replies.
    pub fn send_capmt(&mut self, cam: &mut FakeCam, body: &[u8]) {
        self.send(cam, CA_SUPPORT, TAG_CA_PMT, body);
        self.pump(cam);
    }

    fn on_spdu(&mut self, cam: &mut FakeCam, data: &[u8]) {
        let Some((len, n)) = data.get(1..).and_then(decode_len) else {
            return;
        };
        let Some(hdr) = data.get(1 + n..1 + n + len) else {
            return;
        };
        match data[0] {
            ST_OPEN_SESSION_REQUEST if hdr.len() >= 4 => {
                let res = u32::from_be_bytes([hdr[0], hdr[1], hdr[2], hdr[3]]);
                let known = HOST_RESOURCES.contains(&res);
                let free = (1..=64u16).find(|s| !self.sessions.contains_key(s));
                let (status, session) = match free {
                    Some(s) if known => (0, s),
                    _ => (SS_NOT_FOUND, 0xFFFF),
                };
                let mut d = vec![status];
                d.extend_from_slice(&res.to_be_bytes());
                self.write(
                    cam,
                    &tpdu(
                        T_DATA_LAST,
                        &spdu(session, ST_OPEN_SESSION_RESPONSE, &d, &[]),
                    ),
                );
                if status == 0 {
                    self.sessions.insert(session, res);
                    let create = match res {
                        RESOURCE_MANAGER => Some(TAG_PROFILE_ENQUIRY),
                        APPLICATION_INFO => Some(TAG_APP_INFO_ENQUIRY),
                        CA_SUPPORT => Some(TAG_CA_INFO_ENQUIRY),
                        _ => None,
                    };
                    if let Some(tag) = create {
                        self.write(cam, &apdu_tpdu(session, tag, &[]));
                    }
                }
            }
            ST_CLOSE_SESSION_REQUEST if hdr.len() >= 2 => {
                let session = u16::from_be_bytes([hdr[0], hdr[1]]);
                self.sessions.remove(&session);
                let mut d = vec![0];
                d.extend_from_slice(&session.to_be_bytes());
                self.write(
                    cam,
                    &tpdu(
                        T_DATA_LAST,
                        &spdu(session, ST_CLOSE_SESSION_RESPONSE, &d, &[]),
                    ),
                );
            }
            ST_SESSION_NUMBER if hdr.len() >= 2 => {
                let session = u16::from_be_bytes([hdr[0], hdr[1]]);
                for (tag, body) in parse_apdus(&data[1 + n + len..]) {
                    self.received.push((session, tag, body.to_vec()));
                    self.on_apdu(cam, session, tag, body);
                }
            }
            _ => {}
        }
    }

    fn on_apdu(&mut self, cam: &mut FakeCam, session: u16, tag: u32, body: &[u8]) {
        let reply: Option<(u32, Vec<u8>)> = match tag {
            TAG_PROFILE => Some((TAG_PROFILE_CHANGE, Vec::new())),
            TAG_PROFILE_ENQUIRY => {
                let n = if self.force_ci {
                    6
                } else {
                    HOST_RESOURCES.len()
                };
                let ids = HOST_RESOURCES[..n].iter().flat_map(|r| r.to_be_bytes());
                Some((TAG_PROFILE, ids.collect()))
            }
            TAG_APP_INFO if body.len() >= 6 => {
                let n = usize::from(body[5]).min(body.len() - 6);
                self.cam_name = body[6..6 + n].to_vec();
                None
            }
            TAG_CA_INFO => {
                self.initialized = true;
                self.caids = body
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|c| u16::from_be_bytes(*c))
                    .collect();
                None
            }
            TAG_DATE_TIME_ENQUIRY => Some((TAG_DATE_TIME, date_time_body(self.unix_time).to_vec())),
            TAG_DISPLAY_CONTROL if body.first() == Some(&1) => Some((
                TAG_DISPLAY_REPLY,
                vec![1, body.get(1).copied().unwrap_or(0)],
            )),
            TAG_ENQUIRY | TAG_MENU_LAST | TAG_LIST_LAST => Some((TAG_CLOSE_MMI, vec![0])),
            _ => None,
        };
        if let Some((tag, body)) = reply {
            self.write(cam, &apdu_tpdu(session, tag, &body));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tpdu_layout_matches_ca_write_tpdu() {
        assert_eq!(tpdu(T_CREATE_TC, &[]), [0, 1, 0x82, 1, 1]);
        assert_eq!(tpdu(T_RCV, &[]), [0, 1, 0x81, 1, 1]);
        assert_eq!(tpdu(T_DATA_LAST, &[]), [0, 1, 0xA0, 1, 1]);
        assert_eq!(tpdu(T_DATA_LAST, &[9, 8]), [0, 1, 0xA0, 3, 1, 9, 8]);
    }

    // From tests/test_ca.cpp:1817 (open session response is 9 bytes)
    #[test]
    fn open_session_response_is_nine_bytes() {
        let mut d = vec![SS_NOT_FOUND];
        d.extend_from_slice(&APPLICATION_INFO.to_be_bytes());
        let s = spdu(0xFFFF, ST_OPEN_SESSION_RESPONSE, &d, &[]);
        assert_eq!(s.len(), 9);
        assert_eq!(s[0], ST_OPEN_SESSION_RESPONSE);
        assert_eq!(s[2], 0xF0);
    }

    #[test]
    fn date_time_is_mjd_and_bcd() {
        // 2023-11-14 22:13:20 UTC, MJD 60262
        assert_eq!(
            date_time_body(1_700_000_000),
            [0xEB, 0x66, 0x22, 0x13, 0x20]
        );
    }
}
