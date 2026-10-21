//! Sans-IO port of the satipc RTSP state machine (`satipc.cpp`).
//!
//! [`Client`] holds the `satipc` record plus the adapter fields it touches.
//! Requests and socket actions are queued as [`Op`]s for a driver to execute.

use std::collections::{BTreeSet, VecDeque};
use std::fmt::Write as _;
use std::time::Instant;

use super::util::{
    c_text, delsys_code, delsys_name, header_parameter, is_rtsp_response, parse_int, split,
    starts_with_ci,
};
use crate::tuner::{DeliverySystem, PID_ALL, PidSet, Polarization, TuneParams};

/// `satipc.cpp`'s `#define DEFAULT_LOG LOG_SATIPC`.
const DEFAULT_LOG: u32 = ms_log::LOG_SATIPC;

/// `SATIP_MAX_STRENGTH`.
pub const MAX_STRENGTH: i32 = 255;
/// `SATIP_MAX_QUALITY`.
pub const MAX_QUALITY: i32 = 15;
/// `MAX_PIDS` adapter slots.
pub const MAX_PIDS: usize = 128;
/// `QAM_AUTO` and `QAM_256` (`fe_modulation_t`).
const QAM_AUTO: u32 = 6;
const QAM_256: u32 = 5;
/// Offset that keeps ticks positive like the C++ boot-relative `getTick`.
const TICK_BASE_MS: i64 = 1_000_000;

/// Session state (`SATIP_STATE_*`); `DISCONNECTED` and `INACTIVE` share 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum State {
    /// `SATIP_STATE_INACTIVE` / `SATIP_STATE_DISCONNECTED`.
    #[default]
    Inactive,
    /// `SATIP_STATE_SETUP`.
    Setup,
    /// `SATIP_STATE_PLAY`.
    Play,
    /// `SATIP_STATE_TEARDOWN`.
    Teardown,
}

impl State {
    /// `SATIP_STATE_DISCONNECTED`, the same value as `Inactive`.
    pub const DISCONNECTED: State = State::Inactive;
}

/// Media transport (`satipc_transport_type`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Transport {
    /// RTP over UDP.
    #[default]
    Udp,
    /// RTP interleaved on the RTSP connection.
    Tcp,
    /// TS over SRT from the server's RTSP port (`^` servers).
    #[cfg(feature = "srt")]
    Srt,
}

impl Transport {
    /// `str_transport_type`.
    pub fn name(self) -> &'static str {
        match self {
            Self::Udp => "UDP",
            Self::Tcp => "TCP",
            #[cfg(feature = "srt")]
            Self::Srt => "SRT",
        }
    }
}

/// Last request sent (`RTSP_*` constants).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LastCmd {
    /// No request yet.
    #[default]
    None,
    /// `RTSP_SETUP`.
    Setup,
    /// `RTSP_PLAY`.
    Play,
    /// `RTSP_OPTIONS`.
    Options,
    /// `RTSP_TEARDOWN`.
    Teardown,
    /// `RTSP_DESCRIBE`.
    Describe,
}

/// Work queued for the driver, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    /// Writes a request to the RTSP connection.
    Send(Vec<u8>),
    /// `sockets_del(ad->sock)`: closes the media connection.
    CloseData,
    /// `close_adapter`: closes the whole device.
    CloseAdapter,
    /// `satipc_open_rtsp_socket(ad, sip, false)`: reconnects.
    OpenRtsp,
    /// `satipc_open_srt` before the first SETUP/PLAY; on success the driver
    /// runs [`Client::request`] again.
    #[cfg(feature = "srt")]
    OpenSrt,
    /// The SRT branch of `satipc_timeout`: reopens a broken SRT connection.
    #[cfg(feature = "srt")]
    RestartSrt,
}

/// Adapter PID slot state (`PID_STATE_*`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PidState {
    /// `PID_STATE_NEW`.
    New,
    /// `PID_STATE_ACTIVE`.
    Active,
    /// `PID_STATE_DELETED`.
    Deleted,
}

/// One `ad->pids[]` slot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PidSlot {
    /// PID value.
    pub pid: u16,
    /// Slot state.
    pub flags: PidState,
    /// Whether `set_pid` returned a filter (`fd > 0`).
    pub fd: bool,
}

/// Result of feeding RTSP bytes to [`Client::reply`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReplyOutcome {
    /// Nothing buffered (`rlen == 0`).
    Empty,
    /// The header or body is not complete yet; keep buffering.
    Incomplete,
    /// The response was processed and the buffer is consumed.
    Handled,
}

/// The `satipc` record with the adapter fields used by satipc.cpp.
#[derive(Debug, Clone)]
pub struct Client {
    /// Adapter id (`ad->id`, `sip->id`).
    pub id: usize,
    /// `sip->enabled`; disabled clients ignore every call (`get_satip` is NULL).
    pub enabled: bool,
    /// `ad->err`.
    pub err: bool,
    /// `ad->tp`.
    pub tp: TuneParams,
    /// `ad->sys[0]`, which selects the URL builder.
    pub sys0: DeliverySystem,
    /// `ad->sid_cnt`.
    pub sid_cnt: i32,
    /// `ad->strength`.
    pub strength: i32,
    /// `ad->status` has `FE_HAS_LOCK`.
    pub status: bool,
    /// `ad->snr`.
    pub snr: i32,
    /// Both signal multipliers are 0: report maximum signal.
    pub ignore_signal: bool,
    /// `ad->pids[]`, in slot order.
    pub pids: Vec<Option<PidSlot>>,
    /// Server host (`sip->sip`).
    pub host: String,
    /// Server RTSP port (`sip->sport`).
    pub port: u16,
    /// Per-server source address (`sip->source_ip`).
    pub source_ip: String,
    /// `sip->satip_fe`.
    pub satip_fe: i32,
    /// `sip->transport_type`.
    pub transport: Transport,
    /// `sip->no_pids_all`.
    pub no_pids_all: bool,
    /// PIDs to add (`apid`, length `lap`).
    pub apid: Vec<u16>,
    /// PIDs to delete (`dpid`, length `ldp`).
    pub dpid: Vec<u16>,
    /// `sip->session`.
    pub session: String,
    /// `sip->stream_id`.
    pub stream_id: i32,
    /// `sip->listen_udp`.
    pub listen_udp: u16,
    /// `sip->cseq`.
    pub cseq: u32,
    /// `sip->ignore_packets`.
    pub ignore_packets: bool,
    /// `ad->wait_new_stream`: discard data until the new transponder appears.
    pub wait_new_stream: bool,
    /// `sip->last_cmd`.
    pub last_cmd: LastCmd,
    /// `sip->state`.
    pub state: State,
    /// `sip->last_setup`.
    pub last_setup: i64,
    /// `sip->last_connect`.
    pub last_connect: i64,
    /// `sip->last_close`.
    pub last_close: i64,
    /// `sip->last_response_sent`.
    pub last_response_sent: i64,
    /// `sip->option_no_setup`.
    pub option_no_setup: bool,
    /// `sip->rcvp`.
    pub rcvp: u32,
    /// `sip->repno`.
    pub repno: u32,
    /// `sip->rtp_miss`.
    pub rtp_miss: u32,
    /// `sip->rtp_ooo`.
    pub rtp_ooo: u32,
    /// `sip->rtp_seq`.
    pub rtp_seq: u16,
    /// `sip->num_describe`.
    pub num_describe: i32,
    /// `sip->timeout_ms`.
    pub timeout_ms: i64,
    /// `sip->rtsp_socket_closed`.
    pub rtsp_socket_closed: bool,
    /// `sip->keep_adapter_open`.
    pub keep_adapter_open: bool,
    /// `sip->can_keep_adapter_open`.
    pub can_keep_adapter_open: bool,
    /// `sip->restart_when_tune`.
    pub restart_when_tune: bool,
    /// `sip->restart_needed`.
    pub restart_needed: bool,
    /// `sip->expect_reply`.
    pub expect_reply: bool,
    /// `sip->want_commit`.
    pub want_commit: bool,
    /// `sip->want_tune`.
    pub want_tune: bool,
    /// `sip->force_pids`.
    pub force_pids: bool,
    /// `sip->sent_transport`.
    pub sent_transport: bool,
    /// The remote socket exists (`fe_sock`/`ad->sock >= 0`).
    pub rtsp_connected: bool,
    /// The media socket exists (`ad->sock >= 0`).
    pub data_open: bool,
    /// `sockets_timeout(ad->fe_sock, ...)` in ms.
    pub fe_timeout_ms: i64,
    /// `sip->srt_streamid`, advertised in the SETUP transport.
    #[cfg(feature = "srt")]
    pub srt_streamid: String,
    /// `srt_socket_is_connected(sip->srt_sock)`, refreshed by the driver.
    #[cfg(feature = "srt")]
    pub srt_connected: bool,
    /// `sip->srt_restart_claimed`.
    #[cfg(feature = "srt")]
    pub srt_restart_claimed: bool,
    /// `app_name version` sent as `User-Agent`.
    pub user_agent: String,
    /// Queued driver work.
    pub ops: VecDeque<Op>,
    origin: Instant,
}

impl Client {
    /// A configured, not yet opened client (`add_satip_server`).
    pub fn new(id: usize, host: &str, port: u16) -> Self {
        Self {
            id,
            enabled: false,
            err: false,
            tp: TuneParams::default(),
            sys0: DeliverySystem::DvbS2,
            sid_cnt: 0,
            strength: 0,
            status: false,
            snr: 0,
            ignore_signal: false,
            pids: Vec::new(),
            host: host.to_owned(),
            port,
            source_ip: String::new(),
            satip_fe: -1,
            transport: Transport::Udp,
            no_pids_all: false,
            apid: Vec::new(),
            dpid: Vec::new(),
            session: String::new(),
            stream_id: -1,
            listen_udp: 0,
            cseq: 1,
            ignore_packets: false,
            wait_new_stream: false,
            last_cmd: LastCmd::None,
            state: State::Inactive,
            last_setup: 0,
            last_connect: 0,
            last_close: 0,
            last_response_sent: 0,
            option_no_setup: false,
            rcvp: 0,
            repno: 0,
            rtp_miss: 0,
            rtp_ooo: 0,
            rtp_seq: 0xFFFF,
            num_describe: 0,
            timeout_ms: 30_000,
            rtsp_socket_closed: false,
            keep_adapter_open: false,
            can_keep_adapter_open: false,
            restart_when_tune: false,
            restart_needed: false,
            expect_reply: false,
            want_commit: false,
            want_tune: false,
            force_pids: false,
            sent_transport: false,
            rtsp_connected: false,
            data_open: false,
            fe_timeout_ms: 25_000,
            #[cfg(feature = "srt")]
            srt_streamid: String::new(),
            #[cfg(feature = "srt")]
            srt_connected: false,
            #[cfg(feature = "srt")]
            srt_restart_claimed: false,
            user_agent: format!("minisatip {}", env!("CARGO_PKG_VERSION")),
            ops: VecDeque::new(),
            origin: Instant::now(),
        }
    }

    /// Monotonic milliseconds (`getTick`).
    pub fn tick(&self) -> i64 {
        TICK_BASE_MS + self.origin.elapsed().as_millis() as i64
    }

    /// Drains queued requests, dropping other ops (test helper).
    pub fn take_sent(&mut self) -> Vec<Vec<u8>> {
        self.ops
            .drain(..)
            .filter_map(|op| match op {
                Op::Send(data) => Some(data),
                _ => None,
            })
            .collect()
    }

    fn handle_client_capabilities(&mut self, text: &str) {
        if text.contains("minisatip") {
            self.option_no_setup = true;
        }
        if text.contains("enigma") && !self.restart_when_tune {
            // satipc.cpp:137-139
            ms_log::log!(
                "Setting adapter {} to restart every time the transponder is changed",
                self.id
            );
            self.restart_when_tune = true;
            if self.transport == Transport::Udp {
                self.transport = Transport::Tcp;
                self.restart_needed = true;
                // satipc.cpp:144
                ms_log::log!("adapter {} is not RTSP over TCP, switching", self.id);
            }
            self.option_no_setup = true;
        }
    }

    // Fatal rejection: release the server session and park the adapter.
    fn give_up(&mut self, rc: i32) {
        if self.stream_id != -1 {
            self.state = State::Teardown;
            self.send_teardown();
        } else {
            self.state = State::Inactive;
        }
        self.err = true;
        // satipc.cpp:168
        ms_log::log!("satipc {}: request failed fatally (rc {})", self.id, rc);
    }

    fn handle_setup(&mut self, text: &str) {
        if let Some(pos) = text.find("timeout=") {
            let tmout = i64::from(parse_int(&text[pos + 8..], 30));
            self.fe_timeout_ms = tmout * 500;
            self.timeout_ms = tmout * 1000;
        }
        let args = split(text, ' ');
        let (mut sess, mut sid) = ("", "");
        for i in 0..args.len() {
            if starts_with_ci(args[i], "Session:") {
                sess = header_parameter(&args, i);
            } else if starts_with_ci(args[i], "com.ses.streamID:") {
                sid = header_parameter(&args, i);
            }
        }
        if !self.err && !sid.is_empty() && !sess.is_empty() {
            let sess = sess.split(';').next().unwrap_or("");
            self.session = sess.chars().take(17).collect();
            if self.stream_id == -1 {
                self.stream_id = parse_int(sid, 0);
            }
            // satipc.cpp:203-204
            ms_log::log!(
                "satipc: session set for adapter {} to {} with stream_id {}",
                self.id,
                self.session,
                self.stream_id
            );
        }
    }

    fn handle_play(&mut self) {
        if self.ignore_packets {
            // satipc.cpp:213
            ms_log::log!("accepting packets from RTSP server");
            self.ignore_packets = false;
            // src_satipc.cpp:211-216: flush until the new transponder shows up.
            self.wait_new_stream = true;
        }
        if !self.want_tune && self.last_cmd == LastCmd::Play {
            self.can_keep_adapter_open = true;
        }
    }

    /// `satipc_reply`: handles one RTSP response.
    pub fn reply(&mut self, buf: &[u8]) -> ReplyOutcome {
        if buf.is_empty() {
            return ReplyOutcome::Empty;
        }
        if !self.enabled {
            return ReplyOutcome::Handled;
        }
        if !is_rtsp_response(buf) {
            // satipc.cpp:238-239; socket ids have no equivalent.
            let len = buf.iter().position(|b| *b == 0).unwrap_or(buf.len());
            ms_log::log!(
                "satipc: RTSP header not complete: {} len {}\n{}",
                0,
                len,
                String::from_utf8_lossy(&buf[..len])
            );
            return ReplyOutcome::Incomplete;
        }
        let text = c_text(buf);
        self.keep_adapter_open = false;
        let rc = text
            .find("RTSP/1.0")
            .map_or(0, |pos| parse_int(text.get(pos + 9..).unwrap_or(""), 0));
        let is_transport = text.contains("Transport:");
        // satipc.cpp:250-253, 254
        ms_log::log!(
            "satipc_reply (adapter {}): sock {} (receiving from handle {}, state {}): [[ code {} ]]",
            self.id,
            0,
            0,
            self.state as u8,
            rc
        );
        ms_log::logm!("{}", text);
        self.handle_client_capabilities(&text);
        if self.last_cmd == LastCmd::Describe {
            self.set_adapter_signal(&text);
        }
        self.expect_reply = false;

        // Fritzbox did reply 408 when mtype is missing
        if rc == 408 {
            self.state = State::Setup;
            self.force_pids = true;
            self.last_setup = -10_000;
            if !self.tp.mtype_specified || self.tp.mtype == QAM_AUTO {
                self.tp.mtype = QAM_256;
                self.tp.mtype_specified = true;
            }
        }
        if rc == 404 {
            self.state = if self.err {
                State::Inactive
            } else {
                State::Setup
            };
            if !self.err {
                self.force_pids = true;
            }
        }
        // quirk for Geniatech EyeTV Netstream 4C when fe=x is in the URL
        if rc == 503 && matches!(self.sys0, DeliverySystem::DvbC | DeliverySystem::DvbC2) {
            self.satip_fe = 0;
        }
        if rc == 454 {
            self.state = if self.err {
                State::Inactive
            } else {
                State::Setup
            };
            if !self.err {
                self.force_pids = true;
            }
        } else if rc == 403 || rc == 500 {
            if rc == 403 {
                // satipc.cpp:301
                if let Some(oor) = body_param(&text, "Out-of-Range:") {
                    let attrs = oor.split(['\r', '\n']).next().unwrap_or("");
                    ms_log::log!("satipc {}: server reports {}", self.id, attrs);
                }
            }
            self.give_up(rc);
        } else if rc == 503 || rc == 405 || rc == 400 {
            let no_more_pids =
                rc == 503 && body_param(&text, "No-More:").is_some_and(|v| v.contains("pids"));
            if no_more_pids {
                self.give_up(rc);
            } else {
                if self.err {
                    self.state = State::Inactive;
                } else if self.stream_id != -1 {
                    self.state = State::Teardown;
                    self.send_teardown();
                } else {
                    self.state = State::Setup;
                }
                if !self.err {
                    self.force_pids = true;
                }
                // satipc.cpp:324
                ms_log::log!("satipc {}: request rejected (rc {}), retrying", self.id, rc);
            }
        } else if rc != 200 {
            if rc != 0 {
                self.err = true;
                // satipc.cpp:329
                ms_log::log!("marking device {} as error, rc = {}", self.id, rc);
            } else {
                // satipc.cpp:332-334
                ms_log::log!(
                    "marking device {} not as error but reply is unexpected, rc = {}",
                    self.id,
                    rc
                );
            }
        }

        // satipc.cpp:336-339
        ms_log::log!(
            "satipc {}, expect_reply {}, want_tune {} want commit {}, state {}, is_transport {}",
            self.id,
            i32::from(self.expect_reply),
            i32::from(self.want_tune),
            i32::from(self.want_commit),
            self.state as u8,
            i32::from(is_transport)
        );

        if rc == 200 {
            if is_transport {
                self.handle_setup(&text);
            }
            match self.state {
                State::Setup if self.last_cmd == LastCmd::Setup => self.state = State::Play,
                State::Play => self.handle_play(),
                State::Teardown if self.last_cmd == LastCmd::Teardown => {
                    self.state = State::Inactive
                }
                _ => {}
            }
        }
        self.request();
        ReplyOutcome::Handled
    }

    /// `set_adapter_signal`: reads `tuner=<fe>,<level>,<lock>,<quality>`.
    pub fn set_adapter_signal(&mut self, text: &str) {
        let Some(tun) = text.find("tuner=") else {
            return;
        };
        let Some(comma) = text[tun..].find(',') else {
            return;
        };
        let values = scan_ints(&text[tun + comma + 1..], 3);
        let mut strength = values.first().copied().unwrap_or(0);
        let status = values.get(1).copied().unwrap_or(0);
        let mut quality = values.get(2).copied().unwrap_or(0);
        // Workaround for faulty servers (level=0, e.g. XORO)
        if strength == 0 && status > 0 && quality > 0 {
            strength = 1;
        }
        // Workaround for faulty servers (quality=0, e.g. AVM 6490)
        if quality == 0 && strength > 0 && status > 0 {
            quality = 7;
        }
        if self.ignore_signal {
            // satipc.cpp:606-607
            ms_log::log!(
                "satipc: Ignoring server signal status since both strength_multiplier and snr_multiplier are 0"
            );
            strength = MAX_STRENGTH;
            quality = MAX_QUALITY;
        }
        if self.strength != strength || self.snr != quality {
            // satipc.cpp:613-617
            ms_log::log!(
                "satipc: Received signal status from the server for adapter {}, stength={} status={} quality={}",
                self.id,
                strength,
                status,
                quality
            );
        }
        self.strength = strength;
        self.status = status != 0;
        self.snr = quality;
    }

    /// `satipc_rtcp_reply` signal part: parses the `SES1` APP payload.
    pub fn rtcp_reply(&mut self, b: &[u8]) {
        self.rtsp_socket_closed = false;
        if b.len() >= 24 && b[0] == 0x80 && b[1] == 0xC8 {
            let rp = u32::from_ne_bytes([b[20], b[21], b[22], b[23]]);
            if self.transport == Transport::Udp {
                self.repno = self.repno.wrapping_add(1);
                if self.repno.is_multiple_of(100) {
                    // satipc.cpp:642-646; ad->pid_err is not modelled.
                    let missing = rp.wrapping_sub(self.rcvp) as i32;
                    ms_log::log!(
                        "satipc: rtp report, adapter {}: rtcp missing packets {}, rtp missing {}, rtp ooo {}, pid unknown {}",
                        self.id,
                        missing,
                        self.rtp_miss,
                        self.rtp_ooo,
                        0
                    );
                }
            }
        }
        let (mut sm, mut rp) = (0usize, 0usize);
        while sm < b.len() {
            if !b[sm..].starts_with(b"SES1") {
                sm += 1;
                continue;
            }
            // satipc.cpp:655
            ms_log::debugm!("satipc: satipc_rtcp_reply SES1 match at {}", sm);
            sm += 4;
            if b.len() >= sm + 4 && b[sm] == 0 && b[sm + 1] == 0 {
                rp = (usize::from(b[sm + 2]) << 8) | usize::from(b[sm + 3]);
                sm += 4;
                break;
            }
            sm += 1;
        }
        if rp > 0 && b.len() > sm + rp {
            self.set_adapter_signal(&c_text(&b[sm..sm + rp]));
        } else {
            self.set_adapter_signal(&c_text(b));
        }
    }

    /// `satipc_close_rtsp_socket`: forgets the connection and session.
    pub fn close_rtsp_socket(&mut self) {
        self.restart_needed = false;
        self.state = State::DISCONNECTED;
        self.rtsp_socket_closed = true;
        self.want_tune = true;
        self.want_commit = true;
        self.last_close = self.tick();
        self.sent_transport = false;
        self.expect_reply = false;
        self.last_response_sent = 0;
        self.stream_id = -1;
        self.session.clear();
        self.rtsp_connected = false;
        self.data_open = false;
    }

    /// `satipc_close_rtsp`: the media socket was closed.
    pub fn on_data_closed(&mut self) {
        // satipc.cpp:438-440; socket id/handle have no Rust equivalent.
        ms_log::log!(
            "adapter {} satip rtsp sock {}, handle {}, keep_open {}, restart_needed {}",
            self.id,
            0,
            0,
            i32::from(self.keep_adapter_open),
            i32::from(self.restart_needed)
        );
        if self.keep_adapter_open || self.restart_needed {
            self.close_rtsp_socket();
        } else {
            self.ops.push_back(Op::CloseAdapter);
        }
    }

    /// `sockets_del(ad->sock)`, which runs the close callback at once.
    pub fn close_data(&mut self) {
        if self.data_open {
            self.data_open = false;
            self.ops.push_back(Op::CloseData);
            self.on_data_closed();
        }
    }

    /// `satipc_timeout`: keep-alive, silent-server restart and reconnect.
    pub fn timeout(&mut self) -> i32 {
        if !self.enabled {
            return 1;
        }
        if self.rtsp_socket_closed {
            // Claim the reopen; the driver flags it again when it fails.
            self.rtsp_socket_closed = false;
            self.last_connect = self.tick();
            self.ops.push_back(Op::OpenRtsp);
            return 0;
        }
        // The claim serializes restarts; the driver releases it after the open.
        #[cfg(feature = "srt")]
        if self.transport == Transport::Srt && !self.srt_restart_claimed && !self.srt_connected {
            self.srt_restart_claimed = true;
            self.ops.push_back(Op::RestartSrt);
            return 0;
        }
        if self.expect_reply && self.tick() - self.last_response_sent > self.timeout_ms {
            // satipc.cpp:497-499
            ms_log::log!(
                "satipc {}: no response was received from the server for more than {} ms, closing connection",
                self.id,
                self.tick() - self.last_response_sent
            );
            self.restart_needed = true;
            self.state = State::DISCONNECTED;
            self.close_data();
        }
        if self.want_tune || !self.apid.is_empty() || !self.dpid.is_empty() {
            // satipc.cpp:506-508
            ms_log::log!(
                "satipc {} no timeout will be performed as we have operations in queue tune {} lap {} ldp {}",
                self.id,
                i32::from(self.want_tune),
                self.apid.len(),
                self.dpid.len()
            );
            return 0;
        }
        // satipc.cpp:573-576; socket id/handle have no Rust equivalent.
        ms_log::log!(
            "satipc: Sent keep-alive to the satip server {}:{}, adapter {}, socket_id {}, handle {}, timeout {}",
            self.host,
            self.port,
            self.id,
            0,
            0,
            self.fe_timeout_ms
        );
        self.http_request(None, "OPTIONS");
        0
    }

    /// `satipc_open_device` field reset after the sockets are open.
    pub fn reset_for_open(&mut self) {
        self.session.clear();
        self.apid.clear();
        self.dpid.clear();
        self.cseq = 1;
        self.err = false;
        self.expect_reply = false;
        self.last_response_sent = 0;
        self.last_connect = 0;
        self.sent_transport = false;
        self.stream_id = -1;
        self.want_commit = false;
        self.rcvp = 0;
        self.repno = 0;
        self.rtp_miss = 0;
        self.rtp_ooo = 0;
        self.rtp_seq = 0xFFFF;
        self.ignore_packets = true;
        self.num_describe = 0;
        self.force_pids = false;
        self.last_setup = -10_000;
        self.last_cmd = LastCmd::None;
        self.enabled = true;
        self.rtsp_socket_closed = false;
        self.last_close = 0;
        self.timeout_ms = 30_000;
        self.state = State::Setup;
    }

    /// `satip_close_device`.
    pub fn close_device(&mut self) {
        if !self.enabled {
            return;
        }
        // satipc.cpp:954
        ms_log::log!("satip device {}:{} is closing", self.host, self.port);
        if self.state == State::Play {
            self.state = State::Teardown;
            self.request();
        }
        self.enabled = false;
        self.state = State::DISCONNECTED;
    }

    /// `satip_standby_device`.
    pub fn standby(&mut self) {
        if !self.enabled {
            return;
        }
        // satipc.cpp:979-980
        ms_log::log!(
            "satip device {}:{} going to standby (expect_reply {})",
            self.host,
            self.port,
            i32::from(self.expect_reply)
        );
        if self.state == State::Play {
            self.state = State::Teardown;
            self.request();
        }
        self.err = false;
        self.sent_transport = false;
        self.apid.clear();
        self.dpid.clear();
        if self.restart_when_tune {
            // satipc.cpp:989
            ms_log::log!("All stream closed, restarting satip adapter {}", self.id);
            self.restart_needed = true;
            self.close_data();
            self.state = State::DISCONNECTED;
        }
    }

    /// `satip_post_init`.
    pub fn post_init(&mut self) {
        if !self.enabled {
            return;
        }
        self.state = State::Setup;
        self.request();
    }

    /// `satipc_set_pid`: queues `pid` for `addpids`; returns the filter id.
    pub fn set_pid(&mut self, pid: u16) -> i32 {
        // satipc.cpp:1410-1411
        let err = if self.enabled {
            i32::from(self.err)
        } else {
            -2
        };
        ms_log::log!(
            "satipc: set_pid for adapter {}, pid {}, err {} (lap={})",
            self.id,
            pid,
            err,
            self.apid.len()
        );
        if !self.enabled || self.err {
            return 0;
        }
        let aid = self.id as i32;
        if let Some(i) = self.dpid.iter().position(|p| *p == pid) {
            self.dpid.swap_remove(i);
            // satipc.cpp:1420-1422
            ms_log::logm!(
                "satipc: set_pid for pid {} already in the delete list! (ldp={})",
                pid,
                self.dpid.len()
            );
            return aid + 100;
        }
        self.apid.push(pid);
        aid + 100
    }

    /// `satipc_del_filters`: queues `pid` for `delpids`.
    pub fn del_filters(&mut self, fd: i32, pid: u16) -> i32 {
        // satipc.cpp:1434-1436
        let aid = fd - 100;
        let err = if self.enabled {
            i32::from(self.err)
        } else {
            -2
        };
        ms_log::log!(
            "satipc: del_pid for aid {}, pid {}, err {} (ldp={})",
            aid,
            pid,
            err,
            self.dpid.len()
        );
        if !self.enabled || self.err {
            return 0;
        }
        if let Some(i) = self.apid.iter().position(|p| *p == pid) {
            self.apid.swap_remove(i);
            // satipc.cpp:1445-1447
            ms_log::logm!(
                "satipc: del_pid for pid {} already in the add list! (lap={})",
                pid,
                self.apid.len()
            );
            return 0;
        }
        self.dpid.push(pid);
        0
    }

    /// Marks the adapter PID slots for `wanted` (`mark_pid_add`/`mark_pid_deleted`).
    pub fn mark_pids(&mut self, wanted: &PidSet) {
        let wanted: BTreeSet<u16> = wanted.iter().collect();
        for slot in self.pids.iter_mut().flatten() {
            if wanted.contains(&slot.pid) {
                if slot.flags == PidState::Deleted {
                    slot.flags = PidState::New;
                }
            } else {
                slot.flags = PidState::Deleted;
            }
        }
        for pid in wanted {
            if self.pids.iter().flatten().any(|slot| slot.pid == pid) {
                continue;
            }
            let slot = Some(PidSlot {
                pid,
                flags: PidState::New,
                fd: false,
            });
            if let Some(free) = self.pids.iter_mut().find(|slot| slot.is_none()) {
                *free = slot;
            } else if self.pids.len() < MAX_PIDS {
                self.pids.push(slot);
            }
        }
    }

    /// `update_pids` for this adapter followed by `ad->commit`.
    pub fn update_pids(&mut self) {
        if self.err {
            return;
        }
        let aid = self.id as i32;
        for i in (0..self.pids.len()).rev() {
            if let Some(slot) = self.pids[i]
                && slot.flags == PidState::Deleted
            {
                if slot.fd {
                    self.del_filters(aid + 100, slot.pid);
                }
                self.pids[i] = None;
            }
        }
        for i in 0..self.pids.len() {
            if let Some(slot) = self.pids[i]
                && slot.flags == PidState::New
            {
                let fd = slot.fd || self.set_pid(slot.pid) > 0;
                self.pids[i] = Some(PidSlot {
                    flags: PidState::Active,
                    fd,
                    ..slot
                });
            }
        }
        self.commit();
    }

    /// `get_adapter_pids`: active PIDs in slot order, `all` or `none`.
    pub fn adapter_pids(&self) -> String {
        let mut out = String::new();
        for slot in self.pids.iter().flatten() {
            if matches!(slot.flags, PidState::Active | PidState::New) {
                if slot.pid == PID_ALL {
                    out.push_str("all,");
                    break;
                }
                let _ = write!(out, "{},", slot.pid);
            }
        }
        if out.is_empty() {
            "none".to_owned()
        } else {
            out.pop();
            out
        }
    }

    /// `satipc_get_pids`: `pids=` when forced, else `addpids=`/`delpids=` deltas.
    pub fn get_pids(&mut self, adapter_pids: &str, mut send_pids: bool) -> String {
        if self.force_pids {
            send_pids = true;
            self.force_pids = false;
        }
        if self.apid.is_empty() && self.dpid.is_empty() {
            send_pids = true;
        }
        let mut tmp = adapter_pids.to_owned();
        if tmp == "all" || tmp == "none" {
            send_pids = true;
        }
        if tmp == "all" && self.no_pids_all {
            tmp = "0,1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16,17,18,19,20".to_owned();
        }
        let mut url = String::new();
        if send_pids {
            url.push_str("pids=");
            url.push_str(&tmp);
            self.apid.clear();
            self.dpid.clear();
            return url;
        }
        if !self.apid.is_empty() {
            url.push_str("addpids=");
            url.push_str(&join_pids(&self.apid));
            self.apid.clear();
        }
        if !self.dpid.is_empty() {
            if !url.is_empty() {
                url.push('&');
            }
            url.push_str("delpids=");
            url.push_str(&join_pids(&self.dpid));
            self.dpid.clear();
        }
        url
    }

    /// `get_s2_url`.
    pub fn s2_url(&self) -> String {
        let tp = &self.tp;
        let mut url = String::new();
        if tp.src_specified {
            let _ = write!(url, "src={}", tp.src);
        }
        if self.satip_fe > 0 {
            let _ = write!(url, "&fe={}", self.satip_fe);
        }
        if tp.freq != 0 {
            let _ = write!(url, "&freq={}", tp.freq / 1000);
        }
        fill_str(&mut url, "msys", delsys_name_opt(tp.msys));
        fill_str(
            &mut url,
            "mtype",
            tp.mtype_specified.then(|| modulation_name(tp.mtype)),
        );
        fill_str(&mut url, "pol", pol_name(tp.pol));
        if tp.sr != 0 {
            let _ = write!(url, "&sr={}", tp.sr / 1000);
        }
        fill_str(&mut url, "fec", tp.fec_specified.then(|| fec_name(tp.fec)));
        fill_str(&mut url, "ro", tp.ro.map(rolloff_name));
        fill_str(
            &mut url,
            "plts",
            tp.plts.map(|on| if on { "on" } else { "off" }),
        );
        fill_int(&mut url, "isi", tp.plp);
        fill_str(&mut url, "plsm", tp.plsm.map(pls_mode_name));
        fill_int(&mut url, "plsc", tp.plsc);
        url
    }

    /// `get_c2_url`.
    pub fn c2_url(&self) -> String {
        let tp = &self.tp;
        let mut url = String::new();
        if tp.freq != 0 {
            let _ = write!(url, "freq={:.1}", f64::from(tp.freq) / 1000.0);
        }
        if self.satip_fe > 0 {
            let _ = write!(url, "&fe={}", self.satip_fe);
        }
        if tp.sr != 0 {
            let _ = write!(url, "&sr={}", tp.sr / 1000);
        }
        fill_str(&mut url, "msys", delsys_name_opt(tp.msys));
        fill_str(
            &mut url,
            "mtype",
            tp.mtype_specified.then(|| modulation_name(tp.mtype)),
        );
        fill_str(&mut url, "gi", tp.gi.map(gi_name));
        fill_str(&mut url, "fec", tp.fec_specified.then(|| fec_name(tp.fec)));
        fill_str(&mut url, "tmode", tp.tmode.map(tmode_name));
        fill_int(
            &mut url,
            "specinv",
            tp.specinv.filter(|v| *v != INVERSION_AUTO),
        );
        fill_int(&mut url, "t2id", tp.t2id);
        fill_int(&mut url, "sm", tp.sm);
        fill_int(&mut url, "plp", tp.plp);
        url
    }

    /// `get_t2_url`.
    pub fn t2_url(&self) -> String {
        let tp = &self.tp;
        let mut url = String::new();
        if tp.freq != 0 {
            let _ = write!(url, "freq={:.1}", f64::from(tp.freq) / 1000.0);
        }
        if self.satip_fe > 0 {
            let _ = write!(url, "&fe={}", self.satip_fe);
        }
        if let Some(bw) = tp.bw {
            let _ = write!(url, "&bw={}", bw / 1_000_000);
        }
        fill_str(&mut url, "msys", delsys_name_opt(tp.msys));
        fill_str(
            &mut url,
            "mtype",
            tp.mtype_specified.then(|| modulation_name(tp.mtype)),
        );
        fill_str(&mut url, "gi", tp.gi.map(gi_name));
        fill_str(&mut url, "tmode", tp.tmode.map(tmode_name));
        fill_int(
            &mut url,
            "specinv",
            tp.specinv.filter(|v| *v != INVERSION_AUTO),
        );
        fill_int(&mut url, "c2tft", tp.c2tft);
        fill_int(&mut url, "ds", tp.ds);
        fill_int(&mut url, "plp", tp.plp);
        url
    }

    /// `tune_url`: picks the builder from `ad->sys[0]`.
    pub fn tune_url(&self) -> String {
        match self.sys0 {
            DeliverySystem::DvbS2 | DeliverySystem::DvbS => self.s2_url(),
            DeliverySystem::DvbC2 | DeliverySystem::DvbC => self.c2_url(),
            DeliverySystem::DvbT2 | DeliverySystem::DvbT | DeliverySystem::Other(8) => {
                self.t2_url()
            }
            _ => {
                // satipc.cpp:1706
                ms_log::log!("No system specified {}", delsys_code(self.sys0));
                String::new()
            }
        }
    }

    /// `http_request`: formats a request and queues it on the RTSP socket.
    pub fn http_request(&mut self, url: Option<&str>, method: &str) {
        if !self.enabled {
            return;
        }
        if self.expect_reply {
            // satipc.cpp:1614; glibc prints "(null)" for the NULL url.
            ms_log::log!(
                "http_request: not sending method {}: url {}",
                method,
                url.unwrap_or("(null)")
            );
            return;
        }
        let mut headers = String::new();
        if !self.sent_transport && (method.starts_with('S') || method.starts_with('P')) {
            self.last_setup = self.tick();
            self.sent_transport = true;
            self.stream_id = -1;
            self.err = false;
            self.session.clear();
            match self.transport {
                #[cfg(feature = "srt")]
                Transport::Srt => {
                    let _ = write!(
                        headers,
                        "\r\nTransport: RTP/AVP/SRT;srt_streamid={}",
                        self.srt_streamid
                    );
                }
                Transport::Tcp => headers.push_str("\r\nTransport: RTP/AVP/TCP;interleaved=0-1"),
                Transport::Udp => {
                    let _ = write!(
                        headers,
                        "\r\nTransport: RTP/AVP;unicast;client_port={}-{}",
                        self.listen_udp,
                        u32::from(self.listen_udp) + 1
                    );
                }
            }
        }
        if !self.session.is_empty() {
            let _ = write!(headers, "\r\nSession: {}", self.session);
        }
        match method {
            "OPTIONS" => {
                let _ = write!(headers, "\r\nUser-Agent: {}", self.user_agent);
                self.last_cmd = LastCmd::Options;
            }
            "DESCRIBE" => {
                headers.push_str("\r\nAccept: application/sdp");
                self.last_cmd = LastCmd::Describe;
            }
            "PLAY" => self.last_cmd = LastCmd::Play,
            "TEARDOWN" => self.last_cmd = LastCmd::Teardown,
            "SETUP" => self.last_cmd = LastCmd::Setup,
            _ => {}
        }
        let url = url.unwrap_or("");
        let qm = if url.is_empty() { "" } else { "?" };
        let sid = if self.stream_id != -1 {
            format!("stream={}", self.stream_id)
        } else {
            String::new()
        };
        let request = format!(
            "{method} rtsp://{}:{}/{sid}{qm}{url} RTSP/1.0\r\nCSeq: {}{headers}\r\n\r\n",
            self.host, self.port, self.cseq
        );
        self.cseq = self.cseq.wrapping_add(1);
        // satipc.cpp:1677-1679; the remote socket handle is not tracked here.
        let remote_socket = if self.rtsp_connected { 0 } else { -1 };
        ms_log::log!(
            "satipc_http_request (adapter {}): sock {}: [[ {} {} ]]",
            self.id,
            remote_socket,
            method,
            url
        );
        ms_log::logm!("{}", request);
        if self.rtsp_connected {
            self.ops.push_back(Op::Send(request.into_bytes()));
            self.expect_reply = true;
            self.last_response_sent = self.tick();
        } else {
            // satipc.cpp:1686
            ms_log::log!("http_request: remote socket is -1");
        }
    }

    /// `satipc_send_setup`.
    fn send_setup(&mut self) {
        let mut url = self.tune_url();
        self.ignore_packets = true;
        self.want_tune = false;
        self.want_commit = true;
        self.sent_transport = false;
        url.push_str("&pids=none");
        self.http_request(Some(&url), "SETUP");
    }

    /// `satipc_send_play`.
    // C++ LOG_AND_RETURN(0, ...) in a void function returns `()`.
    #[allow(clippy::unused_unit)]
    fn send_play(&mut self) {
        if !self.want_tune && self.apid.is_empty() && self.dpid.is_empty() && !self.force_pids {
            // satipc.cpp:1793
            ms_log::log_and_return!((), "adapter {}: Nothing to commit", self.id);
        }
        let mut url = String::new();
        let mut use_pids = false;
        if self.want_tune || !self.sent_transport {
            url = self.tune_url();
            self.ignore_packets = true;
            url.push('&');
            use_pids = true;
        }
        let adapter_pids = self.adapter_pids();
        url.push_str(&self.get_pids(&adapter_pids, use_pids));
        self.want_tune = false;
        self.http_request(Some(&url), "PLAY");
    }

    /// `satipc_send_teardown`.
    fn send_teardown(&mut self) {
        if self.stream_id != -1 {
            self.http_request(None, "TEARDOWN");
        } else {
            // satipc.cpp:1814-1815
            ms_log::log!(
                "Unable to send TEARDOWN for adapter {} (stream_id {})",
                self.id,
                self.stream_id
            );
        }
        self.session.clear();
        self.stream_id = -1;
        self.sent_transport = false;
    }

    /// `satipc_send_describe`: polls the signal until the server reports it.
    fn send_describe(&mut self) {
        if self.expect_reply || self.strength != 0 || self.state != State::Play {
            return;
        }
        self.num_describe += 1;
        if (0..4).contains(&self.num_describe) {
            self.http_request(None, "DESCRIBE");
        } else {
            self.num_describe = 0;
        }
    }

    /// `satipc_request`: sends what the state and flags call for.
    pub fn request(&mut self) {
        if !self.enabled {
            return;
        }
        // satipc.cpp:1845-1851
        ms_log::log!(
            "satipc: request for adapter {}, state {}, freq {}, pids to add {}, pids to delete {}, expect_reply {}, want_tune {}, last_cmd {}, sent_transport {}, closed_rtsp {}",
            self.id,
            self.state as u8,
            self.tp.freq,
            self.apid.len(),
            self.dpid.len(),
            i32::from(self.expect_reply),
            i32::from(self.want_tune),
            self.last_cmd as u8,
            i32::from(self.sent_transport),
            i32::from(self.rtsp_socket_closed)
        );
        if self.rtsp_socket_closed {
            return;
        }
        if self.tp.freq == 0 && matches!(self.state, State::Inactive | State::Setup | State::Play) {
            return;
        }
        if self.sid_cnt == 0 && matches!(self.state, State::Setup | State::Play) {
            return;
        }
        if self.expect_reply {
            return;
        }
        // restart after a TEARDOWN unless the adapter gave up (#904)
        if self.state == State::Inactive && !self.err && self.last_cmd == LastCmd::Teardown {
            self.state = State::Setup;
        }
        if self.state == State::Setup {
            self.sent_transport = false;
            self.stream_id = -1;
            self.session.clear();
            self.want_tune = true;
            self.want_commit = true;
        }
        if self.state == State::Setup && self.option_no_setup {
            self.state = State::Play;
        }
        // No SRT socket, no SETUP: never advertise a dead streamid. The driver
        // opens it and asks again; on failure the timeout path retries.
        #[cfg(feature = "srt")]
        if self.transport == Transport::Srt && !self.sent_transport && !self.srt_connected {
            self.ops.push_back(Op::OpenSrt);
            return;
        }
        // satipc.cpp:1900
        ms_log::logm!(
            "satipc: for adapter {}, executing state {}",
            self.id,
            self.state as u8
        );
        match self.state {
            State::Setup => self.send_setup(),
            State::Play => self.send_play(),
            State::Teardown => self.send_teardown(),
            State::Inactive => {}
        }
        self.send_describe();
    }

    /// `satipc_commit`.
    #[allow(clippy::unused_unit)] // C++ LOG_AND_RETURN(0, ...) returns `()`.
    pub fn commit(&mut self) {
        if !self.enabled {
            // satipc.cpp:1927
            ms_log::log_and_return!((), "No satip client found for adapter {}", self.id);
        }
        // satipc.cpp:1929-1932
        ms_log::log!(
            "satipc: commit on freq {}, sys {} for adapter {} (expect reply {})",
            self.tp.freq / 1000,
            delsys_code(self.tp.msys),
            self.id,
            i32::from(self.expect_reply)
        );
        self.want_commit = true;
        self.request();
    }

    /// `satipc_tune`; `ad->tp` must already hold the new parameters.
    pub fn tune(&mut self) -> i32 {
        if !self.enabled {
            return 1;
        }
        // satipc.cpp:1942-1945
        ms_log::log!(
            "satipc: tuning to freq {}, sys {} for adapter {} (state {}, expect_reply {})",
            self.tp.freq / 1000,
            delsys_code(self.tp.msys),
            self.id,
            self.state as u8,
            i32::from(self.expect_reply)
        );
        self.err = false;
        self.want_commit = false;
        self.want_tune = true;
        self.ignore_packets = true;
        self.apid.clear();
        self.dpid.clear();
        if self.restart_when_tune {
            if !self.rtsp_socket_closed {
                // satipc.cpp:1955
                ms_log::log!("Restart needed for satip adapter {}", self.id);
                self.state = State::Teardown;
                self.request();
                self.restart_needed = true;
                self.close_data();
                self.state = State::DISCONNECTED;
            }
            self.last_connect = self.tick();
            self.ops.push_back(Op::OpenRtsp);
        }
        if self.state == State::Inactive {
            self.state = State::Setup;
        }
        0
    }
}

const INVERSION_AUTO: i32 = 2;

fn join_pids(pids: &[u16]) -> String {
    pids.iter()
        .map(u16::to_string)
        .collect::<Vec<_>>()
        .join(",")
}

fn fill_str(url: &mut String, key: &str, value: Option<&str>) {
    if let Some(value) = value.filter(|v| !v.is_empty()) {
        let _ = write!(url, "&{key}={value}");
    }
}

fn fill_int(url: &mut String, key: &str, value: Option<i32>) {
    if let Some(value) = value {
        let _ = write!(url, "&{key}={value}");
    }
}

fn body_param<'a>(text: &'a str, name: &str) -> Option<&'a str> {
    let body = text.find("\r\n\r\n")?;
    let rest = &text[body + 4..];
    rest.find(name).map(|pos| &rest[pos..])
}

/// `sscanf("%d,%d,%d")`: stops at the first value that does not parse.
fn scan_ints(text: &str, count: usize) -> Vec<i32> {
    let mut values = Vec::new();
    let mut rest = text;
    while values.len() < count {
        let trimmed = rest.trim_start();
        let digits = trimmed
            .char_indices()
            .take_while(|(i, c)| c.is_ascii_digit() || (*i == 0 && (*c == '-' || *c == '+')))
            .count();
        let Ok(value) = trimmed[..digits].parse::<i32>() else {
            break;
        };
        values.push(value);
        match trimmed[digits..].strip_prefix(',') {
            Some(next) => rest = next,
            None => break,
        }
    }
    values
}

fn delsys_name_opt(sys: DeliverySystem) -> Option<&'static str> {
    (sys != DeliverySystem::Undefined).then(|| delsys_name(sys))
}

fn pol_name(pol: Polarization) -> Option<&'static str> {
    match pol {
        Polarization::Vertical => Some("v"),
        Polarization::Horizontal => Some("h"),
        Polarization::Right => Some("r"),
        Polarization::Left => Some("l"),
        Polarization::None => None,
    }
}

fn modulation_name(mtype: u32) -> &'static str {
    const NAMES: [&str; 17] = [
        "qpsk", "16qam", "32qam", "64qam", "128qam", "256qam", "", "8vsb", "16vsb", "8psk",
        "16apsk", "32apsk", "dqpsk", "qam_4_nr", "64apsk", "128apsk", "256apsk",
    ];
    NAMES.get(mtype as usize).copied().unwrap_or("")
}

fn fec_name(fec: u8) -> &'static str {
    const NAMES: [&str; 15] = [
        "none", "12", "23", "34", "45", "56", "67", "78", "89", "", "35", "910", "25", "14", "13",
    ];
    NAMES.get(usize::from(fec)).copied().unwrap_or("")
}

fn rolloff_name(ro: u8) -> &'static str {
    const NAMES: [&str; 7] = ["0.35", "0.20", "0.25", "", "0.15", "0.10", "0.05"];
    NAMES.get(usize::from(ro)).copied().unwrap_or("")
}

fn gi_name(gi: i32) -> &'static str {
    const NAMES: [&str; 11] = [
        "132", "116", "18", "14", "", "1128", "19128", "19256", "pn420", "pn595", "pn945",
    ];
    usize::try_from(gi)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("")
}

fn tmode_name(tmode: i32) -> &'static str {
    const NAMES: [&str; 9] = ["2k", "8k", "", "4k", "1k", "16k", "32k", "c1", "c3780"];
    usize::try_from(tmode)
        .ok()
        .and_then(|i| NAMES.get(i))
        .copied()
        .unwrap_or("")
}

fn pls_mode_name(mode: i32) -> &'static str {
    match mode {
        0 => "root",
        1 => "gold",
        2 => "combo",
        _ => "",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn c2_frequency_rounds_like_printf() {
        let mut client = Client::new(0, "h", 554);
        client.sys0 = DeliverySystem::DvbC;
        client.tp.freq = 474_250;
        assert_eq!(client.tune_url(), "freq=474.2");
        client.tp.freq = 474_750;
        assert_eq!(client.tune_url(), "freq=474.8");
        client.tp.freq = 346_000;
        assert_eq!(client.tune_url(), "freq=346.0");
    }

    #[test]
    fn scan_ints_stops_like_sscanf() {
        assert_eq!(scan_ints("215,1,12,0", 3), vec![215, 1, 12]);
        assert_eq!(scan_ints("215,x", 3), vec![215]);
        assert_eq!(scan_ints("", 3), Vec::<i32>::new());
    }
}
