//! Typed view of the frontend/demux ioctls, so the backend logic runs against fakes.

use std::io;

/// `errno` and `strerror(errno)` of a failed syscall.
pub(crate) fn error_parts(error: &io::Error) -> (i32, String) {
    (ms_log::errno(error), ms_log::strerror(error))
}

/// `fe_sec_voltage_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Voltage {
    /// `SEC_VOLTAGE_13`.
    V13 = 0,
    /// `SEC_VOLTAGE_18`.
    V18 = 1,
    /// `SEC_VOLTAGE_OFF`.
    Off = 2,
}

/// `fe_sec_tone_mode_t`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// `SEC_TONE_ON` (22 kHz).
    On = 0,
    /// `SEC_TONE_OFF`.
    Off = 1,
}

/// `fe_sec_mini_cmd_t` (tone burst).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MiniCmd {
    /// `SEC_MINI_A`.
    A = 0,
    /// `SEC_MINI_B`.
    B = 1,
}

/// `struct dvb_diseqc_master_cmd`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiseqcMessage {
    /// Message bytes; only the first `len` are sent.
    pub msg: [u8; 6],
    /// `msg_len`.
    pub len: u8,
}

impl DiseqcMessage {
    /// The bytes that go on the wire.
    pub fn bytes(&self) -> &[u8] {
        &self.msg[..usize::from(self.len.min(6))]
    }
}

/// One `struct dtv_property` with a scalar `u.data`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DtvProperty {
    /// `DTV_*` command.
    pub cmd: u32,
    /// `u.data`.
    pub data: u32,
}

impl DtvProperty {
    /// Property from a C `int` value, stored bit-for-bit like `u.data = value`.
    pub fn new(cmd: u32, data: i32) -> Self {
        Self {
            cmd,
            data: data as u32,
        }
    }
}

/// First entry of a `struct dtv_fe_stats`, as `get_signal_new` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct DtvStat {
    /// `st.len`.
    pub len: u8,
    /// `st.stat[0].scale`.
    pub scale: u8,
    /// `st.stat[0].uvalue` (`svalue` is the same bits).
    pub value: u64,
}

impl DtvStat {
    /// `st.stat[0].svalue`.
    pub fn svalue(&self) -> i64 {
        self.value as i64
    }
}

/// Fields of `struct dvb_frontend_info` the backend uses.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FrontendInfo {
    /// `name`.
    pub name: String,
    /// `type` (`fe_type_t`).
    pub fe_type: u32,
    /// `caps` (`fe_caps_t`).
    pub caps: u32,
}

/// `dmx_output_t` values used for PES filters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PesOutput {
    /// `DMX_OUT_TS_TAP`: packets go to the dvr device.
    TsTap = 2,
    /// `DMX_OUT_TSDEMUX_TAP`: packets are read from the demux fd itself.
    TsDemuxTap = 3,
}

/// Opaque demux file handle owned by the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DemuxFd(pub u32);

/// Switch-control subset of the frontend: what a slave adapter drives on its master.
pub trait SecDevice {
    /// `FE_SET_TONE`.
    fn set_tone(&mut self, tone: Tone) -> io::Result<()>;
    /// `FE_SET_VOLTAGE`.
    fn set_voltage(&mut self, voltage: Voltage) -> io::Result<()>;
    /// `FE_DISEQC_SEND_MASTER_CMD`.
    fn send_master_cmd(&mut self, cmd: &DiseqcMessage) -> io::Result<()>;
    /// `FE_DISEQC_SEND_BURST`.
    fn send_burst(&mut self, burst: MiniCmd) -> io::Result<()>;
    /// `sleep_msec`; fakes record it instead of sleeping.
    fn sleep_ms(&mut self, ms: i32);
    /// `ad->wakeup` hook; Linux DVB adapters have none.
    fn wakeup(&mut self, _voltage: Voltage) {}
    /// Frontend fd used in log messages (`ad->fe` in C++).
    fn log_fd(&self) -> i32 {
        -1
    }
}

/// Frontend, demux and dvr operations of one Linux DVB adapter.
pub trait DvbDevice: SecDevice {
    /// Whether the frontend fd is open (`ad->fe > 0`).
    fn frontend_open(&self) -> bool;
    /// `FE_SET_PROPERTY`.
    fn set_properties(&mut self, props: &[DtvProperty]) -> io::Result<()>;
    /// `FE_GET_PROPERTY` for signal strength, CNR and error-block count, in that order.
    fn get_stats(&mut self) -> io::Result<[DtvStat; 3]>;
    /// `FE_GET_PROPERTY` with `DTV_ENUM_DELSYS`.
    fn enum_delsys(&mut self) -> io::Result<Vec<u32>>;
    /// `FE_GET_INFO`.
    fn get_info(&mut self) -> io::Result<FrontendInfo>;
    /// `FE_GET_EVENT`; `Err` once the queue is empty.
    fn get_event(&mut self) -> io::Result<()>;
    /// `FE_READ_STATUS`.
    fn read_status(&mut self) -> io::Result<i32>;
    /// `FE_READ_BER`.
    fn read_ber(&mut self) -> io::Result<u32>;
    /// `FE_READ_SIGNAL_STRENGTH`.
    fn read_signal_strength(&mut self) -> io::Result<u16>;
    /// `FE_READ_SNR`.
    fn read_snr(&mut self) -> io::Result<u16>;
    /// The stream fd opened with the frontend (dvr, or demux in `USE_DEMUX` mode).
    fn stream_fd(&self) -> DemuxFd;
    /// Opens a new `demuxN` fd (`O_RDWR | O_NONBLOCK`).
    fn open_demux(&mut self) -> io::Result<DemuxFd>;
    /// Closes a fd returned by `open_demux`.
    fn close_demux(&mut self, fd: DemuxFd);
    /// `DMX_SET_PES_FILTER` with `DMX_IN_FRONTEND`, `DMX_PES_OTHER`, `DMX_IMMEDIATE_START`.
    fn set_pes_filter(&mut self, fd: DemuxFd, pid: u16, output: PesOutput) -> io::Result<()>;
    /// `DMX_SET_FILTER` for every section on `pid`, no timeout, `DMX_IMMEDIATE_START`.
    fn set_section_filter(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()>;
    /// Reads one section from a section-filter fd.
    fn read_demux(&mut self, fd: DemuxFd, buf: &mut [u8]) -> io::Result<usize>;
    /// `DMX_ADD_PID`.
    fn add_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()>;
    /// `DMX_REMOVE_PID`.
    fn remove_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()>;
    /// `DMX_STOP`.
    fn demux_stop(&mut self, fd: DemuxFd) -> io::Result<()>;
    /// `DMX_SET_BUFFER_SIZE`.
    fn set_buffer_size(&mut self, fd: DemuxFd, size: i32) -> io::Result<()>;
    /// `DMX_SET_SOURCE` on an open demux fd.
    fn set_demux_source(&mut self, fd: DemuxFd, source: i32) -> io::Result<()>;
    /// Writes `val` to `/proc/stb/frontend/<frontend>/<name>` (`set_proc_data`).
    fn set_proc_data(&mut self, frontend: i32, name: &str, val: i32) -> io::Result<()>;
    /// Reads TS bytes from the stream fd.
    fn read_stream(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    /// Monotonic milliseconds (`getTick`).
    fn now_ms(&self) -> i64;
}
