//! Timestamped log of everything the CAM exchanged, in a stable text format.

use crate::capmt::CaPmt;
use crate::tags;
use std::fmt::{self, Write as _};
use std::sync::{Arc, Mutex, MutexGuard};

/// Direction of a recorded exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    HostToCam,
    CamToHost,
}

impl fmt::Display for Direction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::HostToCam => "H>C",
            Self::CamToHost => "C>H",
        })
    }
}

/// A recorded session-layer event or APDU. Polls are left out on purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Transport connection events (`create_tc`, `ctc_reply`, `delete_tc`, ...).
    Transport(&'static str),
    OpenSessionRequest {
        resource: u32,
    },
    OpenSessionResponse {
        status: u8,
        resource: u32,
        session: u16,
    },
    CloseSessionRequest {
        session: u16,
    },
    CloseSessionResponse {
        status: u8,
        session: u16,
    },
    Apdu {
        session: u16,
        resource: u32,
        tag: u32,
        body: Vec<u8>,
    },
}

/// One log line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Record {
    pub time_ms: u64,
    pub cam: u32,
    pub dir: Direction,
    pub event: Event,
}

fn hex(out: &mut String, data: &[u8]) {
    if data.is_empty() {
        out.push('-');
    }
    for b in data {
        let _ = write!(out, "{b:02X}");
    }
}

impl Record {
    /// The line without its timestamp.
    pub fn untimed(&self) -> String {
        let mut s = format!("cam{} {} ", self.cam, self.dir);
        match &self.event {
            Event::Transport(what) => s.push_str(what),
            Event::OpenSessionRequest { resource } => {
                let _ = write!(s, "open_session {resource:08X}");
            }
            Event::OpenSessionResponse {
                status,
                resource,
                session,
            } => {
                let _ = write!(
                    s,
                    "open_session_rsp s{session} {resource:08X} st={status:02X}"
                );
            }
            Event::CloseSessionRequest { session } => {
                let _ = write!(s, "close_session s{session}");
            }
            Event::CloseSessionResponse { status, session } => {
                let _ = write!(s, "close_session_rsp s{session} st={status:02X}");
            }
            Event::Apdu {
                session,
                resource,
                tag,
                body,
            } => {
                let name = tags::apdu_name(*tag).unwrap_or("unknown");
                let _ = write!(s, "apdu s{session} {resource:08X} {tag:06X} {name} ");
                hex(&mut s, body);
                if *tag == tags::TAG_CA_PMT {
                    match CaPmt::parse(body) {
                        Ok(p) => {
                            let _ = write!(s, " # {}", p.summary());
                        }
                        Err(_) => s.push_str(" # malformed"),
                    }
                }
            }
        }
        s
    }
}

impl fmt::Display for Record {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:>8} {}", self.time_ms, self.untimed())
    }
}

/// Shared, cloneable log; several CAM instances may write to one recorder.
#[derive(Debug, Clone, Default)]
pub struct Recorder(Arc<Mutex<Vec<Record>>>);

impl Recorder {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Vec<Record>> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn push(&self, record: Record) {
        self.lock().push(record);
    }

    /// A copy of every record so far, in insertion order.
    pub fn records(&self) -> Vec<Record> {
        self.lock().clone()
    }

    /// Only the APDUs, for diffs that ignore session plumbing.
    pub fn apdus(&self) -> Vec<Record> {
        self.lock()
            .iter()
            .filter(|r| matches!(r.event, Event::Apdu { .. }))
            .cloned()
            .collect()
    }

    pub fn clear(&self) {
        self.lock().clear();
    }

    /// One line per record: `<ms> cam<id> <H>C|C>H> <event>`.
    pub fn to_text(&self) -> String {
        self.lock().iter().map(|r| format!("{r}\n")).collect()
    }

    /// Like [`Recorder::to_text`] without timestamps, for timing-free diffs.
    pub fn to_untimed_text(&self) -> String {
        self.lock()
            .iter()
            .map(|r| format!("{}\n", r.untimed()))
            .collect()
    }
}
