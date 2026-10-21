//! `Tuner` implementation on top of a Linux DVB device.

use std::collections::BTreeMap;
use std::io;

use super::consts::{DVR_BUFFER, FE_HAS_LOCK};
use super::device::{DemuxFd, DvbDevice, PesOutput, error_parts};
use super::state::AdapterState;
use super::sys::LinuxDvbDevice;
use super::transponder::DvbTransponder;
use crate::tuner::{
    PID_ALL, PidSet, Result, SignalStatus, TS_PACKET_SIZE, TsBuf, TuneParams, Tuner, TunerError,
};

/// Open DVB frontend and DVR with per-PID demux filters (`dvb_set_pid`).
pub struct LinuxDvbTuner<D: DvbDevice = LinuxDvbDevice> {
    dev: D,
    state: AdapterState,
    pids: BTreeMap<u16, DemuxFd>,
    status: SignalStatus,
    partial: Vec<u8>,
    read_buf: Vec<u8>,
}

impl LinuxDvbTuner<LinuxDvbDevice> {
    /// Opens `/dev/dvb/adapterN/frontendM` and `/dev/dvb/adapterN/dvrM`.
    pub fn open(adapter: u32, frontend: u32) -> io::Result<Self> {
        Self::open_with_buffer(adapter, frontend, DVR_BUFFER)
    }

    /// Opens a frontend with an explicit DVR buffer size (`-b`).
    pub fn open_with_buffer(adapter: u32, frontend: u32, dvr_buffer: i32) -> io::Result<Self> {
        Self::open_target(None, adapter, frontend, dvr_buffer)
    }

    /// Opens a runtime adapter and logs the `dvb_open_device` lines, including
    /// the logical adapter id (`ad->id`).
    pub fn open_for_adapter(
        aid: i32,
        adapter: u32,
        frontend: u32,
        dvr_buffer: i32,
    ) -> io::Result<Self> {
        Self::open_target(Some(aid), adapter, frontend, dvr_buffer)
    }

    fn open_target(
        aid: Option<i32>,
        adapter: u32,
        frontend: u32,
        dvr_buffer: i32,
    ) -> io::Result<Self> {
        if let Some(aid) = aid {
            ms_log::log!(
                "trying to open [{}] adapter {} and frontend {}",
                aid,
                adapter,
                frontend
            );
        }
        let dev = LinuxDvbDevice::open(adapter, frontend)?;
        if let Some(aid) = aid {
            ms_log::log!(
                "opened DVB adapter {} fe:{} dvr:{}",
                aid,
                dev.frontend_fd(),
                dev.dvr_fd()
            );
        }
        Ok(Self::with_dvr_buffer(
            dev,
            AdapterState::alloc(),
            dvr_buffer,
        ))
    }
}

impl<D: DvbDevice> LinuxDvbTuner<D> {
    /// Wraps an already-open device and adapter state.
    pub fn with_device(dev: D, state: AdapterState) -> Self {
        Self::with_dvr_buffer(dev, state, DVR_BUFFER)
    }

    /// Wraps a device, setting `DMX_SET_BUFFER_SIZE` to `dvr_buffer` bytes
    /// (`-b`, dvb.cpp:926).
    pub fn with_dvr_buffer(mut dev: D, mut state: AdapterState, dvr_buffer: i32) -> Self {
        state.fe = 1;
        if let Err(error) = dev.set_buffer_size(dev.stream_fd(), dvr_buffer) {
            let (errno, text) = error_parts(&error);
            ms_log::log!("couldn't set DVR buffer size error {}: {}", errno, text);
        } else {
            ms_log::log!("DVR buffer set to {} bytes", dvr_buffer);
        }
        Self {
            dev,
            state,
            pids: BTreeMap::new(),
            status: SignalStatus::default(),
            partial: Vec::with_capacity(TS_PACKET_SIZE),
            read_buf: vec![0u8; 64 * 1024],
        }
    }

    /// Per-adapter state (switch cache, multipliers, transponder).
    pub fn state(&self) -> &AdapterState {
        &self.state
    }

    /// Mutable per-adapter state.
    pub fn state_mut(&mut self) -> &mut AdapterState {
        &mut self.state
    }

    /// Replaces the switch/LNB parameters used by the next tune.
    pub fn set_diseqc_params(&mut self, params: super::transponder::DiseqcParams) {
        self.state.tp.diseqc_param = params;
    }

    /// The wrapped device.
    pub fn device(&self) -> &D {
        &self.dev
    }

    /// Mutable access to the wrapped device.
    pub fn device_mut(&mut self) -> &mut D {
        &mut self.dev
    }

    /// Keeps the cached status in sync with the frontend (`dvb_get_signal`).
    fn refresh_status(&mut self) {
        let _ = self.state.get_signal(&mut self.dev);
        self.status = SignalStatus {
            locked: self.state.status & FE_HAS_LOCK != 0,
            strength: self.state.strength,
            snr: self.state.snr,
        };
    }
}

impl<D: DvbDevice> Tuner for LinuxDvbTuner<D> {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        let tp = DvbTransponder::from_params(params, self.state.tp.diseqc_param);
        let ret = self.state.tune(&mut self.dev, &tp);
        if ret < 0 {
            return Err(TunerError::Backend(ret));
        }
        self.status = SignalStatus::default();
        Ok(())
    }

    fn set_pids(&mut self, pids: &PidSet) -> Result<()> {
        for pid in pids.iter() {
            if pid > PID_ALL {
                ms_log::log!(
                    "pid {} > 8192 for /dev/dvb/adapter{}/demux{}",
                    pid,
                    self.state.pa,
                    self.state.fn_num
                );
                return Err(TunerError::Backend(-1));
            }
        }
        let remove: Vec<u16> = self
            .pids
            .keys()
            .filter(|pid| !pids.contains_exact(**pid))
            .copied()
            .collect();
        for pid in remove {
            if let Some(fd) = self.pids.remove(&pid) {
                let ctime = self.dev.now_ms();
                // Best effort, like dvb_del_filters (dvb.cpp:1601).
                if let Err(error) = self.dev.demux_stop(fd) {
                    let (errno, text) = error_parts(&error);
                    ms_log::log0!(
                        "DMX_STOP failed on PID {} FD {}: error {} {}",
                        pid,
                        fd.0,
                        errno,
                        text
                    );
                } else {
                    ms_log::log!(
                        "clearing filter on PID {} FD {} [{} ms]",
                        pid,
                        fd.0,
                        self.dev.now_ms() - ctime
                    );
                }
                self.dev.close_demux(fd);
            }
        }
        for pid in pids.iter() {
            if self.pids.contains_key(&pid) {
                continue;
            }
            let ctime = self.dev.now_ms();
            let fd = match self.dev.open_demux() {
                Ok(fd) => fd,
                Err(error) => {
                    let (_, text) = error_parts(&error);
                    ms_log::log!(
                        "Could not open demux device /dev/dvb/adapter{}/demux{}: {} ",
                        self.state.pa,
                        self.state.fn_num,
                        text
                    );
                    return Err(TunerError::from(error));
                }
            };
            if let Err(error) = self.dev.set_pes_filter(fd, pid, PesOutput::TsTap) {
                let (errno, text) = error_parts(&error);
                ms_log::log0!(
                    "failed setting filter on /dev/dvb/adapter{}/demux{} pid {}, errno {} ({}), enabled pids {}",
                    self.state.pa,
                    self.state.fn_num,
                    pid,
                    errno,
                    text,
                    self.pids.len()
                );
                self.dev.close_demux(fd);
                return Err(TunerError::from(error));
            }
            ms_log::log!(
                "AD {} [dvr {} {}], setting filter on PID {} for fd {} [{} ms]",
                self.state.id,
                self.state.pa,
                self.state.fn_num,
                pid,
                fd.0,
                self.dev.now_ms() - ctime
            );
            self.pids.insert(pid, fd);
        }
        Ok(())
    }

    fn status(&self) -> SignalStatus {
        self.status
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        self.refresh_status();
        let mut appended = 0;
        loop {
            while self.partial.len() >= TS_PACKET_SIZE && buf.remaining_bytes() >= TS_PACKET_SIZE {
                let packet = &self.partial[..TS_PACKET_SIZE];
                if !buf.push_packet(packet) {
                    break;
                }
                self.partial.drain(..TS_PACKET_SIZE);
                appended += TS_PACKET_SIZE;
            }
            if buf.remaining_bytes() < TS_PACKET_SIZE || self.partial.len() >= TS_PACKET_SIZE {
                break;
            }
            match self.dev.read_stream(&mut self.read_buf) {
                Ok(0) => break,
                Ok(count) => self.partial.extend_from_slice(&self.read_buf[..count]),
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => break,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                Err(error) => return Err(TunerError::from(error)),
            }
        }
        Ok(appended)
    }
}
