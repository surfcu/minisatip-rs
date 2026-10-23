//! All Linux DVB system calls and kernel ABI structures live here.

use std::ffi::CString;
use std::io;
use std::time::Instant;

use super::consts::*;
use super::device::{
    DemuxFd, DiseqcMessage, DtvProperty, DtvStat, DvbDevice, FrontendInfo, MiniCmd, PesOutput,
    SecDevice, Tone, Voltage, error_parts,
};
use super::enumerate::{DeviceTree, FrontendProbe, ProbedFrontend, probe_device};
use super::ioc::NATIVE_IOCTLS;

const DEV_FRONTEND: &str = "/dev/dvb/adapter{}/frontend{}";
const DEV_DEMUX: &str = "/dev/dvb/adapter{}/demux{}";
const DEV_DVR: &str = "/dev/dvb/adapter{}/dvr{}";

const DMX_IN_FRONTEND: i32 = 0;
const DMX_PES_OTHER: i32 = 20;
const DMX_IMMEDIATE_START: u32 = 4;

/// `sizeof(struct dtv_property)` union payload on this pointer width.
const DTV_PROPERTY_UNION: usize = {
    // Packed dtv_fe_stats (len + scale + u64) and the buffer variant.
    let stats = 1 + 1 + 8;
    let buffer = 32 + 4 + 12 + size_of::<usize>();
    if stats > buffer { stats } else { buffer }
};

/// `struct dvb_frontend_info` (168 bytes).
#[repr(C)]
struct FrontendInfoRaw {
    name: [u8; 128],
    fe_type: u32,
    frequency_min: u32,
    frequency_max: u32,
    frequency_stepsize: u32,
    frequency_tolerance: u32,
    symbol_rate_min: u32,
    symbol_rate_max: u32,
    symbol_rate_tolerance: u32,
    notifier_delay: u32,
    caps: u32,
}

impl Default for FrontendInfoRaw {
    fn default() -> Self {
        // SAFETY: all fields are plain integers and byte arrays.
        unsafe { std::mem::zeroed() }
    }
}

/// `struct dvb_diseqc_master_cmd` (7 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct DiseqcMasterCmdRaw {
    msg: [u8; 6],
    msg_len: u8,
}

/// `struct dtv_property`, packed like the kernel header (76 bytes on 64-bit).
#[repr(C, packed)]
#[derive(Clone, Copy)]
struct DtvPropertyRaw {
    cmd: u32,
    reserved: [u32; 3],
    u: [u8; DTV_PROPERTY_UNION],
    result: i32,
}

impl DtvPropertyRaw {
    fn new(cmd: u32, data: u32) -> Self {
        let mut property = Self {
            cmd,
            reserved: [0; 3],
            u: [0; DTV_PROPERTY_UNION],
            result: 0,
        };
        property.u[..4].copy_from_slice(&data.to_ne_bytes());
        property
    }

    fn stat_len(&self) -> u8 {
        self.u[0]
    }

    fn stat_scale(&self) -> u8 {
        self.u[1]
    }

    fn stat_value(&self) -> u64 {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&self.u[2..10]);
        u64::from_ne_bytes(bytes)
    }

    fn buffer_len(&self) -> u32 {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&self.u[32..36]);
        u32::from_ne_bytes(bytes)
    }

    /// One entry of `u.buffer.data[32]`: a `__u8` per delivery system (dvb.cpp `dvb_delsys`).
    fn buffer_data(&self, index: usize) -> u32 {
        u32::from(self.u[index])
    }

    /// `DTV_ENUM_DELSYS` result: `u.buffer.len` byte-sized entries of `u.buffer.data[32]`.
    fn delsys_list(&self) -> Vec<u32> {
        let len = (self.buffer_len() as usize).min(32);
        (0..len).map(|i| self.buffer_data(i)).collect()
    }
}

/// `struct dtv_properties` (pointer-sized).
#[repr(C)]
struct DtvPropertiesRaw {
    num: u32,
    props: *mut DtvPropertyRaw,
}

/// `struct dvb_frontend_event` (40 bytes).
#[repr(C)]
#[derive(Clone, Copy)]
struct FrontendEventRaw {
    status: i32,
    parameters: [u8; 36],
}

/// `struct dmx_sct_filter_params` (60 bytes).
#[repr(C)]
struct SctFilterRaw {
    pid: u16,
    filter: [u8; 16],
    mask: [u8; 16],
    mode: [u8; 16],
    timeout: u32,
    flags: u32,
}

/// `struct dmx_pes_filter_params` (20 bytes).
#[repr(C)]
struct PesFilterRaw {
    pid: u16,
    input: i32,
    output: i32,
    pes_type: i32,
    flags: u32,
}

fn path(template: &str, adapter: u32, frontend: u32) -> String {
    let path = template.replacen("{}", &adapter.to_string(), 1);
    path.replacen("{}", &frontend.to_string(), 1)
}

fn open_path(path: &str, flags: i32) -> io::Result<i32> {
    let c_path = CString::new(path).map_err(io::Error::other)?;
    // SAFETY: c_path is a valid NUL-terminated string and flags are valid open(2) flags.
    let fd = unsafe { libc::open(c_path.as_ptr(), flags) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(fd)
    }
}

fn accessible(path: &str, mode: libc::c_int) -> bool {
    let Ok(c_path) = CString::new(path) else {
        return false;
    };
    // SAFETY: c_path is a valid NUL-terminated string; access(2) only reads it.
    unsafe { libc::access(c_path.as_ptr(), mode) == 0 }
}

/// Real `/dev/dvb` tree used by `find_dvb_adapter`.
pub struct LinuxDeviceTree;

impl DeviceTree for LinuxDeviceTree {
    fn readable(&self, path: &str) -> bool {
        accessible(path, libc::R_OK)
    }

    fn writable(&self, path: &str) -> bool {
        accessible(path, libc::W_OK)
    }
}

/// Opens each discovered frontend to read `FE_GET_INFO` and `DTV_ENUM_DELSYS`.
pub struct LinuxFrontendProbe;

impl FrontendProbe for LinuxFrontendProbe {
    fn probe(&self, adapter: u32, frontend: u32) -> Option<ProbedFrontend> {
        self.probe_with_id(adapter, frontend, adapter as i32)
    }

    fn probe_with_id(&self, adapter: u32, frontend: u32, aid: i32) -> Option<ProbedFrontend> {
        let mut dev = LinuxDvbDevice::open(adapter, frontend).ok()?;
        Some(probe_device(&mut dev, aid))
    }
}

fn close_fd(fd: i32) {
    if fd >= 0 {
        // SAFETY: fd is an open file descriptor owned by this device.
        unsafe { libc::close(fd) };
    }
}

fn ioctl_ptr<T>(fd: i32, request: u32, arg: &mut T) -> io::Result<()> {
    // SAFETY: the caller passes an open fd whose request expects a pointer to T, and arg is a
    // valid, correctly aligned &mut T that lives for the call.
    let result = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg as *mut T) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn ioctl_arg(fd: i32, request: u32, arg: libc::c_ulong) -> io::Result<()> {
    // SAFETY: the request takes its argument by value; fd is an open DVB fd.
    let result = unsafe { libc::ioctl(fd, request as libc::Ioctl, arg) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// Real `/dev/dvb/adapterN` frontend, demux and dvr devices.
pub struct LinuxDvbDevice {
    fe: i32,
    dvr: i32,
    adapter: u32,
    frontend: u32,
    base: Instant,
}

impl LinuxDvbDevice {
    /// Opens `frontendN` and `dvrN` for one adapter (`dvb_open_device`, dvb.cpp:894).
    pub fn open(adapter: u32, frontend: u32) -> io::Result<Self> {
        Self::open_stream(adapter, frontend, false)
    }

    /// Like [`Self::open`], but `use_demux` reads the stream from `demuxN`
    /// opened read-write (`USE_DEMUX`, `USE_PES_FILTERS_AND_DEMUX`).
    pub fn open_stream(adapter: u32, frontend: u32, use_demux: bool) -> io::Result<Self> {
        let fe_path = path(DEV_FRONTEND, adapter, frontend);
        let (dvr_path, flags) = if use_demux {
            (
                path(DEV_DEMUX, adapter, frontend),
                libc::O_RDWR | libc::O_NONBLOCK,
            )
        } else {
            (
                path(DEV_DVR, adapter, frontend),
                libc::O_RDONLY | libc::O_NONBLOCK,
            )
        };
        let fe = match open_path(&fe_path, libc::O_RDWR | libc::O_NONBLOCK) {
            Ok(fd) => fd,
            Err(error) => {
                let (errno, text) = error_parts(&error);
                ms_log::log!(
                    "Could not open {} in RW mode (fe: {}, dvr: {}) error {}: {}",
                    fe_path,
                    -1,
                    -1,
                    errno,
                    text
                );
                return Err(error);
            }
        };
        let dvr = match open_path(&dvr_path, flags) {
            Ok(fd) => fd,
            Err(error) => {
                let (errno, text) = error_parts(&error);
                ms_log::log!(
                    "Could not open {} in RW mode (fe: {}, dvr: {}) error {}: {}",
                    fe_path,
                    fe,
                    -1,
                    errno,
                    text
                );
                close_fd(fe);
                return Err(error);
            }
        };
        Ok(Self {
            fe,
            dvr,
            adapter,
            frontend,
            base: Instant::now(),
        })
    }

    /// Frontend fd, or -1 when closed.
    pub fn frontend_fd(&self) -> i32 {
        self.fe
    }

    /// DVR fd.
    pub fn dvr_fd(&self) -> i32 {
        self.dvr
    }

    /// Adapter number.
    pub fn adapter(&self) -> u32 {
        self.adapter
    }

    /// Frontend number.
    pub fn frontend(&self) -> u32 {
        self.frontend
    }

    fn set_enum(&self, fd: i32, request: u32, value: i32) -> io::Result<()> {
        ioctl_arg(fd, request, value as libc::c_ulong)
    }
}

impl Drop for LinuxDvbDevice {
    fn drop(&mut self) {
        close_fd(self.fe);
        close_fd(self.dvr);
        self.fe = -1;
        self.dvr = -1;
    }
}

impl SecDevice for LinuxDvbDevice {
    fn set_tone(&mut self, tone: Tone) -> io::Result<()> {
        self.set_enum(self.fe, NATIVE_IOCTLS.fe_set_tone, tone as i32)
    }

    fn set_voltage(&mut self, voltage: Voltage) -> io::Result<()> {
        self.set_enum(self.fe, NATIVE_IOCTLS.fe_set_voltage, voltage as i32)
    }

    fn send_master_cmd(&mut self, cmd: &DiseqcMessage) -> io::Result<()> {
        let bytes = cmd.bytes();
        let mut raw = DiseqcMasterCmdRaw {
            msg: [0; 6],
            msg_len: bytes.len() as u8,
        };
        raw.msg[..bytes.len()].copy_from_slice(bytes);
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_diseqc_send_master_cmd, &mut raw)
    }

    fn send_burst(&mut self, burst: MiniCmd) -> io::Result<()> {
        self.set_enum(self.fe, NATIVE_IOCTLS.fe_diseqc_send_burst, burst as i32)
    }

    fn sleep_ms(&mut self, ms: i32) {
        if ms > 0 {
            std::thread::sleep(std::time::Duration::from_millis(ms as u64));
        }
    }

    fn log_fd(&self) -> i32 {
        self.fe
    }
}

impl DvbDevice for LinuxDvbDevice {
    fn frontend_open(&self) -> bool {
        self.fe > 0
    }

    fn set_properties(&mut self, props: &[DtvProperty]) -> io::Result<()> {
        let mut raw: Vec<DtvPropertyRaw> = props
            .iter()
            .map(|p| DtvPropertyRaw::new(p.cmd, p.data))
            .collect();
        let mut sequence = DtvPropertiesRaw {
            num: raw.len() as u32,
            props: raw.as_mut_ptr(),
        };
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_set_property, &mut sequence)
    }

    fn get_stats(&mut self) -> io::Result<[DtvStat; 3]> {
        let mut raw = [
            DtvPropertyRaw::new(DTV_STAT_SIGNAL_STRENGTH, 0),
            DtvPropertyRaw::new(DTV_STAT_CNR, 0),
            DtvPropertyRaw::new(DTV_STAT_ERROR_BLOCK_COUNT, 0),
        ];
        let mut sequence = DtvPropertiesRaw {
            num: raw.len() as u32,
            props: raw.as_mut_ptr(),
        };
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_get_property, &mut sequence)?;
        Ok(raw.map(|property| DtvStat {
            len: property.stat_len(),
            scale: property.stat_scale(),
            value: property.stat_value(),
        }))
    }

    fn enum_delsys(&mut self) -> io::Result<Vec<u32>> {
        let mut raw = [DtvPropertyRaw::new(DTV_ENUM_DELSYS, 0)];
        let mut sequence = DtvPropertiesRaw {
            num: raw.len() as u32,
            props: raw.as_mut_ptr(),
        };
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_get_property, &mut sequence)?;
        Ok(raw[0].delsys_list())
    }

    fn get_info(&mut self) -> io::Result<FrontendInfo> {
        let mut raw = FrontendInfoRaw::default();
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_get_info, &mut raw)?;
        let end = raw.name.iter().position(|&b| b == 0).unwrap_or(128);
        Ok(FrontendInfo {
            name: String::from_utf8_lossy(&raw.name[..end]).into_owned(),
            fe_type: raw.fe_type,
            caps: raw.caps,
        })
    }

    fn get_event(&mut self) -> io::Result<()> {
        let mut event = FrontendEventRaw {
            status: 0,
            parameters: [0; 36],
        };
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_get_event, &mut event)
    }

    fn read_status(&mut self) -> io::Result<i32> {
        let mut status = 0i32;
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_read_status, &mut status)?;
        Ok(status)
    }

    fn read_ber(&mut self) -> io::Result<u32> {
        let mut ber = 0u32;
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_read_ber, &mut ber)?;
        Ok(ber)
    }

    fn read_signal_strength(&mut self) -> io::Result<u16> {
        let mut strength = 0u16;
        ioctl_ptr(
            self.fe,
            NATIVE_IOCTLS.fe_read_signal_strength,
            &mut strength,
        )?;
        Ok(strength)
    }

    fn read_snr(&mut self) -> io::Result<u16> {
        let mut snr = 0u16;
        ioctl_ptr(self.fe, NATIVE_IOCTLS.fe_read_snr, &mut snr)?;
        Ok(snr)
    }

    fn stream_fd(&self) -> DemuxFd {
        DemuxFd(self.dvr as u32)
    }

    fn open_demux(&mut self) -> io::Result<DemuxFd> {
        let demux_path = path(DEV_DEMUX, self.adapter, self.frontend);
        open_path(&demux_path, libc::O_RDWR | libc::O_NONBLOCK).map(|fd| DemuxFd(fd as u32))
    }

    fn close_demux(&mut self, fd: DemuxFd) {
        close_fd(fd.0 as i32);
    }

    fn set_pes_filter(&mut self, fd: DemuxFd, pid: u16, output: PesOutput) -> io::Result<()> {
        let mut filter = PesFilterRaw {
            pid,
            input: DMX_IN_FRONTEND,
            output: output as i32,
            pes_type: DMX_PES_OTHER,
            flags: DMX_IMMEDIATE_START,
        };
        ioctl_ptr(fd.0 as i32, NATIVE_IOCTLS.dmx_set_pes_filter, &mut filter)
    }

    fn set_section_filter(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()> {
        let mut filter = SctFilterRaw {
            pid,
            filter: [0; 16],
            mask: [0; 16],
            mode: [0; 16],
            timeout: 0,
            flags: DMX_IMMEDIATE_START,
        };
        ioctl_ptr(fd.0 as i32, NATIVE_IOCTLS.dmx_set_filter, &mut filter)
    }

    fn read_demux(&mut self, fd: DemuxFd, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: buf is a valid writable slice and fd is an open demux fd.
        let read = unsafe { libc::read(fd.0 as i32, buf.as_mut_ptr().cast(), buf.len()) };
        if read < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(read as usize)
        }
    }

    fn add_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()> {
        let mut value = pid;
        ioctl_ptr(fd.0 as i32, NATIVE_IOCTLS.dmx_add_pid, &mut value)
    }

    fn remove_pid(&mut self, fd: DemuxFd, pid: u16) -> io::Result<()> {
        let mut value = pid;
        ioctl_ptr(fd.0 as i32, NATIVE_IOCTLS.dmx_remove_pid, &mut value)
    }

    fn demux_stop(&mut self, fd: DemuxFd) -> io::Result<()> {
        ioctl_arg(fd.0 as i32, NATIVE_IOCTLS.dmx_stop, 0)
    }

    fn set_buffer_size(&mut self, fd: DemuxFd, size: i32) -> io::Result<()> {
        ioctl_arg(
            fd.0 as i32,
            NATIVE_IOCTLS.dmx_set_buffer_size,
            size as libc::c_ulong,
        )
    }

    fn set_demux_source(&mut self, fd: DemuxFd, source: i32) -> io::Result<()> {
        let mut value = source;
        ioctl_ptr(fd.0 as i32, NATIVE_IOCTLS.dmx_set_source, &mut value)
    }

    fn set_proc_data(&mut self, frontend: i32, name: &str, val: i32) -> io::Result<()> {
        std::fs::write(
            format!("/proc/stb/frontend/{frontend}/{name}"),
            val.to_string(),
        )
    }

    fn read_stream(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: buf is a valid writable slice and dvr is an open file descriptor.
        let read = unsafe { libc::read(self.dvr, buf.as_mut_ptr().cast(), buf.len()) };
        if read < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(read as usize)
        }
    }

    fn now_ms(&self) -> i64 {
        self.base.elapsed().as_millis() as i64
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_abi_sizes_match_linux_headers() {
        assert_eq!(size_of::<FrontendInfoRaw>(), 168);
        assert_eq!(size_of::<DiseqcMasterCmdRaw>(), 7);
        assert_eq!(size_of::<FrontendEventRaw>(), 40);
        assert_eq!(size_of::<PesFilterRaw>(), 20);
        assert_eq!(size_of::<SctFilterRaw>(), 60);
        assert_eq!(size_of::<DtvPropertiesRaw>(), 2 * size_of::<usize>());
        assert_eq!(
            size_of::<DtvPropertyRaw>(),
            if size_of::<usize>() == 4 { 72 } else { 76 }
        );
    }

    // DTV_ENUM_DELSYS fills u.buffer.data[] with one byte per system (dvb.cpp dvb_delsys).
    #[test]
    fn enum_delsys_reads_one_byte_per_system() {
        let mut raw = DtvPropertyRaw::new(DTV_ENUM_DELSYS, 0);
        raw.u[..3].copy_from_slice(&[5, 6, 7]); // SYS_DVBS, SYS_DVBS2, SYS_DSS
        raw.u[32..36].copy_from_slice(&3u32.to_ne_bytes());
        assert_eq!(raw.delsys_list(), vec![5, 6, 7]);
        let systems: Vec<_> = raw
            .delsys_list()
            .into_iter()
            .filter_map(crate::linux_dvb::enumerate::delivery_system_from_number)
            .collect();
        assert!(systems.contains(&crate::DeliverySystem::DvbS));
        assert!(systems.contains(&crate::DeliverySystem::DvbS2));
    }

    #[test]
    fn enum_delsys_len_is_capped_at_the_buffer() {
        let mut raw = DtvPropertyRaw::new(DTV_ENUM_DELSYS, 0);
        raw.u[32..36].copy_from_slice(&200u32.to_ne_bytes());
        assert_eq!(raw.delsys_list().len(), 32);
    }

    #[test]
    fn device_paths_use_both_numbers() {
        assert_eq!(path(DEV_FRONTEND, 1, 2), "/dev/dvb/adapter1/frontend2");
        assert_eq!(path(DEV_DEMUX, 0, 1), "/dev/dvb/adapter0/demux1");
        assert_eq!(path(DEV_DVR, 3, 0), "/dev/dvb/adapter3/dvr0");
    }

    #[test]
    fn property_buffer_helpers_use_the_kernel_offsets() {
        let mut property = DtvPropertyRaw::new(DTV_ENUM_DELSYS, 0);
        assert_eq!(property.stat_len(), 0);
        // u.buffer: __u8 data[32] then __u32 len at offset 32 (linux/dvb/frontend.h).
        property.u[32..36].copy_from_slice(&2u32.to_ne_bytes());
        property.u[0] = 3;
        property.u[1] = 5;
        assert_eq!(property.buffer_len(), 2);
        assert_eq!(property.buffer_data(0), 3);
        assert_eq!(property.buffer_data(1), 5);
    }
}
