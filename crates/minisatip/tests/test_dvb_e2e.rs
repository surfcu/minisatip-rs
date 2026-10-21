//! In-process SETUP+PLAY through `SessionManager` onto a fake DVB device.
//!
//! The scheduler and the real `LinuxDvbTuner` run unchanged; only the kernel
//! ioctls are recorded by [`SharedDevice`].

#![cfg(all(feature = "linux-dvb", target_os = "linux"))]

use std::collections::BTreeSet;
use std::io;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use ms_net::dataplane::{AdapterHandle, DataPlaneConfig, DataPlaneTuner};
use ms_net::handler::{HandlerContext, handle_rtsp};
use ms_net::rtsp::{ParseOutcome, Protocol, Request, parse_request};
use ms_net::session::SessionManager;
use ms_sched::{AdapterConfig, Scheduler};
use ms_tuner::DeliverySystem;
use ms_tuner::linux_dvb::*;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Call {
    SetProperties(Vec<DtvProperty>),
    SetTone(Tone),
    SetVoltage(Voltage),
    SendMasterCmd(DiseqcMessage),
    SendBurst(MiniCmd),
    SetBufferSize(DemuxFd, i32),
    OpenDemux(DemuxFd),
    SetPesFilter(DemuxFd, u16, PesOutput),
    DemuxStop(DemuxFd),
}

/// A `DvbDevice` whose frontend/demux ioctls are visible to the test thread.
struct SharedDevice {
    calls: Arc<Mutex<Vec<Call>>>,
    next_fd: u32,
    fail_set_properties: bool,
}

impl SharedDevice {
    fn new(calls: Arc<Mutex<Vec<Call>>>) -> Self {
        Self {
            calls,
            next_fd: 100,
            fail_set_properties: false,
        }
    }

    /// A frontend whose `FE_SET_PROPERTY` always fails, like a missing device.
    fn failing(calls: Arc<Mutex<Vec<Call>>>) -> Self {
        Self {
            fail_set_properties: true,
            ..Self::new(calls)
        }
    }
}

impl SecDevice for SharedDevice {
    fn set_tone(&mut self, tone: Tone) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call::SetTone(tone));
        Ok(())
    }

    fn set_voltage(&mut self, voltage: Voltage) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call::SetVoltage(voltage));
        Ok(())
    }

    fn send_master_cmd(&mut self, cmd: &DiseqcMessage) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call::SendMasterCmd(*cmd));
        Ok(())
    }

    fn send_burst(&mut self, burst: MiniCmd) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call::SendBurst(burst));
        Ok(())
    }

    fn sleep_ms(&mut self, _ms: i32) {}
}

impl DvbDevice for SharedDevice {
    fn frontend_open(&self) -> bool {
        true
    }

    fn set_properties(&mut self, props: &[DtvProperty]) -> io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::SetProperties(props.to_vec()));
        if self.fail_set_properties {
            return Err(io::Error::from_raw_os_error(5));
        }
        Ok(())
    }

    fn get_stats(&mut self) -> io::Result<[DtvStat; 3]> {
        Err(io::Error::from_raw_os_error(22))
    }

    fn enum_delsys(&mut self) -> io::Result<Vec<u32>> {
        Ok(vec![SYS_DVBS2 as u32, SYS_DVBS as u32])
    }

    fn get_info(&mut self) -> io::Result<FrontendInfo> {
        Ok(FrontendInfo {
            name: "E2E Mock FE".to_owned(),
            fe_type: FE_QPSK,
            caps: FE_CAN_2G_MODULATION,
        })
    }

    fn get_event(&mut self) -> io::Result<()> {
        Err(io::Error::from_raw_os_error(11))
    }

    fn read_status(&mut self) -> io::Result<i32> {
        Err(io::Error::from_raw_os_error(22))
    }

    fn read_ber(&mut self) -> io::Result<u32> {
        Err(io::Error::from_raw_os_error(22))
    }

    fn read_signal_strength(&mut self) -> io::Result<u16> {
        Err(io::Error::from_raw_os_error(22))
    }

    fn read_snr(&mut self) -> io::Result<u16> {
        Err(io::Error::from_raw_os_error(22))
    }

    fn stream_fd(&self) -> DemuxFd {
        DemuxFd(0)
    }

    fn open_demux(&mut self) -> io::Result<DemuxFd> {
        let fd = DemuxFd(self.next_fd);
        self.next_fd += 1;
        self.calls.lock().unwrap().push(Call::OpenDemux(fd));
        Ok(fd)
    }

    fn close_demux(&mut self, _fd: DemuxFd) {}

    fn set_pes_filter(&mut self, fd: DemuxFd, pid: u16, output: PesOutput) -> io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::SetPesFilter(fd, pid, output));
        Ok(())
    }

    fn add_pid(&mut self, _fd: DemuxFd, _pid: u16) -> io::Result<()> {
        Ok(())
    }

    fn remove_pid(&mut self, _fd: DemuxFd, _pid: u16) -> io::Result<()> {
        Ok(())
    }

    fn demux_stop(&mut self, fd: DemuxFd) -> io::Result<()> {
        self.calls.lock().unwrap().push(Call::DemuxStop(fd));
        Ok(())
    }

    fn set_buffer_size(&mut self, fd: DemuxFd, size: i32) -> io::Result<()> {
        self.calls
            .lock()
            .unwrap()
            .push(Call::SetBufferSize(fd, size));
        Ok(())
    }

    fn set_demux_source(&mut self, _source: i32) -> io::Result<()> {
        Ok(())
    }

    fn read_stream(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::from_raw_os_error(11))
    }

    fn now_ms(&self) -> i64 {
        0
    }
}

fn request(raw: &str) -> Request {
    match parse_request(raw.as_bytes()) {
        ParseOutcome::Complete { request, .. } => request,
        outcome => panic!("request did not parse: {outcome:?}"),
    }
}

fn wait_for_filters(calls: &Arc<Mutex<Vec<Call>>>, expected: &BTreeSet<u16>) -> Vec<Call> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = calls.lock().unwrap().clone();
        let seen: BTreeSet<u16> = snapshot
            .iter()
            .filter_map(|call| match call {
                Call::SetPesFilter(_, pid, PesOutput::TsTap) => Some(*pid),
                _ => None,
            })
            .collect();
        if expected.is_subset(&seen) {
            return snapshot;
        }
        if Instant::now() > deadline {
            panic!("timed out waiting for PES filters {expected:?}; recorded {snapshot:?}");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

/// Ported from tests/test_stream.cpp's PLAY path and dvb.cpp's set_pid:
/// C++ tunes the frontend, then installs PES filters for the adapter PIDs
/// (defaults 0/1/16/18 plus the client's pids).
#[test]
fn setup_play_drives_frontend_properties_and_pes_filters() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let device = SharedDevice::new(Arc::clone(&calls));
    let tuner = LinuxDvbTuner::with_device(device, AdapterState::alloc());
    let handle = AdapterHandle::spawn(Box::new(tuner), DataPlaneConfig::default())
        .expect("spawn data plane");
    let bridge = DataPlaneTuner::new(handle.command_sender());

    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(bridge),
    );
    let mut session = SessionManager::with_scheduler(scheduler);

    let url = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=100,200";
    let id = session
        .setup_stream_request(url, None, "SETUP")
        .expect("SETUP allocates a stream");
    session
        .decode_transport(id, "RTP/AVP;unicast;client_port=46000-46001")
        .expect("transport accepted");
    session.start_play(id, Protocol::Rtsp).expect("PLAY");

    let expected: BTreeSet<u16> = [0, 1, 16, 18, 100, 200].into_iter().collect();
    let recorded = wait_for_filters(&calls, &expected);

    // get_stream_pids only reports PIDs owned by this stream.
    assert_eq!(session.scheduler().stream_pids(0, id), vec![100, 200]);

    let props = recorded
        .iter()
        .rev()
        .find_map(|call| match call {
            Call::SetProperties(props) => Some(props.clone()),
            _ => None,
        })
        .expect("tune issues FE_SET_PROPERTY");
    // 11_362_000 - lnb_low(9_750_000), like get_lnb_int_freq.
    for property in [
        DtvProperty::new(DTV_DELIVERY_SYSTEM, SYS_DVBS2),
        DtvProperty::new(DTV_FREQUENCY, 1_612_000),
        DtvProperty::new(DTV_SYMBOL_RATE, 22_000_000),
        DtvProperty::new(DTV_TUNE, 0),
    ] {
        assert!(
            props.contains(&property),
            "missing {property:?} in {props:?}"
        );
    }

    assert!(
        recorded.contains(&Call::SetVoltage(Voltage::V18)),
        "horizontal polarisation selects 18 V"
    );
    assert!(
        recorded.contains(&Call::SetBufferSize(DemuxFd(0), DVR_BUFFER)),
        "open sets the DVR buffer"
    );

    let filtered: BTreeSet<u16> = recorded
        .iter()
        .filter_map(|call| match call {
            Call::SetPesFilter(_, pid, PesOutput::TsTap) => Some(*pid),
            _ => None,
        })
        .collect();
    assert_eq!(
        filtered, expected,
        "only the defaults and the requested pids are filtered"
    );
    assert!(!filtered.contains(&300));

    handle.stop();
}

/// C++ tunes inside the request path, so a failing `FE_SET_PROPERTY` makes
/// PLAY return 500 with no body (test_stream.cpp:640).
#[test]
fn play_reports_frontend_failure_as_500() {
    let calls = Arc::new(Mutex::new(Vec::new()));
    let device = SharedDevice::failing(Arc::clone(&calls));
    let tuner = LinuxDvbTuner::with_device(device, AdapterState::alloc());
    let handle = AdapterHandle::spawn(Box::new(tuner), DataPlaneConfig::default())
        .expect("spawn data plane");
    let bridge = DataPlaneTuner::new(handle.command_sender());

    let mut scheduler = Scheduler::new();
    scheduler.add_adapter(
        0,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(bridge),
    );
    let mut session = SessionManager::with_scheduler(scheduler);

    let setup_request = request(
        "SETUP rtsp://127.0.0.1/?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=100 \
         RTSP/1.0\r\n\
         CSeq: 1\r\n\
         Transport: RTP/AVP;unicast;client_port=46000-46001\r\n\r\n",
    );
    let setup = {
        let mut ctx = HandlerContext::new(&mut session, Protocol::Rtsp);
        handle_rtsp(&mut ctx, &setup_request).expect("SETUP answered")
    };
    assert_eq!(setup.status, 200, "SETUP must allocate the stream");
    let session_id = setup.session_id.expect("SETUP returns a session id");

    let play_request = request(&format!(
        "PLAY rtsp://127.0.0.1/ RTSP/1.0\r\nCSeq: 2\r\nSession: {session_id:010}\r\n\r\n"
    ));
    let play = {
        let mut ctx = HandlerContext::new(&mut session, Protocol::Rtsp);
        handle_rtsp(&mut ctx, &play_request).expect("PLAY answered")
    };

    assert_eq!(
        play.status, 500,
        "a failing FE_SET_PROPERTY must yield the C++ 500"
    );
    assert!(
        play.body.is_empty(),
        "the 500 response carries no body in C++"
    );
    assert!(
        calls.lock().unwrap().iter().any(|call| matches!(
            call,
            Call::SetProperties(props)
                if props.iter().any(|prop| prop.cmd == DTV_TUNE)
        )),
        "PLAY must have attempted the frontend tune"
    );

    handle.stop();
}
