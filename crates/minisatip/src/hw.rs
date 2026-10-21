//! `--hw-descrambler` (`hw_descrambler.cpp`): CWs go to the adapter's CA device
//! and the demux clears the packets; other algorithms fall back to software.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use ms_ca::{CaService, CwAlgo, TablesResult};
use ms_ca_drivers::algo::Algo;
use ms_ca_drivers::descramble::CipherRegistry;
use ms_ca_drivers::hw_descrambler::{
    CaDevice, HwAdapter, HwCipher, HwDescrambler, HwPmt, HwRegistry,
};
use ms_sched::pmt::CaSink;

/// The process' hardware descrambler, shared by the adapter ciphers and the CA hooks.
pub struct Hw<D: CaDevice + Send> {
    hw: Arc<Mutex<HwDescrambler<D>>>,
    adapters: Arc<Mutex<BTreeMap<usize, HwAdapter>>>,
}

impl<D: CaDevice + Send> Clone for Hw<D> {
    fn clone(&self) -> Self {
        Self {
            hw: Arc::clone(&self.hw),
            adapters: Arc::clone(&self.adapters),
        }
    }
}

impl<D: CaDevice + Send + 'static> Hw<D> {
    pub fn new(dev: D) -> Self {
        // hw_descrambler.cpp:517-518 (`init_hw_descrambler` when enabled).
        ms_log::log!(
            "hw_descrambler: Initializing Hardware Descrambler (--hw-descrambler enabled)"
        );
        Self {
            hw: Arc::new(Mutex::new(HwDescrambler::new(true, dev))),
            adapters: Arc::default(),
        }
    }

    fn adapters(&self) -> BTreeMap<usize, HwAdapter> {
        self.adapters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// `register_algo(&hw_*_op)` ahead of the software ops (`init_algo` order).
    pub fn registry(&self, aid: usize, pa: i32, fe: i32) -> CipherRegistry {
        let adapter = HwAdapter {
            id: aid,
            pa,
            fe,
            enabled: true,
        };
        self.adapters
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(aid, adapter);
        let mut registry = CipherRegistry::empty();
        for algo in [CwAlgo::Csa, CwAlgo::Aes128Ecb, CwAlgo::Aes128Cbc] {
            let hw = Arc::clone(&self.hw);
            registry.register(
                algo,
                Arc::new(move || {
                    Box::new(HwCipher::new(Arc::clone(&hw), adapter, Algo::from(algo)))
                }),
            );
        }
        registry.extend_missing(&CipherRegistry::software());
        registry
    }

    /// The `hw_ca_op` CA hooks.
    pub fn sink(&self) -> HwSink<D> {
        HwSink {
            hw: self.clone(),
            pmts: BTreeMap::new(),
        }
    }
}

/// `hw_ca_add_pmt`/`hw_ca_del_pmt`: keep the bound PID set of each PMT current.
pub struct HwSink<D: CaDevice + Send> {
    hw: Hw<D>,
    /// Live PMTs, so PIDs another PMT still uses stay bound.
    pmts: BTreeMap<usize, HwPmt>,
}

impl<D: CaDevice + Send + 'static> HwSink<D> {
    fn pmt(service: &CaService) -> HwPmt {
        HwPmt {
            id: service.pmt_id,
            adapter: Some(service.adapter),
            enabled: true,
            stream_pids: service.streams.iter().map(|s| s.pid).collect(),
        }
    }

    fn registry(&self) -> HwRegistry {
        HwRegistry {
            adapters: self.hw.adapters(),
            pmts: self.pmts.clone(),
        }
    }
}

impl<D: CaDevice + Send + 'static> CaSink for HwSink<D> {
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let pmt = Self::pmt(service);
        self.pmts.insert(pmt.id, pmt.clone());
        let reg = self.registry();
        if let Some(ad) = reg.adapter(service.adapter).copied() {
            let mut hw = self.hw.hw.lock().unwrap_or_else(|e| e.into_inner());
            hw.add_pmt(&ad, &pmt, update, &reg);
        }
        TablesResult::Ok
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        let pmt = Self::pmt(service);
        self.pmts.remove(&pmt.id);
        let reg = self.registry();
        if let Some(ad) = reg.adapter(service.adapter).copied() {
            let mut hw = self.hw.hw.lock().unwrap_or_else(|e| e.into_inner());
            hw.del_pmt(&ad, &pmt, &reg);
        }
        TablesResult::Ok
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ms_ca::StreamPid;
    use ms_ca_drivers::descramble::Descrambler;
    use ms_ca_drivers::hw_descrambler::fake::RecordingCaDevice;

    fn service(pids: &[u16]) -> CaService {
        CaService {
            pmt_id: 1,
            adapter: 0,
            sid: 0x64,
            pmt_pid: 0x100,
            streams: pids
                .iter()
                .map(|&pid| StreamPid {
                    stream_type: 2,
                    pid,
                })
                .collect(),
            ..CaService::default()
        }
    }

    #[test]
    fn control_words_reach_the_ca_device_and_pids_are_unbound() {
        let hw = Hw::new(RecordingCaDevice::with_node("/dev/dvb/adapter0/ca0", 4));
        let mut descrambler = Descrambler::new(hw.registry(0, 0, 0));
        descrambler.add_service(1, &[0x101, 0x102]);
        let cw = [1, 2, 3, 6, 4, 5, 6, 15];
        assert!(
            descrambler
                .send_cw(1, CwAlgo::Csa, 0, &cw, None, 0, 0, 1000)
                .is_ok()
        );
        let calls = || {
            hw.hw
                .lock()
                .unwrap()
                .device()
                .set_calls()
                .iter()
                .map(|c| format!("{c:?}"))
                .collect::<Vec<_>>()
        };
        let programmed = calls();
        assert!(
            programmed.iter().any(|c| c.contains("SetDescr")),
            "no CA_SET_DESCR: {programmed:?}"
        );
        assert!(programmed.iter().any(|c| c.contains("SetPid")));

        // A PMT update that drops 0x102 unbinds it (hw_ca_add_pmt).
        let mut sink = hw.sink();
        assert_eq!(
            sink.add_pmt(&service(&[0x101, 0x102]), false),
            TablesResult::Ok
        );
        let before = calls().len();
        assert_eq!(sink.add_pmt(&service(&[0x101]), true), TablesResult::Ok);
        assert!(calls().len() > before, "the dropped PID was not unbound");
        let before = calls().len();
        assert_eq!(sink.del_pmt(&service(&[0x101])), TablesResult::Ok);
        assert!(calls().len() > before, "del_pmt did not unbind");
    }
}
