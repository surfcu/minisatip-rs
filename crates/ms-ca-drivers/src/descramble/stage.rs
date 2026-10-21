//! The descrambler as a data-plane stage, driven by messages from the control plane.

use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Instant;

use ms_ca::CaEvent;
use ms_config::DropEncrypted;
use ms_ts::TsStage;

use super::cipher::CipherRegistry;
use super::pipeline::{Descrambler, ServiceStats};

/// Messages to an adapter's descrambler.
pub enum DescramblerCommand {
    /// Track a service and map its elementary PIDs to it.
    AddService {
        /// PMT slot id.
        pmt_id: usize,
        /// PIDs whose scrambled packets belong to the service.
        pids: Vec<u16>,
    },
    /// Remap a service's PIDs and keep its CWs; adds it when unknown.
    UpdatePids {
        /// PMT slot id.
        pmt_id: usize,
        /// PIDs whose scrambled packets belong to the service.
        pids: Vec<u16>,
    },
    /// Re-validate the next CW (`disable_cw` after a CAPMT send).
    DisableCw {
        /// PMT slot id.
        pmt_id: usize,
    },
    /// Stop tracking a service and drop its CWs.
    RemoveService {
        /// PMT slot id.
        pmt_id: usize,
    },
    /// A CA driver event; only `ControlWord` is used.
    Ca(CaEvent),
    /// Whether a CA serves this adapter (`ad->ca_mask != 0`).
    SetCaActive(bool),
    /// Whether `pids=all` is streamed with `-o ~` (no dropping).
    SetPidsAll(bool),
    /// Replies with the decrypt statistics.
    Query(Sender<DescramblerStats>),
}

/// Decrypt statistics of one adapter.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DescramblerStats {
    /// Per-service stamps, by PMT id.
    pub services: Vec<ServiceStats>,
    /// Packets left scrambled (`ad->dec_err`).
    pub dec_err: u64,
}

/// Settings of one adapter's descrambler.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StageConfig {
    /// `-E` undecrypted packet handling.
    pub drop_encrypted: DropEncrypted,
    /// `-o ~` (`opts.pids_all_no_dec`).
    pub pids_all_no_dec: bool,
}

impl Default for StageConfig {
    fn default() -> Self {
        Self {
            drop_encrypted: DropEncrypted::Drop,
            pids_all_no_dec: false,
        }
    }
}

/// [`Descrambler`] behind a command channel; runs on the adapter thread.
pub struct DescramblerStage {
    commands: Receiver<DescramblerCommand>,
    descrambler: Descrambler,
    config: StageConfig,
    ca_active: bool,
    pids_all: bool,
    clock: Box<dyn FnMut() -> i64 + Send>,
}

impl DescramblerStage {
    /// A stage whose clock counts ms from `origin`, the control plane's tick base.
    pub fn new(
        registry: CipherRegistry,
        config: StageConfig,
        origin: Instant,
    ) -> (Sender<DescramblerCommand>, Self) {
        let clock = move || origin.elapsed().as_millis() as i64;
        Self::with_clock(registry, config, Box::new(clock))
    }

    /// A stage with an injected clock (ms).
    pub fn with_clock(
        registry: CipherRegistry,
        config: StageConfig,
        clock: Box<dyn FnMut() -> i64 + Send>,
    ) -> (Sender<DescramblerCommand>, Self) {
        let (tx, commands) = mpsc::channel();
        let stage = Self {
            commands,
            descrambler: Descrambler::new(registry),
            config,
            ca_active: false,
            pids_all: false,
            clock,
        };
        (tx, stage)
    }

    /// The wrapped descrambler.
    pub fn descrambler(&self) -> &Descrambler {
        &self.descrambler
    }

    fn handle(&mut self, command: DescramblerCommand, now: i64) {
        match command {
            DescramblerCommand::AddService { pmt_id, pids } => {
                self.descrambler.add_service(pmt_id, &pids)
            }
            DescramblerCommand::UpdatePids { pmt_id, pids } => {
                self.descrambler.set_service_pids(pmt_id, &pids)
            }
            DescramblerCommand::DisableCw { pmt_id } => self.descrambler.disable_cw(pmt_id),
            DescramblerCommand::RemoveService { pmt_id } => self.descrambler.remove_service(pmt_id),
            DescramblerCommand::Ca(CaEvent::ControlWord {
                pmt_id,
                algo,
                parity,
                cw,
                icam_ecm,
            }) => {
                // DVBAPI sends no IV and no expiry (`send_cw(..., NULL, 0, ...)`).
                let _ = self
                    .descrambler
                    .send_cw(pmt_id, algo, parity, &cw, None, 0, icam_ecm, now);
            }
            DescramblerCommand::Ca(_) => {}
            DescramblerCommand::SetCaActive(active) => self.ca_active = active,
            DescramblerCommand::SetPidsAll(on) => self.pids_all = on,
            DescramblerCommand::Query(reply) => {
                let _ = reply.send(DescramblerStats {
                    services: self.descrambler.all_stats(),
                    dec_err: self.descrambler.dec_err(),
                });
            }
        }
    }

    /// Applies pending commands without processing a buffer.
    pub fn drain_commands(&mut self) {
        let now = (self.clock)();
        while let Ok(command) = self.commands.try_recv() {
            self.handle(command, now);
        }
    }
}

impl TsStage for DescramblerStage {
    fn process(&mut self, buf: &mut [u8]) {
        let now = (self.clock)();
        while let Ok(command) = self.commands.try_recv() {
            self.handle(command, now);
        }
        // Clear-text adapters pass through untouched, like `ad->ca_mask == 0`.
        if !self.ca_active {
            return;
        }
        self.descrambler.decrypt_stream(buf, now);
        let pids_all = self.config.drop_encrypted != DropEncrypted::Send
            && self.config.pids_all_no_dec
            && self.pids_all;
        self.descrambler
            .mark_pids_null(buf, self.config.drop_encrypted, pids_all);
    }
}
