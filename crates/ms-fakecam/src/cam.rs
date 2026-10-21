//! EN 50221 CAM: transport, session and application layers as seen from the module.

use crate::asn1::{decode_len, encode_len};
use crate::capmt::{CaPmt, encode_reply};
use crate::clock::Clock;
use crate::config::{CamConfig, PmtReplyMode};
use crate::ddci::{DdciPort, TsStats};
use crate::programs::ProgramList;
use crate::record::{Direction, Event, Record, Recorder};
use crate::tags::*;
use std::collections::{BTreeMap, VecDeque};
use std::sync::Arc;

/// Session-layer status: resource non-existent (also used by the host).
pub const SS_NOT_FOUND: u8 = 0xF0;

/// Builds an SPDU: tag, length, header bytes, then the APDUs.
fn spdu(tag: u8, header: &[u8], apdus: &[u8]) -> Vec<u8> {
    let mut v = vec![tag];
    encode_len(header.len(), &mut v);
    v.extend_from_slice(header);
    v.extend_from_slice(apdus);
    v
}

/// Builds an APDU: 3-byte tag, length, body.
pub fn apdu(tag: u32, body: &[u8]) -> Vec<u8> {
    let mut v = tag.to_be_bytes()[1..].to_vec();
    encode_len(body.len(), &mut v);
    v.extend_from_slice(body);
    v
}

/// Splits a byte run into `(tag, body)` APDUs; stops at the first malformed one.
pub fn parse_apdus(mut data: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    while data.len() >= 4 {
        let tag = u32::from_be_bytes([0, data[0], data[1], data[2]]);
        let Some((len, n)) = decode_len(&data[3..]) else {
            break;
        };
        let Some(body) = data.get(3 + n..3 + n + len) else {
            break;
        };
        out.push((tag, body));
        data = &data[3 + n + len..];
    }
    out
}

/// A simulated CAM; one instance per CI slot.
pub struct FakeCam {
    config: CamConfig,
    clock: Arc<dyn Clock>,
    recorder: Recorder,
    tc_open: bool,
    rx: Vec<u8>,
    tx: VecDeque<Vec<u8>>,
    tx_rest: Vec<u8>,
    sessions: BTreeMap<u16, u32>,
    to_open: VecDeque<u32>,
    awaiting_open: Option<u32>,
    rm_ready: bool,
    host_resources: Vec<u32>,
    date_time: Option<Vec<u8>>,
    programs: ProgramList,
    ddci: DdciPort,
}

impl FakeCam {
    pub fn new(config: CamConfig, clock: Arc<dyn Clock>) -> Self {
        Self::with_recorder(config, clock, Recorder::new())
    }

    /// Like [`FakeCam::new`], logging into a shared recorder (multi-CAM setups).
    pub fn with_recorder(config: CamConfig, clock: Arc<dyn Clock>, recorder: Recorder) -> Self {
        Self {
            programs: ProgramList::new(&config),
            ddci: DdciPort::new(&config),
            config,
            clock,
            recorder,
            tc_open: false,
            rx: Vec::new(),
            tx: VecDeque::new(),
            tx_rest: Vec::new(),
            sessions: BTreeMap::new(),
            to_open: VecDeque::new(),
            awaiting_open: None,
            rm_ready: false,
            host_resources: Vec::new(),
            date_time: None,
        }
    }

    pub fn config(&self) -> &CamConfig {
        &self.config
    }

    pub fn recorder(&self) -> &Recorder {
        &self.recorder
    }

    pub fn programs(&self) -> &ProgramList {
        &self.programs
    }

    /// Open sessions as `(session number, resource id)`.
    pub fn sessions(&self) -> Vec<(u16, u32)> {
        self.sessions.iter().map(|(s, r)| (*s, *r)).collect()
    }

    /// Resources the host listed in its profile reply.
    pub fn host_resources(&self) -> &[u32] {
        &self.host_resources
    }

    /// Body of the last DATE_TIME the host sent.
    pub fn last_date_time(&self) -> Option<&[u8]> {
        self.date_time.as_deref()
    }

    /// True once the CA support session is open (the host can send CA_PMTs).
    pub fn ca_ready(&self) -> bool {
        self.session_for(CA_SUPPORT).is_some()
    }

    fn now(&self) -> u64 {
        self.clock.now_ms()
    }

    fn record(&self, dir: Direction, event: Event) {
        self.recorder.push(Record {
            time_ms: self.now(),
            cam: self.config.id,
            dir,
            event,
        });
    }

    fn session_for(&self, resource: u32) -> Option<u16> {
        self.sessions
            .iter()
            .find(|(_, r)| **r == resource)
            .map(|(s, _)| *s)
    }

    fn reset(&mut self) {
        self.rx.clear();
        self.tx.clear();
        self.tx_rest.clear();
        self.sessions.clear();
        self.to_open.clear();
        self.awaiting_open = None;
        self.rm_ready = false;
        self.programs.clear();
        self.ddci.reset();
    }

    /// Module inserted: forget everything and start opening sessions.
    pub fn start(&mut self) {
        self.reset();
        self.to_open = self.config.resources.iter().copied().collect();
        self.open_next();
    }

    fn open_next(&mut self) {
        if self.awaiting_open.is_some() {
            return;
        }
        while let Some(res) = self.to_open.pop_front() {
            if self.session_for(res).is_some() {
                continue;
            }
            self.record(
                Direction::CamToHost,
                Event::OpenSessionRequest { resource: res },
            );
            self.tx
                .push_back(spdu(ST_OPEN_SESSION_REQUEST, &res.to_be_bytes(), &[]));
            self.awaiting_open = Some(res);
            return;
        }
    }

    /// Queues an APDU on the session of `resource`; false when none is open.
    pub fn send_to_resource(&mut self, resource: u32, tag: u32, body: &[u8]) -> bool {
        match self.session_for(resource) {
            Some(s) => {
                self.send_apdu(s, tag, body);
                true
            }
            None => false,
        }
    }

    fn send_apdu(&mut self, session: u16, tag: u32, body: &[u8]) {
        let resource = self.sessions.get(&session).copied().unwrap_or(0);
        self.record(
            Direction::CamToHost,
            Event::Apdu {
                session,
                resource,
                tag,
                body: body.to_vec(),
            },
        );
        self.tx.push_back(spdu(
            ST_SESSION_NUMBER,
            &session.to_be_bytes(),
            &apdu(tag, body),
        ));
    }

    /// Takes the queued SPDUs (enigma-style framing, no transport layer).
    pub fn take_spdus(&mut self) -> Vec<Vec<u8>> {
        self.tx.drain(..).collect()
    }

    /// Handles one raw SPDU and returns the SPDUs queued in response.
    pub fn handle_spdu(&mut self, data: &[u8]) -> Vec<Vec<u8>> {
        self.on_spdu(data);
        self.take_spdus()
    }

    fn on_spdu(&mut self, data: &[u8]) {
        let Some(&tag) = data.first() else { return };
        let Some((len, n)) = decode_len(&data[1..]) else {
            return;
        };
        let Some(hdr) = data.get(1 + n..1 + n + len) else {
            return;
        };
        let rest = &data[1 + n + len..];
        match tag {
            ST_OPEN_SESSION_RESPONSE if hdr.len() >= 7 => {
                let status = hdr[0];
                let resource = u32::from_be_bytes([hdr[1], hdr[2], hdr[3], hdr[4]]);
                let session = u16::from_be_bytes([hdr[5], hdr[6]]);
                self.record(
                    Direction::HostToCam,
                    Event::OpenSessionResponse {
                        status,
                        resource,
                        session,
                    },
                );
                if self.awaiting_open == Some(resource) {
                    self.awaiting_open = None;
                }
                if status == 0 {
                    self.sessions.insert(session, resource);
                    self.on_session_opened(session, resource);
                }
                if status != 0 || resource != RESOURCE_MANAGER || self.rm_ready {
                    self.open_next();
                }
            }
            ST_CLOSE_SESSION_REQUEST if hdr.len() >= 2 => {
                let session = u16::from_be_bytes([hdr[0], hdr[1]]);
                self.record(Direction::HostToCam, Event::CloseSessionRequest { session });
                let status = match self.close_session(session) {
                    true => 0,
                    false => SS_NOT_FOUND,
                };
                self.record(
                    Direction::CamToHost,
                    Event::CloseSessionResponse { status, session },
                );
                let mut h = vec![status];
                h.extend_from_slice(&session.to_be_bytes());
                self.tx.push_back(spdu(ST_CLOSE_SESSION_RESPONSE, &h, &[]));
            }
            ST_CLOSE_SESSION_RESPONSE if hdr.len() >= 3 => {
                let session = u16::from_be_bytes([hdr[1], hdr[2]]);
                self.record(
                    Direction::HostToCam,
                    Event::CloseSessionResponse {
                        status: hdr[0],
                        session,
                    },
                );
                self.close_session(session);
            }
            ST_SESSION_NUMBER if hdr.len() >= 2 => {
                let session = u16::from_be_bytes([hdr[0], hdr[1]]);
                let resource = self.sessions.get(&session).copied().unwrap_or(0);
                for (tag, body) in parse_apdus(rest) {
                    self.record(
                        Direction::HostToCam,
                        Event::Apdu {
                            session,
                            resource,
                            tag,
                            body: body.to_vec(),
                        },
                    );
                    if resource != 0 {
                        self.on_apdu(session, resource, tag, body);
                    }
                }
            }
            _ => {}
        }
    }

    /// Closes a session from the CAM side (ST_CLOSE_SESSION_REQUEST).
    pub fn request_close(&mut self, resource: u32) -> bool {
        let Some(session) = self.session_for(resource) else {
            return false;
        };
        self.record(Direction::CamToHost, Event::CloseSessionRequest { session });
        self.tx
            .push_back(spdu(ST_CLOSE_SESSION_REQUEST, &session.to_be_bytes(), &[]));
        true
    }

    fn close_session(&mut self, session: u16) -> bool {
        match self.sessions.remove(&session) {
            Some(CA_SUPPORT) => {
                self.programs.clear();
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn on_session_opened(&mut self, session: u16, resource: u32) {
        if resource == DATE_TIME {
            let interval = self.config.datetime_interval_s;
            self.send_apdu(session, TAG_DATE_TIME_ENQUIRY, &[interval]);
        }
    }

    fn on_apdu(&mut self, session: u16, resource: u32, tag: u32, body: &[u8]) {
        match (resource, tag) {
            (RESOURCE_MANAGER, TAG_PROFILE_ENQUIRY) => {
                let ids: Vec<u8> = self
                    .config
                    .resources
                    .iter()
                    .flat_map(|r| r.to_be_bytes())
                    .collect();
                self.send_apdu(session, TAG_PROFILE, &ids);
            }
            (RESOURCE_MANAGER, TAG_PROFILE_CHANGE) => {
                self.send_apdu(session, TAG_PROFILE_ENQUIRY, &[]);
            }
            (RESOURCE_MANAGER, TAG_PROFILE) => {
                self.host_resources = body
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .map(|c| u32::from_be_bytes(*c))
                    .collect();
                if !self.rm_ready {
                    self.rm_ready = true;
                    self.open_next();
                }
            }
            (APPLICATION_INFO, TAG_APP_INFO_ENQUIRY) => {
                let ai = &self.config.app_info;
                let menu = ai.menu.as_bytes();
                let menu = &menu[..menu.len().min(255)];
                let mut b = vec![ai.application_type];
                b.extend_from_slice(&ai.manufacturer.to_be_bytes());
                b.extend_from_slice(&ai.manufacturer_code.to_be_bytes());
                b.push(menu.len() as u8);
                b.extend_from_slice(menu);
                self.send_apdu(session, TAG_APP_INFO, &b);
            }
            (CA_SUPPORT, TAG_CA_INFO_ENQUIRY) => {
                let ids: Vec<u8> = self
                    .config
                    .caids
                    .iter()
                    .flat_map(|c| c.to_be_bytes())
                    .collect();
                self.send_apdu(session, TAG_CA_INFO, &ids);
            }
            (CA_SUPPORT, TAG_CA_PMT) => {
                let Ok(pmt) = CaPmt::parse(body) else { return };
                let applied = self.programs.apply(&pmt, self.now());
                let reply = match self.config.pmt_reply {
                    PmtReplyMode::Never => false,
                    PmtReplyMode::OnQuery => applied.query,
                    PmtReplyMode::Always => applied.query || applied.changed,
                };
                if reply {
                    let b = encode_reply(&pmt, applied.ca_enable);
                    self.send_apdu(session, TAG_CA_PMT_REPLY, &b);
                }
            }
            (DATE_TIME, TAG_DATE_TIME) => self.date_time = Some(body.to_vec()),
            _ => {}
        }
    }

    /// Handles one host write in Linux CA device framing (`ca_write_tpdu`) and
    /// returns the module's reply: the R_TPDU, always ending in a T_SB.
    pub fn handle_tpdu(&mut self, msg: &[u8]) -> Option<Vec<u8>> {
        let [slot, tcid, tag, rest @ ..] = msg else {
            return None;
        };
        let (slot, tcid, tag) = (*slot, *tcid, *tag);
        let (len, n) = decode_len(rest)?;
        let body = rest.get(n..n + len)?;
        let data = body.get(1..).unwrap_or(&[]);
        let mut reply = None;
        match tag {
            T_CREATE_TC => {
                self.record(Direction::HostToCam, Event::Transport("create_tc"));
                self.tc_open = true;
                self.start();
                self.record(Direction::CamToHost, Event::Transport("ctc_reply"));
                reply = Some((T_CTC_REPLY, Vec::new()));
            }
            T_DELETE_TC => {
                self.record(Direction::HostToCam, Event::Transport("delete_tc"));
                self.tc_open = false;
                self.reset();
                self.record(Direction::CamToHost, Event::Transport("dtc_reply"));
                reply = Some((T_DTC_REPLY, Vec::new()));
            }
            T_DATA_LAST | T_DATA_MORE if self.tc_open => {
                self.rx.extend_from_slice(data);
                if tag == T_DATA_LAST && !self.rx.is_empty() {
                    let spdu = std::mem::take(&mut self.rx);
                    self.on_spdu(&spdu);
                }
            }
            T_RCV if self.tc_open => reply = self.next_chunk(),
            _ => {}
        }
        Some(self.r_tpdu(slot, tcid, reply))
    }

    fn next_chunk(&mut self) -> Option<(u8, Vec<u8>)> {
        if self.tx_rest.is_empty() {
            self.tx_rest = self.tx.pop_front()?;
        }
        let max = self.config.max_tpdu_data.unwrap_or(usize::MAX).max(1);
        if self.tx_rest.len() > max {
            let rest = self.tx_rest.split_off(max);
            Some((T_DATA_MORE, std::mem::replace(&mut self.tx_rest, rest)))
        } else {
            Some((T_DATA_LAST, std::mem::take(&mut self.tx_rest)))
        }
    }

    fn r_tpdu(&self, slot: u8, tcid: u8, obj: Option<(u8, Vec<u8>)>) -> Vec<u8> {
        let mut v = vec![slot, tcid];
        if let Some((tag, data)) = obj {
            v.push(tag);
            encode_len(data.len() + 1, &mut v);
            v.push(tcid);
            v.extend_from_slice(&data);
        }
        let pending = self.tc_open && (!self.tx.is_empty() || !self.tx_rest.is_empty());
        v.extend_from_slice(&[T_SB, 2, tcid, if pending { 0x80 } else { 0 }]);
        v
    }

    /// Feeds TS written to the CI (DDCI input).
    pub fn write_ts(&mut self, data: &[u8]) {
        let now = self.now();
        self.ddci.write(data, &self.programs, now);
    }

    /// TS due at the CI output now.
    pub fn read_ts(&mut self) -> Vec<u8> {
        let now = self.now();
        self.ddci.read(now)
    }

    pub fn ts_stats(&self) -> TsStats {
        self.ddci.stats()
    }

    /// TS packets accepted but still delayed.
    pub fn ts_pending(&self) -> usize {
        self.ddci.pending()
    }
}
