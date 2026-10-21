//! Real `/dev/dvb/adapterN/caM` ioctls and the kernel ABI structures.

use std::ffi::CString;
use std::io;

use super::device::{
    CaDescr, CaDescrData, CaDescrInfo, CaDescrMode, CaDevice, CaFd, CaPid, NATIVE_CA_IOCTLS,
};

#[repr(C)]
#[derive(Default)]
struct CaDescrInfoRaw {
    num: u32,
    ty: u32,
}

#[repr(C)]
struct CaDescrModeRaw {
    index: u32,
    algo: u32,
    cipher_mode: u32,
}

#[repr(C)]
struct CaPidRaw {
    pid: u32,
    index: i32,
}

#[repr(C)]
struct CaDescrRaw {
    index: u32,
    parity: u32,
    cw: [u8; 8],
}

#[repr(C)]
struct CaDescrDataRaw {
    index: u32,
    parity: u32,
    data_type: u32,
    data_len: u32,
    data: [u8; 16],
}

fn ioctl_ptr<T>(fd: CaFd, request: u32, arg: &mut T) -> io::Result<()> {
    // SAFETY: fd is an open CA fd whose request expects a pointer to T, and arg is a valid,
    // aligned &mut T that outlives the call.
    let result = unsafe { libc::ioctl(fd.0, request as libc::Ioctl, arg as *mut T) };
    if result < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// The host's CA devices.
#[derive(Debug, Default)]
pub struct LinuxCaDevice;

impl CaDevice for LinuxCaDevice {
    fn open(&mut self, path: &str) -> io::Result<CaFd> {
        let c_path = CString::new(path).map_err(io::Error::other)?;
        // SAFETY: c_path is a valid NUL-terminated string and the flags are valid open(2) flags.
        let fd = unsafe { libc::open(c_path.as_ptr(), libc::O_RDWR | libc::O_CLOEXEC) };
        if fd < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(CaFd(fd))
        }
    }

    fn close(&mut self, fd: CaFd) {
        // SAFETY: fd was returned by open and is closed once by the slot manager.
        unsafe { libc::close(fd.0) };
    }

    fn get_descr_info(&mut self, fd: CaFd) -> io::Result<CaDescrInfo> {
        let mut raw = CaDescrInfoRaw::default();
        ioctl_ptr(fd, NATIVE_CA_IOCTLS.get_descr_info, &mut raw)?;
        Ok(CaDescrInfo {
            num: raw.num,
            ty: raw.ty,
        })
    }

    fn set_descr_mode(&mut self, fd: CaFd, mode: &CaDescrMode) -> io::Result<()> {
        let mut raw = CaDescrModeRaw {
            index: mode.index,
            algo: mode.algo,
            cipher_mode: mode.cipher_mode,
        };
        ioctl_ptr(fd, NATIVE_CA_IOCTLS.set_descr_mode, &mut raw)
    }

    fn set_pid(&mut self, fd: CaFd, pid: &CaPid) -> io::Result<()> {
        let mut raw = CaPidRaw {
            pid: pid.pid,
            index: pid.index,
        };
        ioctl_ptr(fd, NATIVE_CA_IOCTLS.set_pid, &mut raw)
    }

    fn set_descr(&mut self, fd: CaFd, descr: &CaDescr) -> io::Result<()> {
        let mut raw = CaDescrRaw {
            index: descr.index,
            parity: descr.parity,
            cw: descr.cw,
        };
        ioctl_ptr(fd, NATIVE_CA_IOCTLS.set_descr, &mut raw)
    }

    fn set_descr_data(&mut self, fd: CaFd, data: &CaDescrData) -> io::Result<()> {
        let mut raw = CaDescrDataRaw {
            index: data.index,
            parity: data.parity,
            data_type: data.data_type,
            data_len: data.data_len,
            data: data.data,
        };
        ioctl_ptr(fd, NATIVE_CA_IOCTLS.set_descr_data, &mut raw)
    }
}

#[cfg(test)]
mod tests {
    use super::super::device::*;
    use super::*;

    #[test]
    fn abi_sizes_match_the_ioctl_encoding() {
        assert_eq!(size_of::<CaDescrInfoRaw>(), CA_DESCR_INFO_SIZE);
        assert_eq!(size_of::<CaDescrRaw>(), CA_DESCR_SIZE);
        assert_eq!(size_of::<CaPidRaw>(), CA_PID_SIZE);
        assert_eq!(size_of::<CaDescrModeRaw>(), CA_DESCR_MODE_SIZE);
        assert_eq!(size_of::<CaDescrDataRaw>(), CA_DESCR_DATA_SIZE);
    }

    #[test]
    fn missing_node_reports_not_found() {
        let err = LinuxCaDevice
            .open("/dev/dvb/adapter99/ca99-does-not-exist")
            .unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::NotFound);
    }
}
