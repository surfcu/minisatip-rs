//! `ca_devices[]` and the per-adapter entry points: init, close, reconnect poller.

use super::clock::Clock;
use super::consts::*;
use super::device::{CaDevice, CaState, HostEvent, LinkMode};
use super::link::{AdapterInfo, AdapterKind, CaOpener};
use std::sync::Arc;

/// `TABLES_RESULT_*` returned by `dvbca_init_dev`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InitResult {
    Ok,
    Retry,
    NoRetry,
}

/// Every CI slot, indexed by adapter id.
pub struct CaRegistry {
    devices: Vec<Option<CaDevice>>,
    clock: Arc<dyn Clock>,
    /// `opts.enigma`: `/dev/ciN` with bare SPDUs.
    pub enigma: bool,
    /// Set once any CA device initialised (`has_ci`).
    pub has_ci: bool,
}

fn index(i: i32) -> Option<usize> {
    usize::try_from(i).ok().filter(|&i| i < MAX_ADAPTERS)
}

impl CaRegistry {
    pub fn new(clock: Arc<dyn Clock>) -> Self {
        Self {
            devices: (0..MAX_ADAPTERS).map(|_| None).collect(),
            clock,
            enigma: false,
            has_ci: false,
        }
    }

    pub fn get(&self, i: i32) -> Option<&CaDevice> {
        self.devices.get(index(i)?)?.as_ref()
    }

    pub fn get_mut(&mut self, i: i32) -> Option<&mut CaDevice> {
        self.devices.get_mut(index(i)?)?.as_mut()
    }

    /// The device of slot `i`, allocated on first use (`alloc_ca_device`).
    pub fn device(&mut self, i: usize) -> &mut CaDevice {
        let clock = Arc::clone(&self.clock);
        self.devices[i].get_or_insert_with(|| CaDevice::new(i, clock))
    }

    /// `set_ca_channels`: the `-c` CAIDs, appended to each slot and kept over the CAM's.
    pub fn set_ca_channels(&mut self, channels: &[ms_config::CaChannels]) {
        for c in channels {
            let Some(i) = index(i32::from(c.adapter)) else {
                continue;
            };
            let dev = self.device(i);
            for &caid in &c.caids {
                if dev.caids.len() < MAX_CAID {
                    // `ca_device_t::caid[]` is uint16_t.
                    dev.caids.push(caid as u16);
                    dev.has_forced_caids = true;
                }
            }
        }
    }

    pub fn insert(&mut self, dev: CaDevice) {
        let i = dev.id;
        self.devices[i] = Some(dev);
    }

    pub fn remove(&mut self, i: usize) -> Option<CaDevice> {
        self.devices[i].take()
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = &mut CaDevice> {
        self.devices.iter_mut().flatten()
    }

    /// `is_ca_initializing`: enabled but not INITIALIZED, so PMTs keep retrying.
    pub fn is_ca_initializing(&self, i: i32) -> bool {
        self.get(i)
            .is_some_and(|d| d.enabled && d.state != CaState::Initialized)
    }

    /// `get_ca_caids_string` ("0B00, 0B01"), bounded like `strlcatf` into `max_len` bytes.
    pub fn caids_string(&mut self, i: usize, max_len: usize) -> String {
        let mut s = String::new();
        for c in self.device(i).caids() {
            s.push_str(&format!("{c:04X}, "));
        }
        s.truncate(max_len.saturating_sub(1));
        if s.len() > 1 {
            s.truncate(s.len() - 2);
        }
        s
    }

    /// `dvbca_init_dev`: opens the CA device of an adapter and starts the link.
    pub fn init_dev(&mut self, ad: &AdapterInfo, opener: &mut dyn CaOpener) -> InitResult {
        let Some(i) = index(ad.id) else {
            return InitResult::NoRetry;
        };
        if self.devices[i]
            .as_ref()
            .is_some_and(|c| c.state == CaState::Initialized)
        {
            return InitResult::Ok;
        }
        if !matches!(ad.kind, AdapterKind::Dvb | AdapterKind::Ci) {
            return InitResult::NoRetry;
        }
        let mut link = match opener.open(&ad.ca_path(self.enigma)) {
            Ok(l) => l,
            Err(_) if self.enigma && self.has_ci => {
                ms_log::log!(
                    "No CA device detected on adapter {}: enabling already registered CAs",
                    ad.id
                );
                return InitResult::Ok;
            }
            Err(_) => {
                ms_log::log!(
                    "No CA device detected on adapter {}: file {}",
                    ad.id,
                    ad.ca_path(self.enigma)
                );
                return InitResult::NoRetry;
            }
        };
        link.flush_input();
        let enigma = self.enigma;
        let c = self.device(i);
        c.set_link(Some(link));
        c.id = i;
        c.mode = if enigma {
            LinkMode::Enigma
        } else {
            LinkMode::Tpdu
        };
        c.state = CaState::Inactive;
        c.sock = None;
        c.pending_tag = 0;
        c.pending_session = 0;
        c.pending_since = -1;
        c.key = [[0; 16]; 2];
        c.iv = [[0; 16]; 2];
        c.push_event(HostEvent::Opened);
        if !c.force_ci && !opener.has_ciplus_root_cert() {
            c.force_ci = true;
        }
        if !enigma && !c.slot_present() {
            ms_log::log!("CA {}: no module in slot, deferring init", c.id);
            c.init_failed();
            return InitResult::Retry;
        }
        let ok = if enigma {
            c.init_enigma()
        } else {
            c.init_en50221()
        };
        if !ok {
            c.init_failed();
            return InitResult::Retry;
        }
        c.enabled = true;
        self.has_ci = true;
        InitResult::Ok
    }

    /// `dvbca_close_dev`: closes only a device stuck in INACTIVE.
    pub fn close_dev(&mut self, ad: &AdapterInfo) {
        if let Some(c) = self.get_mut(ad.id)
            && c.enabled
            && c.state == CaState::Inactive
        {
            c.request_close();
        }
    }

    /// `ca_reconnect`, every second: retries enabled devices that lost their handle.
    pub fn reconnect(
        &mut self,
        adapter: impl Fn(usize) -> Option<AdapterInfo>,
        opener: &mut dyn CaOpener,
    ) {
        let now = self.clock.tick_ms();
        for i in 0..MAX_ADAPTERS {
            if !self.devices[i]
                .as_ref()
                .is_some_and(|c| c.reconnect_due(now))
            {
                continue;
            }
            let Some(ad) = adapter(i).filter(|a| a.enabled) else {
                continue;
            };
            ms_log::log!("Trying to reconnect to CA {}", i);
            let r = self.init_dev(&ad, opener);
            let next = match r {
                InitResult::Ok => 0,
                _ => self.clock.tick_ms() + CA_RECONNECT_INTERVAL_MS,
            };
            if let Some(c) = self.devices[i].as_mut() {
                c.reconnect_next_try = next;
            }
        }
    }

    /// `ca_close` for socket `sock` of slot `sid`; guards bad slots like C++.
    pub fn close(&mut self, sid: i32, sock: u64) -> bool {
        self.get_mut(sid).is_some_and(|c| c.close(sock))
    }

    /// `ca_timeout` for slot `sid`.
    pub fn timeout(&mut self, sid: i32) {
        if let Some(c) = self.get_mut(sid) {
            c.timeout();
        }
    }

    /// `ca_read` for slot `sid`.
    pub fn read(&mut self, sid: i32, spdu: &[u8]) {
        if let Some(c) = self.get_mut(sid) {
            c.handle_spdu(spdu);
        }
    }
}

/// `get_authdata_filename`: CI+ auth cache file, blanks replaced by underscores.
pub fn authdata_filename(cache_dir: &str, slot: u32, ci_name: &str) -> String {
    let name: String = ci_name
        .chars()
        .take(CI_NAME_MAX)
        .map(|c| if c == ' ' { '_' } else { c })
        .collect();
    format!("{cache_dir}/ci_auth_{name}_{slot}.bin")
}
