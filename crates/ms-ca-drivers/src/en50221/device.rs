//! `ca_device_t`, host side: TPDU link layer, SPDU sessions, keepalive and watchdogs.

use super::asn1::{decode_len, encode_len};
use super::clock::Clock;
use super::consts::*;
use super::link::{
    CA_CI_LINK, CA_CI_MODULE_READY, CaLink, ResetResult, SlotInfo, reset_and_wait_ready,
};
use super::resources::{Handler, cmd_reply, cmd_tracked, find_handler};
use std::collections::VecDeque;
use std::fmt;
use std::io;
use std::sync::Arc;

/// `EIO`: a failed write with it means the CAM was removed.
const EIO: i32 = 5;

/// `#define DEFAULT_LOG LOG_DVBCA` (ca.cpp:52).
const DEFAULT_LOG: u32 = ms_log::LOG_DVBCA;

/// `CA_STATE_*`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum CaState {
    #[default]
    Inactive,
    Active,
    Initialized,
}

/// Framing on the CA device: TPDUs (`/dev/dvb`) or bare SPDUs (`opts.enigma`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum LinkMode {
    #[default]
    Tpdu,
    Enigma,
}

/// Result of one read callback: C++ returns 1 (data or retry) or 0 (close the socket).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadOutcome {
    /// Session data gathered from the T_DATA objects; may be empty.
    Data(Vec<u8>),
    Again,
    Eof,
}

/// What the host reports to its owner; the CA reconciler (M6) consumes the CAPMT ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostEvent {
    /// `ca_request_close`: the owner must close the CA socket.
    CloseRequested,
    /// The CAPMT slots were wiped (`dvbca_init_dev`).
    Opened,
    /// CA_INFO arrived: state is INITIALIZED, CAPMT hashes must be forgotten, CAIDs registered.
    CaInfo { caids: Vec<u16> },
    /// The CA support session closed (`APP_CA_close`).
    CaSessionClosed,
    /// `ca_release_pmts` during teardown: forget every PMT held on this CAM.
    ReleasePmts,
    /// An MMI menu or list was shown and cancelled.
    MmiMenu { lines: Vec<String> },
    MmiEnquiry {
        text: String,
        blind: bool,
        answer_len: u8,
    },
    /// `check_new_key`: a descrambling key and its IV for `parity` (0 even,
    /// 1 odd). The bytes are never printed.
    CcKey {
        parity: u8,
        key: CcKeyBytes,
        iv: CcKeyBytes,
    },
    /// `disable_cws_for_all_pmts`: stop descrambling until the SAC resyncs.
    CcDisableCws,
}

/// AES key or IV carried by [`HostEvent::CcKey`]; zeroed on drop.
#[derive(Clone, PartialEq, Eq)]
pub struct CcKeyBytes(pub [u8; 16]);

impl fmt::Debug for CcKeyBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("CcKeyBytes(..)")
    }
}

impl Drop for CcKeyBytes {
    fn drop(&mut self) {
        self.0.fill(0);
        // Keeps the zeroing from being optimised away.
        std::hint::black_box(&self.0);
    }
}

/// An open session (`ca_session_t`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub number: u16,
    pub resource: u32,
    pub handler: &'static Handler,
    pub ai_version: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApduDir {
    HostToCam,
    CamToHost,
}

/// One APDU seen by the host, for traces and parity checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApduRecord {
    pub dir: ApduDir,
    pub session: u16,
    pub tag: u32,
    pub body: Vec<u8>,
}

/// One CI slot, host side.
pub struct CaDevice {
    pub id: usize,
    pub enabled: bool,
    pub state: CaState,
    pub mode: LinkMode,
    pub(super) link: Option<Box<dyn CaLink>>,
    pub(super) sock: Option<u64>,
    generation: u64,
    pub(super) caids: Vec<u16>,
    pub has_forced_caids: bool,
    pub(super) sessions: [Option<Session>; MAX_SESSIONS],
    pub uri_mask: i32,
    pub force_ci: bool,
    pub(super) ci_name: Vec<u8>,
    pub(super) pin: Vec<u8>,
    pub key: [[u8; 16]; 2],
    pub iv: [[u8; 16]; 2],
    /// Key register of the last `check_new_key` (0 even, 1 odd).
    pub parity: u8,
    #[cfg(feature = "ci-plus")]
    pub(crate) cc: Option<Box<super::ciplus::cc::CcState>>,
    #[cfg(feature = "ci-plus")]
    pub(super) ciplus_cert_dir: String,
    #[cfg(feature = "ci-plus")]
    pub(super) ciplus_cache_dir: String,
    pub datetime_response_interval: u64,
    pub datetime_next_send: i64,
    pub reconnect_next_try: i64,
    pub poll_fails: u32,
    pub pending_tag: u32,
    pub pending_session: u16,
    pub pending_since: i64,
    /// MMI message anchor of the CI+ key watchdog (ms; -1 unarmed).
    pub mmi_error_since: i64,
    /// Last CI+ key delivery (ms; -1 before the first key).
    pub last_key_ms: i64,
    /// CI+ key watchdog timeout (ms; 0 disables it). No C++ equivalent.
    pub recovery_timeout_ms: i64,
    close_requested: bool,
    events: VecDeque<HostEvent>,
    apdu_log: Option<Vec<ApduRecord>>,
    pub(super) clock: Arc<dyn Clock>,
}

impl CaDevice {
    /// `alloc_ca_device`: disabled, no handle.
    pub fn new(id: usize, clock: Arc<dyn Clock>) -> Self {
        Self {
            id,
            enabled: false,
            state: CaState::Inactive,
            mode: LinkMode::Tpdu,
            link: None,
            sock: None,
            generation: 0,
            caids: Vec::new(),
            has_forced_caids: false,
            sessions: [None; MAX_SESSIONS],
            uri_mask: 0,
            force_ci: false,
            ci_name: Vec::new(),
            pin: Vec::new(),
            key: [[0; 16]; 2],
            iv: [[0; 16]; 2],
            parity: 0,
            #[cfg(feature = "ci-plus")]
            cc: None,
            #[cfg(feature = "ci-plus")]
            ciplus_cert_dir: "/etc/ssl/certs".to_owned(),
            #[cfg(feature = "ci-plus")]
            ciplus_cache_dir: "/var/cache/minisatip".to_owned(),
            datetime_response_interval: 0,
            datetime_next_send: 0,
            reconnect_next_try: 0,
            poll_fails: 0,
            pending_tag: 0,
            pending_session: 0,
            pending_since: -1,
            mmi_error_since: -1,
            last_key_ms: -1,
            recovery_timeout_ms: 0,
            close_requested: false,
            events: VecDeque::new(),
            apdu_log: None,
            clock,
        }
    }

    pub(super) fn now(&self) -> i64 {
        self.clock.tick_ms()
    }

    pub fn has_link(&self) -> bool {
        self.link.is_some()
    }

    /// Replaces the device handle (`d->fd`).
    pub fn set_link(&mut self, link: Option<Box<dyn CaLink>>) {
        self.link = link;
    }

    /// The current socket generation (`d->sock`); `None` when closed.
    pub fn sock(&self) -> Option<u64> {
        self.sock
    }

    /// Opens a new socket generation, as `sockets_add` does.
    pub(super) fn new_sock(&mut self) -> u64 {
        self.generation += 1;
        self.sock = Some(self.generation);
        self.generation
    }

    pub fn caids(&self) -> &[u16] {
        &self.caids
    }

    /// `-ca-channels` CAIDs: kept over what the CAM reports.
    pub fn set_forced_caids(&mut self, caids: &[u16]) {
        self.caids = caids.iter().copied().take(MAX_CAID).collect();
        self.has_forced_caids = !self.caids.is_empty();
    }

    /// The CAM name from APPLICATION_INFO (`ci_name`), lossily decoded.
    pub fn ci_name(&self) -> String {
        String::from_utf8_lossy(&self.ci_name).into_owned()
    }

    pub fn ci_name_bytes(&self) -> &[u8] {
        &self.ci_name
    }

    pub fn set_ci_name(&mut self, name: &[u8]) {
        self.ci_name = name[..name.len().min(CI_NAME_MAX)].to_vec();
        #[cfg(feature = "ci-plus")]
        if let Some(cc) = self.cc.as_mut() {
            cc.set_ci_name(self.ci_name.clone());
        }
    }

    /// CI+ certificate (`root.pem`, ...) and authdata cache directories.
    #[cfg(feature = "ci-plus")]
    pub fn set_ciplus_paths(&mut self, cert_dir: &str, cache_dir: &str) {
        self.ciplus_cert_dir = cert_dir.to_owned();
        self.ciplus_cache_dir = cache_dir.to_owned();
        if let Some(cc) = self.cc.as_mut() {
            cc.set_paths(self.ciplus_cert_dir.clone(), self.ciplus_cache_dir.clone());
        }
    }

    /// `cc_ctrl_data`: created by `CIPLUS_APP_CC_create`, kept over teardown.
    #[cfg(feature = "ci-plus")]
    pub(crate) fn cc_state(&mut self) -> &mut super::ciplus::cc::CcState {
        let cert_dir = self.ciplus_cert_dir.clone();
        let cache_dir = self.ciplus_cache_dir.clone();
        let slot = self.id as u32;
        let ci_name = self.ci_name.clone();
        let uri_mask = self.uri_mask;
        self.cc.get_or_insert_with(|| {
            Box::new(super::ciplus::cc::CcState::new(
                cert_dir, cache_dir, slot, ci_name, uri_mask,
            ))
        })
    }

    /// `set_ca_pin`: at most 9 bytes are kept.
    pub fn set_pin(&mut self, pin: &[u8]) {
        self.pin = pin[..pin.len().min(PIN_MAX)].to_vec();
    }

    pub fn sessions(&self) -> impl Iterator<Item = &Session> {
        self.sessions.iter().flatten()
    }

    pub fn session(&self, number: u16) -> Option<&Session> {
        let idx = usize::from(number).checked_sub(1)?;
        self.sessions.get(idx)?.as_ref()
    }

    /// Records every APDU in and out from now on.
    pub fn record_apdus(&mut self, on: bool) {
        self.apdu_log = on.then(Vec::new);
    }

    pub fn take_apdu_log(&mut self) -> Vec<ApduRecord> {
        self.apdu_log
            .as_mut()
            .map(std::mem::take)
            .unwrap_or_default()
    }

    fn log_apdu(&mut self, dir: ApduDir, session: u16, tag: u32, body: &[u8]) {
        if let Some(log) = self.apdu_log.as_mut() {
            log.push(ApduRecord {
                dir,
                session,
                tag,
                body: body.to_vec(),
            });
        }
    }

    pub(super) fn push_event(&mut self, e: HostEvent) {
        self.events.push_back(e);
    }

    pub fn next_event(&mut self) -> Option<HostEvent> {
        self.events.pop_front()
    }

    /// `cam_state` mode: plain CI or CI+.
    pub fn mode_name(&self) -> &'static str {
        if self.force_ci { "ci" } else { "ci+" }
    }

    /// `ca_request_close`.
    pub fn request_close(&mut self) {
        ms_log::log!("Requesting CA {} close", self.id);
        ms_log::diag::events::cam_state(
            i64::try_from(self.id).unwrap_or(0),
            "reset",
            self.mode_name(),
        );
        self.close_requested = true;
        self.push_event(HostEvent::CloseRequested);
    }

    /// Whether a close was requested since the last call (`force_close`).
    pub fn take_close_request(&mut self) -> bool {
        std::mem::take(&mut self.close_requested)
    }

    /// `ca_reconnect_due`.
    pub fn reconnect_due(&self, now: i64) -> bool {
        self.enabled && self.link.is_none() && now >= self.reconnect_next_try
    }

    /// `ca_teardown`: drops volatile CAM state; `enabled` stays set.
    pub fn teardown(&mut self) {
        self.push_event(HostEvent::ReleasePmts);
        self.sessions = [None; MAX_SESSIONS];
        self.key = [[0; 16]; 2];
        self.iv = [[0; 16]; 2];
        self.caids.clear();
        self.poll_fails = 0;
        self.datetime_next_send = 0;
        self.pending_tag = 0;
        self.pending_session = 0;
        self.pending_since = -1;
        self.mmi_error_since = -1;
        self.last_key_ms = -1;
        self.link = None;
        self.sock = None;
        self.close_requested = false;
        self.state = CaState::Inactive;
    }

    /// `ca_close`: only the current socket generation may tear down.
    pub fn close(&mut self, sock: u64) -> bool {
        if self.sock != Some(sock) {
            return false;
        }
        ms_log::log!("closing CA device {} fd {}", self.id, -1);
        self.teardown();
        true
    }

    /// `ca_init_failed`: drops the handle and stays enabled for the reconnect poller.
    pub fn init_failed(&mut self) {
        self.link = None;
        self.sock = None;
        self.state = CaState::Inactive;
        self.enabled = true;
    }

    /// `ca_init_en50221`: reset, wait for READY, then T_CREATE_TC.
    pub(super) fn init_en50221(&mut self) -> bool {
        let start = self.now();
        let id = self.id;
        let Some(link) = self.link.as_deref_mut() else {
            return false;
        };
        let mut info = SlotInfo::default();
        let reset = reset_and_wait_ready(link, &mut info, id, CA_RESET_ATTEMPTS, CA_READY_POLLS);
        if reset == ResetResult::IoError {
            ms_log::log!("ca_init_en50221: io error on ca {}", id);
            return false;
        }
        ms_log::log!(
            "initializing CA {} fd {} type {} flags 0x{:x}, after {} ms",
            id,
            -1,
            info.kind,
            info.flags,
            self.now() - start
        );
        if info.kind != CA_CI_LINK {
            ms_log::log!("incompatible CA interface");
            return false;
        }
        if info.flags & CA_CI_MODULE_READY == 0 {
            ms_log::log!("CA module not present or not ready");
            return false;
        }
        self.write_tpdu(T_CREATE_TC, &[]);
        self.new_sock();
        self.pending_since = self.now();
        self.mmi_error_since = -1;
        self.last_key_ms = -1;
        true
    }

    /// `ca_init_enigma`.
    pub(super) fn init_enigma(&mut self) -> bool {
        let sock = self.new_sock();
        self.pending_since = self.now();
        self.mmi_error_since = -1;
        self.last_key_ms = -1;
        ms_log::log!("initializing CA {} fd {} sock {}", self.id, -1, sock);
        true
    }

    /// `ca_slot_present`: an ioctl error lets the full init decide.
    pub(super) fn slot_present(&mut self) -> bool {
        match self.link.as_deref_mut().map(|l| l.slot_info()) {
            Some(Ok(info)) => info.has_module(),
            _ => true,
        }
    }

    fn write_raw(&mut self, buf: &[u8], eio_closes: bool) -> bool {
        let rc = match self.link.as_deref_mut() {
            Some(l) => l.write(buf),
            None => Err(io::Error::from(io::ErrorKind::NotConnected)),
        };
        match rc {
            Ok(n) if n == buf.len() => true,
            Ok(n) => {
                // C++ would print the stale errno; a short write has none.
                ms_log::log!(
                    "incomplete write to CA {} fd {} expected {} got {}, errno {} {}",
                    self.id,
                    -1,
                    buf.len(),
                    n,
                    0,
                    "Success"
                );
                false
            }
            Err(e) => {
                ms_log::log!(
                    "incomplete write to CA {} fd {} expected {} got {}, errno {} {}",
                    self.id,
                    -1,
                    buf.len(),
                    -1,
                    ms_log::errno(&e),
                    ms_log::strerror(&e)
                );
                if eio_closes && e.raw_os_error() == Some(EIO) {
                    self.request_close();
                }
                false
            }
        }
    }

    /// `ca_write_tpdu` on slot 0, connection 1; true when it went out.
    pub fn write_tpdu(&mut self, tag: u8, data: &[u8]) -> bool {
        if self.mode == LinkMode::Enigma {
            return self.write_raw(data, false);
        }
        let mut p = vec![0, 1, tag];
        match tag {
            T_RCV | T_CREATE_TC | T_CTC_REPLY | T_DELETE_TC | T_DTC_REPLY | T_REQUEST_TC => {
                p.extend_from_slice(&[1, 1]);
            }
            T_NEW_TC | T_TC_ERROR => {
                p.extend_from_slice(&[2, 1, data.first().copied().unwrap_or(0)])
            }
            T_DATA_LAST | T_DATA_MORE => {
                encode_len(data.len() + 1, &mut p);
                p.push(1);
                p.extend_from_slice(data);
            }
            _ => p.clear(),
        }
        if tag != T_DATA_LAST || !data.is_empty() {
            ms_log::hexdump!("WRITE TPDU: ", &p);
        }
        self.write_raw(&p, true)
    }

    /// `ca_write_spdu`; a session number of -1 goes out as 0xFFFF.
    pub fn write_spdu(&mut self, session_number: i32, tag: u8, data: &[u8], apdu: &[u8]) -> bool {
        let mut pkt = vec![tag];
        encode_len(data.len() + 2, &mut pkt);
        pkt.extend_from_slice(data);
        pkt.extend_from_slice(&(session_number as u16).to_be_bytes());
        pkt.extend_from_slice(apdu);
        let name = if session_number > 0 {
            self.sessions
                .get(session_number as usize - 1)
                .and_then(|s| *s)
                .map_or("(null)", |s| s.handler.name)
        } else {
            "none"
        };
        ms_log::debugm!(
            "ca_write_spdu: CA {}, session {}, name {}, write tag {:02X}, data length {}",
            self.id,
            session_number,
            name,
            tag,
            pkt.len()
        );
        if !pkt.is_empty() {
            ms_log::hexdump!("WRITE SPDU: ", &pkt);
        }
        self.write_tpdu(T_DATA_LAST, &pkt)
    }

    /// `ca_write_apdu` on the session at `idx`; a successful tracked command arms the watchdog.
    pub fn write_apdu(&mut self, idx: usize, tag: u32, body: &[u8]) -> bool {
        let Some(session) = self.sessions.get(idx).copied().flatten() else {
            return false;
        };
        let number = session.number;
        let mut pkt = tag.to_be_bytes()[1..].to_vec();
        encode_len(body.len(), &mut pkt);
        pkt.extend_from_slice(body);
        ms_log::log!(
            "ca_write_apdu: CA {}, session {}, name {}, write tag {:06X}, data length {}",
            self.id,
            number,
            session.handler.name,
            tag,
            body.len()
        );
        self.log_apdu(ApduDir::HostToCam, number, tag, body);
        let ok = self.write_spdu(i32::from(number), ST_SESSION_NUMBER, &[], &pkt);
        if ok {
            self.track_command(number, tag);
        }
        ok
    }

    fn track_command(&mut self, number: u16, tag: u32) {
        if !cmd_tracked(tag) || (self.pending_tag == tag && self.pending_session == number) {
            return;
        }
        self.pending_tag = tag;
        self.pending_session = number;
        self.pending_since = self.now();
    }

    fn match_reply(&mut self, number: u16, tag: u32) {
        if self.pending_tag != 0
            && number == self.pending_session
            && cmd_reply(self.pending_tag, tag)
        {
            self.clear_pending();
        }
    }

    fn clear_pending(&mut self) {
        self.pending_tag = 0;
        self.pending_session = 0;
    }

    /// `ca_read_tpdu`: one read of the device, answering CTC_REPLY and T_SB on the spot.
    pub fn read_tpdu(&mut self) -> ReadOutcome {
        let Some(link) = self.link.as_deref_mut() else {
            return ReadOutcome::Eof;
        };
        let mut data = [0u8; MAX_TPDU];
        let len = match link.read(&mut data) {
            Ok(0) => return ReadOutcome::Eof,
            Ok(n) => n.min(MAX_TPDU),
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                ) =>
            {
                return ReadOutcome::Again;
            }
            Err(_) => return ReadOutcome::Eof,
        };
        self.poll_fails = 0;
        if !(len == 6 && data[5] == 0) {
            ms_log::hexdump!("READ TPDU: ", &data[..len]);
        }
        let mut out = Vec::new();
        let mut d = &data[len.min(2)..len];
        while let Some(&tag) = d.first() {
            let Some((asn_len, lfl)) = decode_len(&d[1..]) else {
                ms_log::log!(
                    "Received data with invalid asn from module on device {}",
                    self.id
                );
                ms_log::hexdump(Some("INVALID LENGTH: "), &data[..len]);
                break;
            };
            if asn_len < 1 || asn_len > d.len() - (1 + lfl) {
                ms_log::log!(
                    "Received data with invalid length from module on device {}",
                    self.id
                );
                ms_log::hexdump(Some("INVALID LENGTH: "), &data[..len]);
                break;
            }
            if out.len() + asn_len > MAX_TPDU {
                // C++ prints the socket buffer bound (`*rb + buf_len`).
                ms_log::log!(
                    "Received data for CA {} larger than socket buffer (len {})",
                    self.id,
                    MAX_TPDU
                );
                break;
            }
            let body = &d[2 + lfl..1 + lfl + asn_len];
            match tag {
                T_CTC_REPLY => {
                    ms_log::debugm!("Got CTC Replay (CA {})", self.id);
                    if self.state == CaState::Inactive {
                        self.state = CaState::Active;
                    }
                    self.write_tpdu(T_DATA_LAST, &[]);
                }
                T_DELETE_TC => {
                    ms_log::log!("Got \"Delete Transport Connection\" from module ");
                    return ReadOutcome::Eof;
                }
                T_DTC_REPLY => {
                    ms_log::log!("Got \"Delete Transport Connection Replay\" from module!\n");
                }
                T_REQUEST_TC => {
                    ms_log::log!(
                        "Got \"Request Transport Connection\" from Module ->currently not handled!"
                    );
                }
                T_DATA_MORE | T_DATA_LAST => out.extend_from_slice(body),
                T_SB => {
                    if body.first().is_some_and(|b| b & 0x80 != 0) {
                        self.write_tpdu(T_RCV, &[]);
                    }
                }
                _ => ms_log::log!("unhandled tpdu_tag 0x{:x}\n", tag),
            }
            d = &d[1 + lfl + asn_len..];
        }
        ReadOutcome::Data(out)
    }

    /// `ca_read_enigma`: SPDUs straight from the device; POLLPRI without data is a removal.
    pub fn read_enigma(&mut self, pri: bool) -> ReadOutcome {
        let Some(link) = self.link.as_deref_mut() else {
            return ReadOutcome::Eof;
        };
        let mut buf = [0u8; MAX_TPDU];
        if let Ok(n) = link.read(&mut buf)
            && n > 0
        {
            return ReadOutcome::Data(buf[..n].to_vec());
        }
        if pri && self.state != CaState::Inactive {
            self.request_close();
        }
        ReadOutcome::Again
    }

    /// Runs the read callback and `ca_read` until the device has nothing left; false on EOF.
    pub fn service(&mut self) -> bool {
        loop {
            let r = match self.mode {
                LinkMode::Tpdu => self.read_tpdu(),
                LinkMode::Enigma => self.read_enigma(false),
            };
            match r {
                ReadOutcome::Data(d) if !d.is_empty() => self.handle_spdu(&d),
                ReadOutcome::Data(_) => {}
                ReadOutcome::Again => return true,
                ReadOutcome::Eof => return false,
            }
        }
    }

    /// `new_session`: refuses resources without a handler and a full table.
    fn new_session(&mut self, resource: u32) -> Option<usize> {
        let Some(handler) = find_handler(resource) else {
            ms_log::log!("CA {}: resource {:06X} is not supported", self.id, resource);
            return None;
        };
        let Some(idx) = self.sessions.iter().position(Option::is_none) else {
            ms_log::log!("Could not allocate a new session id for CA {}", self.id);
            return None;
        };
        self.sessions[idx] = Some(Session {
            number: idx as u16 + 1,
            resource,
            handler,
            ai_version: 0,
        });
        Some(idx)
    }

    /// `find_session_for_resource`; like C++ it never looks at session 1.
    pub fn find_session_for_resource(&self, resource: u32) -> Option<usize> {
        (1..MAX_SESSIONS).find(|&i| self.sessions[i].is_some_and(|s| s.resource == resource))
    }

    /// `ca_read`: one SPDU from the CAM.
    pub fn handle_spdu(&mut self, data: &[u8]) {
        ms_log::hexdump!("CAREAD", data);
        let Some(&tag) = data.first() else {
            return;
        };
        if self.state == CaState::Inactive {
            self.state = CaState::Active;
        }
        self.poll_fails = 0;
        let Some((len, llen)) = decode_len(&data[1..]) else {
            ms_log::log!(
                "Invalid SPDU length {:02X} {:02X} {:02X}",
                data.get(1).copied().unwrap_or(0),
                data.get(2).copied().unwrap_or(0),
                data.get(3).copied().unwrap_or(0)
            );
            return;
        };
        let start = 1 + llen + len;
        match tag {
            ST_OPEN_SESSION_REQUEST => {
                let Some(r) = data.get(2..6) else {
                    return;
                };
                let resource = u32::from_be_bytes([r[0], r[1], r[2], r[3]]);
                let idx = self.new_session(resource);
                let status = if idx.is_some() { 0 } else { SS_NOT_FOUND };
                let number = idx.map_or(-1, |i| i as i32 + 1);
                let name = idx
                    .and_then(|i| self.sessions[i])
                    .map_or("none", |s| s.handler.name);
                ms_log::log!(
                    "Found session_number {} for resource {:06X}, name {}, status {:02X}",
                    number,
                    resource,
                    name,
                    status
                );
                let mut pkt = vec![status];
                pkt.extend_from_slice(&resource.to_be_bytes());
                self.write_spdu(number, ST_OPEN_SESSION_RESPONSE, &pkt, &[]);
                if let Some(idx) = idx {
                    self.app_create(idx, resource);
                }
            }
            ST_CLOSE_SESSION_REQUEST => {
                let Some(b) = data.get(2..4) else {
                    return;
                };
                let number = u16::from_be_bytes([b[0], b[1]]);
                if number < 1 || usize::from(number) > MAX_SESSIONS {
                    ms_log::log!("CA {}: bad session {}, dropping", self.id, number);
                    return;
                }
                let idx = usize::from(number) - 1;
                // C++ prints a zeroed `handler.name` as glibc's "(null)".
                let name = self.sessions[idx].map_or("(null)", |s| s.handler.name);
                ms_log::log!("Received close session {}", name);
                if self.sessions[idx].is_some() {
                    self.app_close(idx);
                }
                if self.pending_session == number {
                    self.clear_pending();
                }
                if self.pending_tag == 0 {
                    self.pending_since = self.now();
                }
                self.sessions[idx] = None;
                let mut pkt = vec![0];
                pkt.extend_from_slice(&number.to_be_bytes());
                self.write_spdu(i32::from(number), ST_CLOSE_SESSION_RESPONSE, &pkt, &[]);
            }
            ST_SESSION_NUMBER => {
                let Some(b) = data.get(1 + llen..3 + llen) else {
                    return;
                };
                let number = u16::from_be_bytes([b[0], b[1]]);
                ms_log::debugm!("got ST_SESSION_NUMBER for session_number {}", number);
                if number < 1 || usize::from(number) > MAX_SESSIONS {
                    ms_log::log!("CA {}: bad session {}, dropping", self.id, number);
                    return;
                }
                let idx = usize::from(number) - 1;
                if self.sessions[idx].is_some() {
                    self.read_apdu(idx, data.get(start..).unwrap_or(&[]));
                } else {
                    ms_log::log!("CA {}: session {} is not registered", self.id, number);
                }
            }
            _ => {}
        }
    }

    /// `ca_read_apdu`: every APDU of the SPDU goes to the session's handler.
    pub fn read_apdu(&mut self, idx: usize, mut data: &[u8]) {
        ms_log::hexdump!("APDU", data);
        while data.len() >= 4 {
            let Some((len, llen)) = decode_len(&data[3..]) else {
                ms_log::log!(
                    "Invalid APDU length {:02X} {:02X} {:02X}",
                    data.get(3).copied().unwrap_or(0),
                    data.get(4).copied().unwrap_or(0),
                    data.get(5).copied().unwrap_or(0)
                );
                return;
            };
            let tag = u32::from_be_bytes([0, data[0], data[1], data[2]]);
            let Some(body) = data.get(3 + llen..3 + llen + len) else {
                return;
            };
            let Some(session) = self.sessions[idx] else {
                return;
            };
            let number = session.number;
            ms_log::log!(
                "ca_read_apdu: CA {}, session {}, name {}, read tag {:06X}, data length {}",
                self.id,
                number,
                session.handler.name,
                tag,
                len
            );
            if len > 0 {
                ms_log::hexdump!("data: ", body);
            }
            self.log_apdu(ApduDir::CamToHost, number, tag, body);
            self.match_reply(number, tag);
            self.app_apdu(idx, tag, body);
            data = &data[3 + llen + len..];
        }
    }

    /// `ca_timeout`, run every second: keepalive poll, DATE_TIME resend, watchdog.
    pub fn timeout(&mut self) {
        if self.mode == LinkMode::Tpdu {
            let ok = match self.state {
                CaState::Inactive => self.write_tpdu(T_CREATE_TC, &[]),
                _ => self.write_tpdu(T_DATA_LAST, &[]),
            };
            if ok {
                self.poll_fails = 0;
            } else {
                self.poll_fails += 1;
                if self.poll_fails >= CA_MAX_POLL_FAILS {
                    self.poll_fails = 0;
                    self.request_close();
                }
            }
        }
        if self.state == CaState::Initialized
            && self.datetime_response_interval != 0
            && self.now() > self.datetime_next_send
        {
            self.send_datetime();
        }
        let now = self.now();
        self.cmd_watchdog(now);
        self.mmi_recovery_watchdog(now);
    }

    /// `ca_cmd_watchdog`: resets a CAM that leaves a command or its initialization hanging.
    pub fn cmd_watchdog(&mut self, now: i64) {
        if self.pending_since < 0 || now - self.pending_since < CA_CMD_TIMEOUT_MS {
            return;
        }
        if self.pending_tag != 0 {
            ms_log::log!(
                "CA {}: command {:06X} unanswered after {} ms, resetting",
                self.id,
                self.pending_tag,
                now - self.pending_since
            );
        } else if self.state != CaState::Initialized {
            ms_log::log!(
                "CA {}: initialization stalled after {} ms, resetting",
                self.id,
                now - self.pending_since
            );
        } else {
            return;
        }
        self.clear_pending();
        self.pending_since = -1;
        self.request_close();
    }

    /// CI+ recovery: a CAM that sent an MMI message and then stopped delivering
    /// keys is wedged; reset it. No C++ equivalent, disabled unless configured.
    pub fn mmi_recovery_watchdog(&mut self, now: i64) {
        if self.mmi_error_since < 0 {
            return;
        }
        if self.last_key_ms > self.mmi_error_since {
            self.mmi_error_since = -1;
            return;
        }
        if now - self.mmi_error_since < self.recovery_timeout_ms {
            return;
        }
        ms_log::log!(
            "CA {}: no CI+ key {} ms after a CAM MMI message, resetting the CAM (CI+ recovery)",
            self.id,
            now - self.mmi_error_since
        );
        ms_log::diag::events::cam_state(
            i64::try_from(self.id).unwrap_or(0),
            "mmi_recovery",
            self.mode_name(),
        );
        self.mmi_error_since = -1;
        self.request_close();
    }
}

/// Hook the CA reconciler (M6) calls to hand CA_PMT bodies to this CAM.
pub trait CaPmtSink {
    /// A CA support session is open (`find_session_for_resource`).
    fn has_ca_session(&self) -> bool;
    /// Writes one CA_PMT APDU body; false when there is no session or the write failed.
    fn send_ca_pmt(&mut self, body: &[u8]) -> bool;
}

impl CaPmtSink for CaDevice {
    fn has_ca_session(&self) -> bool {
        self.find_session_for_resource(EN50221_APP_CA_RESOURCEID)
            .is_some()
    }

    fn send_ca_pmt(&mut self, body: &[u8]) -> bool {
        match self.find_session_for_resource(EN50221_APP_CA_RESOURCEID) {
            Some(idx) => self.write_apdu(idx, TAG_CA_PMT, body),
            None => false,
        }
    }
}
