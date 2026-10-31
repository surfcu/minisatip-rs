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

/// `opts.use_demux_device` (`-?`, dvb.h:265-275): where the stream is read
/// and how PIDs are opened.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DemuxMode {
    /// `USE_DVR`: one `DMX_OUT_TS_TAP` demux fd per PID, read from the DVR.
    #[default]
    Dvr,
    /// `USE_DEMUX`: every PID on the demux fd the stream is read from.
    Demux,
    /// `USE_PES_FILTERS_AND_DVR`: section filters for PSI, the DVR otherwise.
    PesFiltersAndDvr,
    /// `USE_PES_FILTERS_AND_DEMUX`: section filters for PSI, the demux fd otherwise.
    PesFiltersAndDemux,
}

impl DemuxMode {
    /// Maps the option value; anything outside 1-3 is the DVR.
    pub fn from_option(value: i32) -> Self {
        match value {
            1 => Self::Demux,
            2 => Self::PesFiltersAndDvr,
            3 => Self::PesFiltersAndDemux,
            _ => Self::Dvr,
        }
    }

    /// The stream is read from `demuxN` instead of `dvrN` (dvb.cpp:896-897).
    pub fn reads_demux(self) -> bool {
        matches!(self, Self::Demux | Self::PesFiltersAndDemux)
    }

    fn pes_filters(self) -> bool {
        matches!(self, Self::PesFiltersAndDvr | Self::PesFiltersAndDemux)
    }
}

/// How one PID is open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OpenPid {
    /// Own demux fd with `DMX_OUT_TS_TAP` (`dvb_set_pid`).
    Tap(DemuxFd),
    /// On the shared stream fd (`dvb_demux_set_pid`).
    Shared,
    /// Own section filter fd, re-packed into TS with its CC (`dvb_set_psi_filter`).
    Section(DemuxFd, i16),
}

/// Largest section read from a section filter (`dvb_psi_read`).
const SECTION_BUF: usize = 4096;

/// Open DVB frontend with per-PID demux filters in one of the `DemuxMode`s.
pub struct LinuxDvbTuner<D: DvbDevice = LinuxDvbDevice> {
    dev: D,
    state: AdapterState,
    mode: DemuxMode,
    pids: BTreeMap<u16, OpenPid>,
    /// `ad->active_demux_pids`: PIDs on the shared stream fd.
    active_demux_pids: usize,
    section_pids: PidSet,
    status: SignalStatus,
    partial: Vec<u8>,
    /// Whole TS packets built from sections, delivered before the stream.
    psi: Vec<u8>,
    read_buf: Vec<u8>,
}

impl LinuxDvbTuner<LinuxDvbDevice> {
    /// Opens `/dev/dvb/adapterN/frontendM` and `/dev/dvb/adapterN/dvrM`.
    pub fn open(adapter: u32, frontend: u32) -> io::Result<Self> {
        Self::open_with_buffer(adapter, frontend, DVR_BUFFER)
    }

    /// Opens a frontend with an explicit DVR buffer size (`-b`).
    pub fn open_with_buffer(adapter: u32, frontend: u32, dvr_buffer: i32) -> io::Result<Self> {
        Self::open_target(None, adapter, frontend, dvr_buffer, DemuxMode::Dvr)
    }

    /// Opens a runtime adapter and logs the `dvb_open_device` lines, including
    /// the logical adapter id (`ad->id`).
    pub fn open_for_adapter(
        aid: i32,
        adapter: u32,
        frontend: u32,
        dvr_buffer: i32,
        mode: DemuxMode,
    ) -> io::Result<Self> {
        Self::open_target(Some(aid), adapter, frontend, dvr_buffer, mode)
    }

    fn open_target(
        aid: Option<i32>,
        adapter: u32,
        frontend: u32,
        dvr_buffer: i32,
        mode: DemuxMode,
    ) -> io::Result<Self> {
        if let Some(aid) = aid {
            ms_log::log!(
                "trying to open [{}] adapter {} and frontend {}",
                aid,
                adapter,
                frontend
            );
        }
        let dev = LinuxDvbDevice::open_stream(adapter, frontend, mode.reads_demux())?;
        if let Some(aid) = aid {
            ms_log::log!(
                "opened DVB adapter {} fe:{} dvr:{}",
                aid,
                dev.frontend_fd(),
                dev.dvr_fd()
            );
        }
        let mut tuner =
            Self::with_dvr_buffer(dev, AdapterState::alloc(), dvr_buffer).with_demux_mode(mode);
        // ad->id, pa and fn exist before dvb_open_device logs with them.
        tuner.state.id = aid.unwrap_or(tuner.state.id);
        tuner.state.pa = adapter as i32;
        tuner.state.fn_num = frontend as i32;
        Ok(tuner)
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
            mode: DemuxMode::Dvr,
            pids: BTreeMap::new(),
            active_demux_pids: 0,
            section_pids: PidSet::new(),
            status: SignalStatus::default(),
            partial: Vec::with_capacity(TS_PACKET_SIZE),
            psi: Vec::new(),
            read_buf: vec![0u8; 64 * 1024],
        }
    }

    /// The tail of `dvb_open_device` (dvb.cpp:931-944): FBC links, then
    /// `DMX_SET_SOURCE` when `dmx_source` is set; `master_source` is -1 or
    /// the master adapter of a slave.
    pub fn post_open(&mut self, fbc: bool, master_source: i32, dmx_source: i32) {
        let fn_num = self.state.fn_num;
        if fbc {
            if master_source >= 0 {
                self.set_proc_data(fn_num, "fbc_link", 1);
                self.set_proc_data(fn_num, "fbc_connect", master_source);
            } else {
                if fn_num > 1 {
                    ms_log::log!(
                        "FBC master adapter is recommended to be adapter 0 or 1, depending on the number of physical inputs"
                    );
                }
                self.set_proc_data(fn_num, "fbc_link", 0);
                self.set_proc_data(fn_num, "fbc_connect", fn_num);
            }
        }
        if dmx_source >= 0 {
            let Ok(fd) = self.dev.open_demux() else {
                ms_log::log!("DMX_SET_SOURCE: Failed opening {}", self.demux_path());
                return;
            };
            let result = self.dev.set_demux_source(fd, dmx_source);
            self.dev.close_demux(fd);
            match result {
                Ok(()) => ms_log::log!(
                    "Set DMX_SET_SOURCE for adapter {} to {}",
                    self.state.id,
                    dmx_source
                ),
                Err(error) => {
                    let (errno, text) = error_parts(&error);
                    ms_log::log!(
                        "DMX_SET_SOURCE failed for adapter {} - {}: {}",
                        self.state.id,
                        errno,
                        text
                    );
                }
            }
        }
    }

    fn set_proc_data(&mut self, frontend: i32, name: &str, val: i32) {
        let filename = format!("/proc/stb/frontend/{frontend}/{name}");
        ms_log::log!("setProcData {} -> {}", filename, val);
        if let Err(error) = self.dev.set_proc_data(frontend, name, val) {
            ms_log::log!("setProcData open failed, {}: {}", filename, error);
        }
    }

    /// Opens PIDs as `mode` does; the device must read the matching stream fd.
    pub fn with_demux_mode(mut self, mode: DemuxMode) -> Self {
        self.mode = mode;
        self
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
            ber: self.state.ber,
            db: self.state.db,
            status_valid: self.state.status_valid,
        };
    }
}

impl<D: DvbDevice> LinuxDvbTuner<D> {
    fn demux_path(&self) -> String {
        format!(
            "/dev/dvb/adapter{}/demux{}",
            self.state.pa, self.state.fn_num
        )
    }

    fn open_demux_fd(&mut self) -> Result<DemuxFd> {
        self.dev.open_demux().map_err(|error| {
            let (_, text) = error_parts(&error);
            ms_log::log!(
                "Could not open demux device {}: {} ",
                self.demux_path(),
                text
            );
            TunerError::from(error)
        })
    }

    /// `dvb_set_pid` (dvb.cpp:1549): own fd, `DMX_OUT_TS_TAP` to the DVR.
    fn open_tap(&mut self, pid: u16) -> Result<OpenPid> {
        let ctime = self.dev.now_ms();
        let fd = self.open_demux_fd()?;
        if let Err(error) = self.dev.set_pes_filter(fd, pid, PesOutput::TsTap) {
            let (errno, text) = error_parts(&error);
            ms_log::log0!(
                "failed setting filter on {} pid {}, errno {} ({}), enabled pids {}",
                self.demux_path(),
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
        Ok(OpenPid::Tap(fd))
    }

    /// `dvb_demux_set_pid` (dvb.cpp:1642): the first PID sets a
    /// `DMX_OUT_TSDEMUX_TAP` filter on the stream fd, later ones `DMX_ADD_PID`.
    fn open_shared(&mut self, pid: u16) -> Result<OpenPid> {
        let fd = self.dev.stream_fd();
        let ctime = self.dev.now_ms();
        self.active_demux_pids += 1;
        if self.active_demux_pids == 1 {
            if let Err(error) = self.set_demux_pes_filter(fd, pid) {
                self.active_demux_pids -= 1;
                return Err(error);
            }
            ms_log::log!(
                "AD {} [dvr {} {}], setting filter on PID {} for fd {}, active pids {}",
                self.state.id,
                self.state.pa,
                self.state.fn_num,
                pid,
                fd.0,
                self.pids.len()
            );
            return Ok(OpenPid::Shared);
        }
        if let Err(error) = self.dev.add_pid(fd, pid) {
            let (errno, text) = error_parts(&error);
            ms_log::log0!(
                "failed to add pid {} to fd {} maximum pids {}: errno {}, {}",
                pid,
                fd.0,
                self.pids.len(),
                errno,
                text
            );
            let shared = self.pids.values().any(|open| *open == OpenPid::Shared);
            // dvb.cpp:1665 recovers a counter left stale by a reopen; each Rust
            // tuner starts at 0 and keeps it in step, so this is kept for parity.
            if errno == libc::EINVAL && !shared && self.set_demux_pes_filter(fd, pid).is_ok() {
                ms_log::log!(
                    "AD {} [demux {} {}], recovered stale demux counter, setting filter on PID {} for fd {}",
                    self.state.id,
                    self.state.pa,
                    self.state.fn_num,
                    pid,
                    fd.0
                );
                self.active_demux_pids = 1;
                return Ok(OpenPid::Shared);
            }
            self.active_demux_pids -= 1;
            return Err(TunerError::from(error));
        }
        ms_log::log!(
            "AD {} [demux {} {}], setting filter on PID {} for fd {} [{} ms]",
            self.state.id,
            self.state.pa,
            self.state.fn_num,
            pid,
            fd.0,
            self.dev.now_ms() - ctime
        );
        Ok(OpenPid::Shared)
    }

    /// `dvb_demux_set_pes_filter` (dvb.cpp:1622).
    fn set_demux_pes_filter(&mut self, fd: DemuxFd, pid: u16) -> Result<()> {
        self.dev
            .set_pes_filter(fd, pid, PesOutput::TsDemuxTap)
            .map_err(|error| {
                let (errno, text) = error_parts(&error);
                ms_log::log0!(
                    "failed setting filter on fd {}, adapter {}, pid {}, errno {} ({}), enabled pids {}",
                    fd.0,
                    self.state.id,
                    pid,
                    errno,
                    text,
                    self.pids.len()
                );
                TunerError::from(error)
            })
    }

    /// `dvb_set_psi_filter` (dvb.cpp:1771): own fd with a section filter.
    fn open_section(&mut self, pid: u16) -> Result<OpenPid> {
        let ctime = self.dev.now_ms();
        let fd = self.open_demux_fd()?;
        if let Err(error) = self.dev.set_section_filter(fd, pid) {
            let (errno, text) = error_parts(&error);
            ms_log::log0!(
                "dvb_set_psi_filter: failed setting filter on {} pid {}, errno {} ({}), enabled pids {}",
                self.demux_path(),
                pid,
                errno,
                text,
                self.pids.len()
            );
            self.dev.close_demux(fd);
            return Err(TunerError::from(error));
        }
        ms_log::log!(
            "AD {} [demux {} {}], setting PSI filter on PID {} for fd {} [{} ms]",
            self.state.id,
            self.state.pa,
            self.state.fn_num,
            pid,
            fd.0,
            self.dev.now_ms() - ctime
        );
        Ok(OpenPid::Section(fd, -1))
    }

    /// `dvb_del_filters`, `dvb_demux_del_filters` or `dvb_del_psi_filters`.
    fn close_pid(&mut self, pid: u16, open: OpenPid) {
        match open {
            OpenPid::Tap(fd) | OpenPid::Section(fd, _) => {
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
            OpenPid::Shared => {
                let fd = self.dev.stream_fd();
                let ctime = self.dev.now_ms();
                if let Err(error) = self.dev.remove_pid(fd, pid) {
                    let (errno, text) = error_parts(&error);
                    ms_log::log0!(
                        "failed to remove pid {} to fd {}: errno {}, {}",
                        pid,
                        fd.0,
                        errno,
                        text
                    );
                }
                self.active_demux_pids = self.active_demux_pids.saturating_sub(1);
                if self.active_demux_pids == 0 {
                    // Best effort: the next PID re-arms the demux (dvb.cpp:1702).
                    if let Err(error) = self.dev.demux_stop(fd) {
                        let (errno, text) = error_parts(&error);
                        ms_log::log!(
                            "DMX_STOP failed on PID {} FD {}: error {} {}",
                            pid,
                            fd.0,
                            errno,
                            text
                        );
                    }
                    ms_log::log!("stopped filters on fd {}", fd.0);
                }
                ms_log::log!(
                    "clearing demux filter on PID {} FD {}, active_pids {} [{} ms]",
                    pid,
                    fd.0,
                    self.active_demux_pids,
                    self.dev.now_ms() - ctime
                );
            }
        }
    }

    /// `dvb_psi_read` (dvb.cpp:1716): every pending section of every section
    /// filter, packed into TS packets with the PID's own continuity counter.
    fn read_sections(&mut self) {
        let mut section = [0u8; SECTION_BUF];
        let mut packets = vec![0u8; (SECTION_BUF / 184 + 2) * TS_PACKET_SIZE];
        let open: Vec<(u16, DemuxFd)> = self
            .pids
            .iter()
            .filter_map(|(pid, open)| match open {
                OpenPid::Section(fd, _) => Some((*pid, *fd)),
                _ => None,
            })
            .collect();
        for (pid, fd) in open {
            loop {
                // section[0] is the pointer field ahead of the section bytes.
                section[0] = 0;
                match self.dev.read_demux(fd, &mut section[1..]) {
                    Ok(0) => break,
                    Ok(read) => {
                        let Some(OpenPid::Section(_, cc)) = self.pids.get_mut(&pid) else {
                            break;
                        };
                        let len = ms_ts::buffer_to_ts(&mut packets, &section[..read + 1], cc, pid);
                        if len > 0 {
                            self.psi.extend_from_slice(&packets[..len as usize]);
                        }
                    }
                    Err(error) if error.raw_os_error() == Some(libc::EOVERFLOW) => {
                        ms_log::log!("socket {} error: EOVERFLOW", fd.0);
                        break;
                    }
                    Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
                    Err(_) => break,
                }
            }
        }
    }
}

impl<D: DvbDevice> Drop for LinuxDvbTuner<D> {
    /// Closes the per-PID demux fds; the stream fd belongs to the device.
    fn drop(&mut self) {
        for open in self.pids.values() {
            if let OpenPid::Tap(fd) | OpenPid::Section(fd, _) = *open {
                self.dev.close_demux(fd);
            }
        }
    }
}

impl<D: DvbDevice> Tuner for LinuxDvbTuner<D> {
    fn tune(&mut self, params: &TuneParams) -> Result<()> {
        let tp = DvbTransponder::from_params(params, self.state.tp.diseqc_param);
        let ret = self.state.tune(&mut self.dev, &tp);
        if ret < 0 {
            return Err(TunerError::Backend(ret));
        }
        self.status = SignalStatus {
            status_valid: false,
            ..SignalStatus::default()
        };
        // Sections of the previous transponder are not delivered after a retune.
        self.psi.clear();
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
            if let Some(open) = self.pids.remove(&pid) {
                self.close_pid(pid, open);
            }
        }
        for pid in pids.iter() {
            if self.pids.contains_key(&pid) {
                continue;
            }
            // dvb_set_psi_pes_filter: the kind is fixed when the PID opens.
            let psi = pid < 32 || self.section_pids.contains_exact(pid);
            let open = match self.mode {
                DemuxMode::Dvr => self.open_tap(pid)?,
                DemuxMode::Demux => self.open_shared(pid)?,
                DemuxMode::PesFiltersAndDvr | DemuxMode::PesFiltersAndDemux if psi => {
                    self.open_section(pid)?
                }
                DemuxMode::PesFiltersAndDvr => self.open_tap(pid)?,
                DemuxMode::PesFiltersAndDemux => self.open_shared(pid)?,
            };
            self.pids.insert(pid, open);
        }
        Ok(())
    }

    fn set_section_pids(&mut self, pids: &PidSet) {
        self.section_pids = pids.clone();
    }

    fn status(&self) -> SignalStatus {
        self.status
    }

    fn read(&mut self, buf: &mut TsBuf) -> Result<usize> {
        self.refresh_status();
        let mut appended = 0;
        if self.mode.pes_filters() {
            self.read_sections();
        }
        let mut taken = 0;
        while taken + TS_PACKET_SIZE <= self.psi.len()
            && buf.remaining_bytes() >= TS_PACKET_SIZE
            && buf.push_packet(&self.psi[taken..taken + TS_PACKET_SIZE])
        {
            taken += TS_PACKET_SIZE;
            appended += TS_PACKET_SIZE;
        }
        self.psi.drain(..taken);
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
