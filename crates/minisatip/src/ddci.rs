//! DDCI (`ddci.cpp`): tuner programs are remapped onto a CI adapter's CAM and
//! back; one [`DdciHub`] mutex is shared by the threads involved (DEV-215).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use ms_ca::{CaService, ProgramTable, TablesResult};
use ms_ca_drivers::ddci::{CiWriter, DDCI_SID, Ddci, TsBuffer};
use ms_sched::pmt::CaSink;
use ms_ts::TsStage;

/// Shared DDCI state.
pub struct DdciState {
    pub ddci: Ddci,
    pub programs: ProgramTable,
    writers: BTreeMap<usize, Box<dyn CiWriter + Send>>,
    origin: Instant,
}

/// Handle to the shared DDCI state.
#[derive(Clone)]
pub struct DdciHub(Arc<Mutex<DdciState>>);

impl Default for DdciHub {
    fn default() -> Self {
        Self::new()
    }
}

impl DdciHub {
    pub fn new() -> Self {
        Self(Arc::new(Mutex::new(DdciState {
            ddci: Ddci::new(),
            programs: ProgramTable::new(),
            writers: BTreeMap::new(),
            origin: Instant::now(),
        })))
    }

    /// The state; a poisoned lock still holds consistent tables.
    pub fn lock(&self) -> MutexGuard<'_, DdciState> {
        self.0.lock().unwrap_or_else(|poison| poison.into_inner())
    }

    /// `ddci_open_device`: CI adapter `aid` writes to its CAM through `writer`.
    pub fn open_device(&self, aid: usize, writer: Box<dyn CiWriter + Send>) {
        let mut state = self.lock();
        state.ddci.create_adapter(aid as u16);
        let device = ms_ca_drivers::ddci::DdciDevice::opened(aid as i32);
        state.ddci.devices.insert(aid, device);
        state.ddci.post_tune_ci(aid as u16);
        state.writers.insert(aid, writer);
    }

    /// `(adapter, pid)` pairs DDCI subscribed with `DDCI_SID`.
    pub fn ddci_pids(&self) -> Vec<(usize, u16)> {
        let state = self.lock();
        let mut pids = Vec::new();
        for (aid, adapter) in &state.ddci.adapters {
            for (pid, entry) in &adapter.pids {
                if !entry.deleted && entry.sids.contains(&DDCI_SID) {
                    pids.push((usize::from(*aid), *pid));
                }
            }
        }
        pids
    }
}

/// `ddci_ts` on a tuner adapter: mapped packets go to the CAMs and their
/// output replaces null packets. Runs before the descrambler (pmt.cpp:1520).
pub struct DdciStage {
    hub: DdciHub,
    aid: u16,
}

impl DdciStage {
    pub fn new(hub: DdciHub, aid: usize) -> Self {
        hub.lock().ddci.create_adapter(aid as u16);
        Self {
            hub,
            aid: aid as u16,
        }
    }
}

impl TsStage for DdciStage {
    fn process(&mut self, buf: &mut [u8]) {
        let mut data = buf.to_vec();
        let capacity = data.len();
        self.process_buf(&mut data, capacity);
        buf.copy_from_slice(&data[..buf.len()]);
    }

    fn process_buf(&mut self, buf: &mut Vec<u8>, capacity: usize) {
        let mut guard = self.hub.lock();
        let state = &mut *guard;
        if state.ddci.devices.contains_key(&usize::from(self.aid)) {
            return;
        }
        let devices: Vec<usize> = state
            .ddci
            .devices
            .iter()
            .filter(|(_, d)| d.enabled)
            .map(|(id, _)| *id)
            .collect();
        if devices.is_empty() {
            return;
        }
        state.ddci.now_ms = state.origin.elapsed().as_millis() as u64;
        // CAM output fills null slots and may grow the batch up to the adapter
        // buffer (`ad->rlen` up to `ad->lbuf`, ddci.cpp:1105). The zeroed tail
        // (about 376 KB) reuses the batch allocation.
        let rlen = buf.len();
        let mut data = std::mem::take(buf);
        data.resize(capacity.max(rlen), 0);
        let mut ts = TsBuffer {
            id: self.aid,
            buf: data,
            rlen,
        };
        for dev in devices {
            if let Some(writer) = state.writers.get_mut(&dev) {
                state
                    .ddci
                    .process_ts(&mut ts, dev, &mut state.programs, writer.as_mut());
            }
        }
        let mut data = ts.buf;
        data.truncate(ts.rlen);
        *buf = data;
    }
}

/// `ddci_read_sec_data`: the CI adapter's batch, after its own PSI pass,
/// goes to the device FIFO the tuner adapters read from.
pub struct DdciCapture {
    hub: DdciHub,
    aid: usize,
}

impl DdciCapture {
    pub fn new(hub: DdciHub, aid: usize) -> Self {
        Self { hub, aid }
    }
}

impl TsStage for DdciCapture {
    fn process(&mut self, buf: &mut [u8]) {
        self.hub.lock().ddci.push_from_cam(self.aid, buf);
    }
}

/// The DDCI `SCA_op` (`ddci_process_pmt`/`ddci_del_pmt`).
pub struct DdciSink {
    pub hub: DdciHub,
}

impl CaSink for DdciSink {
    fn add_pmt(&mut self, service: &CaService, update: bool) -> TablesResult {
        let program = crate::dvbca::program_from_service(service);
        let mut guard = self.hub.lock();
        let state = &mut *guard;
        state.ddci.now_ms = state.origin.elapsed().as_millis() as u64;
        state.programs.insert(program.clone());
        state
            .ddci
            .process_pmt(service.adapter as u16, &program, update)
    }

    fn del_pmt(&mut self, service: &CaService) -> TablesResult {
        let program = crate::dvbca::program_from_service(service);
        self.hub
            .lock()
            .ddci
            .del_pmt(service.adapter as u16, &program);
        TablesResult::Ok
    }
}

/// Linux DDCI devices: `/dev/dvb/adapterN/caM` next to `secM` or `ciM`.
#[cfg(all(feature = "linux-dvb", target_os = "linux"))]
pub mod linux {
    use std::fs::{File, OpenOptions};
    use std::io::{self, Read, Write};
    use std::os::unix::fs::OpenOptionsExt;
    use std::os::unix::io::AsRawFd;
    use std::path::Path;

    use ms_ca_drivers::ddci::CiWriter;
    use ms_tuner::{PidSet, SignalStatus, TsBuf, TuneParams, Tuner, TunerError};

    /// One DDCI pair found by `find_ddci_adapter`.
    pub struct DdciNode {
        pub pa: u32,
        pub fe: u32,
    }

    /// `find_ddci_adapter`: CA plus sec (or ci) node of the same number.
    pub fn find() -> Vec<DdciNode> {
        let mut nodes = Vec::new();
        for pa in 0..16u32 {
            for fe in 0..8u32 {
                let base = format!("/dev/dvb/adapter{pa}");
                let ca = Path::new(&format!("{base}/ca{fe}")).exists();
                let sec = Path::new(&format!("{base}/sec{fe}")).exists()
                    || Path::new(&format!("{base}/ci{fe}")).exists();
                if ca && sec {
                    nodes.push(DdciNode { pa, fe });
                }
            }
        }
        nodes
    }

    /// `ddci_open_device`: the write and read handles of the CAM TS interface.
    pub fn open(aid: usize, node: &DdciNode) -> io::Result<(DdciReader, DdciWriter)> {
        ms_log::log!(
            "DDCI opening [{}] adapter {} and frontend {}",
            aid,
            node.pa,
            node.fe
        );
        let open = |path: &str, write: bool| {
            OpenOptions::new()
                .read(!write)
                .write(write)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
        };
        let sec = format!("/dev/dvb/adapter{}/sec{}", node.pa, node.fe);
        let ci = format!("/dev/dvb/adapter{}/ci{}", node.pa, node.fe);
        let (writer, path) = match open(&sec, true) {
            Ok(file) => (file, sec),
            Err(_) => match open(&ci, true) {
                Ok(file) => (file, ci),
                Err(error) => {
                    ms_log::log!(
                        "ddci_open_device: could not open {} in WRONLY mode error {}: {}",
                        ci,
                        ms_log::errno(&error),
                        ms_log::strerror(&error)
                    );
                    return Err(error);
                }
            },
        };
        let reader = match open(&path, false) {
            Ok(file) => file,
            Err(error) => {
                ms_log::log!(
                    "ddci_open_device: could not open {} in RDONLY mode error {}: {}",
                    path,
                    ms_log::errno(&error),
                    ms_log::strerror(&error)
                );
                return Err(error);
            }
        };
        let writer = DdciWriter::new(writer);
        ms_log::log!(
            "opened DDCI adapter {} fe:{} dvr:{}",
            aid,
            CiWriter::fd(&writer),
            reader.as_raw_fd()
        );
        Ok((DdciReader(reader), writer))
    }

    /// The CAM output as the CI adapter's tuner (`ad->dvr`).
    pub struct DdciReader(File);

    impl Tuner for DdciReader {
        fn tune(&mut self, _params: &TuneParams) -> Result<(), TunerError> {
            Ok(())
        }

        fn set_pids(&mut self, _pids: &PidSet) -> Result<(), TunerError> {
            Ok(())
        }

        fn status(&self) -> SignalStatus {
            SignalStatus::default()
        }

        fn read(&mut self, buf: &mut TsBuf) -> Result<usize, TunerError> {
            let mut chunk = vec![0u8; buf.remaining_bytes()];
            match self.0.read(&mut chunk) {
                Ok(n) => Ok(buf.push_data(&chunk[..n])),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
                Err(e) => Err(TunerError::Io(e)),
            }
        }
    }

    /// `writev` on `ad->fe` of the CI adapter.
    pub struct DdciWriter {
        file: File,
        /// `errno` after a failed write, for the incomplete-write log.
        errno: i32,
        error: String,
    }

    impl DdciWriter {
        fn new(file: File) -> Self {
            Self {
                file,
                errno: 0,
                error: String::new(),
            }
        }
    }

    impl CiWriter for DdciWriter {
        fn writev(&mut self, iov: &[&[u8]]) -> isize {
            let slices: Vec<io::IoSlice<'_>> = iov.iter().map(|s| io::IoSlice::new(s)).collect();
            match self.file.write_vectored(&slices) {
                Ok(n) => n as isize,
                Err(error) => {
                    self.errno = ms_log::errno(&error);
                    self.error = ms_log::strerror(&error);
                    -1
                }
            }
        }

        fn last_errno(&self) -> i32 {
            self.errno
        }

        fn last_error(&self) -> &str {
            &self.error
        }

        fn fd(&self) -> i32 {
            use std::os::unix::io::AsRawFd;
            self.file.as_raw_fd()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ca::CaRuntime;
    use crate::dvbca::{Dvbca, DvbcaSettings};
    use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY};
    use ms_ca_drivers::en50221::{
        AdapterInfo, AdapterKind, CaLink, CaOpener, SlotInfo, SystemClock,
    };
    use ms_config::Options;
    use ms_fakecam::{CamConfig, FakeCam};
    use ms_net::dataplane::{AdapterHandle, Command, DataPlaneConfig, DataPlaneTuner, Event};
    use ms_net::output::{OutputConfig, StreamOutput, UdpTarget};
    use ms_sched::{AdapterConfig, AdapterType, Scheduler};
    use ms_ts::crc32_mpeg2;
    use ms_tuner::{
        DeliverySystem, FileReplayConfig, FileReplayTuner, Pacing, PidSet, SignalStatus, TsBuf,
        TuneParams, Tuner, TunerError,
    };
    use std::collections::VecDeque;
    use std::io;
    use std::net::UdpSocket;
    use std::time::Duration;

    type Cam = Arc<Mutex<FakeCam>>;

    /// The CAM's CI slot (EN 50221 TPDUs).
    struct SlotLink {
        cam: Cam,
        rx: VecDeque<Vec<u8>>,
    }

    impl CaLink for SlotLink {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(msg) = self.rx.pop_front() else {
                return Err(io::ErrorKind::WouldBlock.into());
            };
            buf[..msg.len()].copy_from_slice(&msg);
            Ok(msg.len())
        }

        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            if let Some(reply) = self.cam.lock().unwrap().handle_tpdu(buf) {
                self.rx.push_back(reply);
            }
            Ok(buf.len())
        }

        fn reset(&mut self) -> io::Result<()> {
            Ok(())
        }

        fn slot_info(&mut self) -> io::Result<SlotInfo> {
            Ok(SlotInfo {
                kind: CA_CI_LINK,
                flags: CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY,
            })
        }

        fn sleep_ms(&mut self, _: u32) {}
    }

    struct SlotOpener(Cam);

    impl CaOpener for SlotOpener {
        fn open(&mut self, _: &str) -> io::Result<Box<dyn CaLink>> {
            Ok(Box::new(SlotLink {
                cam: Arc::clone(&self.0),
                rx: VecDeque::new(),
            }))
        }

        fn has_ciplus_root_cert(&self) -> bool {
            false
        }
    }

    /// The CAM's TS input (`sec0` write side).
    struct CamWriter(Cam);

    impl CiWriter for CamWriter {
        fn writev(&mut self, iov: &[&[u8]]) -> isize {
            let mut cam = self.0.lock().unwrap();
            iov.iter()
                .map(|part| {
                    cam.write_ts(part);
                    part.len() as isize
                })
                .sum()
        }
    }

    /// The CAM's TS output (`sec0` read side) as the CI adapter's tuner.
    struct CamReader {
        cam: Cam,
        pending: Vec<u8>,
    }

    impl Tuner for CamReader {
        fn tune(&mut self, _: &TuneParams) -> Result<(), TunerError> {
            Ok(())
        }

        fn set_pids(&mut self, _: &PidSet) -> Result<(), TunerError> {
            Ok(())
        }

        fn status(&self) -> SignalStatus {
            SignalStatus::default()
        }

        fn read(&mut self, buf: &mut TsBuf) -> Result<usize, TunerError> {
            let data = self.cam.lock().unwrap().read_ts();
            self.pending.extend_from_slice(&data);
            let used = buf.push_data(&self.pending);
            self.pending.drain(..used);
            Ok(used)
        }
    }

    const SID: u16 = 0x64;
    const PMT_PID: u16 = 0x100;
    const VIDEO_PID: u16 = 0x101;

    fn section_packet(pid: u16, body: &[u8], cc: u8) -> [u8; 188] {
        let mut section = body.to_vec();
        let len = section.len() + 4 - 3;
        section[1] = 0xB0 | (len >> 8) as u8;
        section[2] = len as u8;
        let crc = crc32_mpeg2(&section);
        section.extend_from_slice(&crc.to_be_bytes());
        let mut packet = [0xFF; 188];
        packet[..5].copy_from_slice(&[0x47, 0x40 | (pid >> 8) as u8, pid as u8, 0x10 | cc, 0]);
        packet[5..5 + section.len()].copy_from_slice(&section);
        packet
    }

    /// PAT, a PMT with a CA descriptor, and video with the scrambling bits set.
    fn scrambled_mux(path: &std::path::Path) {
        let pat = [0, 0, 0, 0, 1, 0xC1, 0, 0, 0, SID as u8, 0xE1, PMT_PID as u8];
        let pmt = [
            2,
            0,
            0,
            0,
            SID as u8,
            0xC1,
            0,
            0,
            0xE1,
            VIDEO_PID as u8,
            0xF0,
            6, //
            9,
            4,
            0x0B,
            0x00,
            0xE5,
            0x00, //
            2,
            0xE1,
            VIDEO_PID as u8,
            0xF0,
            0,
        ];
        let mut data = Vec::new();
        for cycle in 0..40u8 {
            data.extend_from_slice(&section_packet(0, &pat, cycle & 0xF));
            data.extend_from_slice(&section_packet(PMT_PID, &pmt, cycle & 0xF));
            for i in 0..20u8 {
                let mut packet = [i; 188];
                packet[..4].copy_from_slice(&[
                    0x47,
                    (VIDEO_PID >> 8) as u8,
                    VIDEO_PID as u8,
                    0x90 | (cycle.wrapping_mul(20).wrapping_add(i) & 0xF),
                ]);
                data.extend_from_slice(&packet);
            }
        }
        std::fs::write(path, data).unwrap();
    }

    fn udp_pair() -> (UdpSocket, u16) {
        loop {
            let first = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
            let port = first.local_addr().unwrap().port();
            if port < u16::MAX && UdpSocket::bind(("127.0.0.1", port + 1)).is_ok() {
                return (first, port);
            }
        }
    }

    fn free_server_port() -> u16 {
        loop {
            let probe = UdpSocket::bind(("127.0.0.1", 0)).unwrap();
            let port = probe.local_addr().unwrap().port();
            drop(probe);
            if port < u16::MAX && UdpSocket::bind(("127.0.0.1", port + 1)).is_ok() {
                return port;
            }
        }
    }

    #[test]
    fn ddci_cam_descrambles_the_tuner_service() {
        let cam: Cam = Arc::new(Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ms_fakecam::ManualClock::new(0)),
        )));
        let dir = std::env::temp_dir().join(format!("ms-ddci-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let capture = dir.join("mux.ts");
        scrambled_mux(&capture);

        let opts = Options::default();
        let mut sched = Scheduler::new();
        let mut ca = CaRuntime::new(&opts);
        let dvbca = Dvbca::spawn(
            DvbcaSettings::from_options(&opts),
            Box::new(SlotOpener(Arc::clone(&cam))),
            Arc::new(SystemClock::default()),
        )
        .unwrap();
        ca.attach_dvbca(&mut sched, dvbca);
        let hub = DdciHub::new();
        ca.attach_ddci(&mut sched, hub.clone());

        // Adapter 0: the tuner; adapter 1: the DDCI device.
        let tuner = FileReplayTuner::open(
            &capture,
            FileReplayConfig {
                looping: true,
                pacing: Pacing::FixedBitrate(2_000_000),
            },
        )
        .unwrap();
        let config = |id| DataPlaneConfig {
            adapter_id: id,
            ..DataPlaneConfig::default()
        };
        let source =
            AdapterHandle::spawn_with_stages(Box::new(tuner), config(0), None, ca.stages(0))
                .unwrap();
        sched.add_adapter(
            0,
            AdapterConfig::new(DeliverySystem::DvbS2),
            Box::new(DataPlaneTuner::new(source.command_sender())),
        );
        ca.adapter_added(&mut sched, 0);

        hub.open_device(1, Box::new(CamWriter(Arc::clone(&cam))));
        let reader = CamReader {
            cam: Arc::clone(&cam),
            pending: Vec::new(),
        };
        let ci =
            AdapterHandle::spawn_with_stages(Box::new(reader), config(1), None, ca.ci_stages(1))
                .unwrap();
        ci.send(Command::KeepReading(true));
        sched.add_adapter(
            1,
            AdapterConfig {
                enabled: true,
                adapter_type: AdapterType::Ci,
                ..AdapterConfig::default()
            },
            Box::new(DataPlaneTuner::new(ci.command_sender())),
        );
        sched.post_tune_adapter(1);
        ca.adapter_added(&mut sched, 1);
        ca.dvbca_init_dev(AdapterInfo {
            id: 1,
            enabled: true,
            kind: AdapterKind::Ci,
            pa: 0,
            fe: 0,
        });
        let handles = vec![Some(source), Some(ci)];

        // A client stream on the tuner asks for the service.
        for pid in [0, i32::from(PMT_PID), i32::from(VIDEO_PID)] {
            sched.mark_pid_add(0, 0, pid);
        }
        sched.post_tune_adapter(0);
        let (client, client_port) = udp_pair();
        client
            .set_read_timeout(Some(Duration::from_millis(20)))
            .unwrap();
        let output = StreamOutput::udp(
            UdpTarget {
                local: (std::net::Ipv4Addr::LOCALHOST, free_server_port()).into(),
                dest: (std::net::Ipv4Addr::LOCALHOST, client_port).into(),
                ttl: None,
            },
            OutputConfig {
                ssrc: 1,
                seq: 0,
                fifo_size: 1 << 20,
            },
            0,
        )
        .unwrap();
        let pids = PidSet::from_pids([0, PMT_PID, VIDEO_PID]);
        handles[0].as_ref().unwrap().start_output(0, output, pids);
        ca.after_scheduler(&mut sched, &handles);

        let mut clear = 0;
        let mut scrambled_after_clear = 0;
        let deadline = Instant::now() + Duration::from_secs(10);
        let mut buf = [0u8; 2048];
        while clear < 100 && Instant::now() < deadline {
            for (aid, handle) in handles.iter().enumerate() {
                let handle = handle.as_ref().unwrap();
                while let Ok(event) = handle.events().try_recv() {
                    if let Event::TablePackets {
                        generation,
                        packets,
                    } = event
                    {
                        ca.on_table_packets(&mut sched, &handles, aid, generation, &packets);
                    }
                }
            }
            ca.tick(&mut sched, &handles);
            while let Ok(len) = client.recv(&mut buf) {
                for packet in buf[12..len].chunks(188) {
                    let pid = (u16::from(packet[1] & 0x1F) << 8) | u16::from(packet[2]);
                    if pid != VIDEO_PID {
                        continue;
                    }
                    if packet[3] & 0xC0 == 0 {
                        assert!(packet[5..].iter().all(|b| *b == packet[4]), "payload kept");
                        clear += 1;
                    } else if clear > 0 {
                        scrambled_after_clear += 1;
                    }
                }
            }
        }
        let programs: Vec<u16> = cam
            .lock()
            .unwrap()
            .programs()
            .programs()
            .iter()
            .map(|p| p.number())
            .collect();
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!programs.is_empty(), "the CAM got no CAPMT");
        assert!(clear >= 100, "only {clear} descrambled packets came back");
        assert_eq!(scrambled_after_clear, 0);
        assert!(cam.lock().unwrap().ts_stats().descrambled > 0);
    }
}

#[cfg(test)]
mod growth_tests {
    use super::*;
    use ms_ca_drivers::ddci::{pid_from_ts, set_pid_ts};

    /// The CI side: records what the stage writes, for the CAM to send back later.
    struct Loopback(Arc<Mutex<Vec<u8>>>);

    impl CiWriter for Loopback {
        fn writev(&mut self, iov: &[&[u8]]) -> isize {
            let mut sent = self.0.lock().unwrap();
            for part in iov {
                sent.extend_from_slice(part);
            }
            iov.iter().map(|part| part.len()).sum::<usize>() as isize
        }
    }

    // ddci.cpp:1105: CAM output may grow the batch up to the adapter buffer, so
    // a stream fully routed through the CAM loses nothing and builds no backlog.
    #[test]
    fn cam_output_grows_the_batch_instead_of_backing_up() {
        const BATCH: usize = 50;
        const BATCHES: usize = 200;
        let hub = DdciHub::new();
        let sent = Arc::new(Mutex::new(Vec::new()));
        hub.open_device(1, Box::new(Loopback(Arc::clone(&sent))));
        let mut stage = DdciStage::new(hub.clone(), 0);
        hub.lock().ddci.add_pid_mapping_table(0, 1000, 0, 1, false);
        let mut delivered = 0;
        for _ in 0..BATCHES {
            // The CAM returns the previous batch's packets, PSI and null tail excluded.
            let echoed: Vec<u8> = std::mem::take(&mut *sent.lock().unwrap())
                .chunks(188)
                .filter(|p| pid_from_ts(p) == 1000)
                .flatten()
                .copied()
                .collect();
            hub.lock().ddci.push_from_cam(1, &echoed);
            let mut batch = vec![0u8; BATCH * 188];
            for p in batch.chunks_mut(188) {
                p[0] = 0x47;
                p[3] = 0x10;
                set_pid_ts(p, 1000);
            }
            stage.process_buf(&mut batch, 2000 * 188);
            assert_eq!(batch.len() % 188, 0);
            delivered += batch.chunks(188).filter(|p| pid_from_ts(p) == 1000).count();
        }
        let state = hub.lock();
        let d = &state.ddci.devices[&1];
        assert_eq!(
            d.fifo.write_index - d.read_index[0],
            0,
            "CAM output left in the FIFO"
        );
        // All but two batches came back: the first reply predates the adapter's
        // first read, which starts at the write index (ddci.cpp:1179), and the
        // last batch is still at the CAM.
        assert_eq!(delivered, BATCH * (BATCHES - 2));
    }
}
