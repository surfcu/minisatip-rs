//! Linux CA device (`/dev/dvb/adapterN/caM`): open, read/write and the slot ioctls.
#![allow(unsafe_code)]

use super::link::{CaLink, CaOpener, SlotInfo};
use std::ffi::CString;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};

/// `_IO('o', 128)`.
const CA_RESET: u32 = 0x6F80;
/// `_IOR('o', 130, ca_slot_info_t)`.
const CA_GET_SLOT_INFO: u32 = 0x800C_6F82;

/// `ca_slot_info_t`.
#[repr(C)]
#[derive(Default)]
struct CaSlotInfo {
    num: libc::c_int,
    kind: libc::c_int,
    flags: libc::c_uint,
}

/// An open CA device, non-blocking so reads end with `WouldBlock`.
#[derive(Debug)]
pub struct DvbCaLink {
    fd: OwnedFd,
}

impl DvbCaLink {
    pub fn open(path: &str) -> io::Result<Self> {
        let c = CString::new(path).map_err(|_| io::Error::from(io::ErrorKind::InvalidInput))?;
        // SAFETY: open(2) gets a NUL-terminated path; the result is checked below.
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: fd is a freshly opened descriptor owned by nobody else.
        Ok(Self {
            fd: unsafe { OwnedFd::from_raw_fd(fd) },
        })
    }

    /// Waits up to `timeout_ms` for input; returns `(readable, priority)` like POLLIN/POLLPRI.
    pub fn poll(&self, timeout_ms: i32) -> io::Result<(bool, bool)> {
        let mut p = libc::pollfd {
            fd: self.fd.as_raw_fd(),
            events: libc::POLLIN | libc::POLLPRI | libc::POLLERR,
            revents: 0,
        };
        // SAFETY: poll(2) reads and writes exactly one valid pollfd.
        let n = unsafe { libc::poll(&mut p, 1, timeout_ms) };
        if n < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok((
            p.revents & (libc::POLLIN | libc::POLLERR) != 0,
            p.revents & libc::POLLPRI != 0,
        ))
    }
}

impl AsRawFd for DvbCaLink {
    fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

fn check(rc: isize) -> io::Result<usize> {
    match rc {
        r if r < 0 => Err(io::Error::last_os_error()),
        r => Ok(r as usize),
    }
}

impl CaLink for DvbCaLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        // SAFETY: read(2) writes at most buf.len() bytes into the slice.
        check(unsafe { libc::read(self.fd.as_raw_fd(), buf.as_mut_ptr().cast(), buf.len()) })
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        loop {
            // SAFETY: write(2) reads buf.len() bytes from the slice.
            let rc = unsafe { libc::write(self.fd.as_raw_fd(), buf.as_ptr().cast(), buf.len()) };
            match check(rc) {
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                r => return r,
            }
        }
    }

    fn reset(&mut self) -> io::Result<()> {
        let mut info = CaSlotInfo::default();
        // SAFETY: CA_RESET takes no argument; the zeroed struct pointer mirrors ca_slot_reset.
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), CA_RESET as _, &mut info) };
        check(rc as isize).map(|_| ())
    }

    fn slot_info(&mut self) -> io::Result<SlotInfo> {
        let mut info = CaSlotInfo::default();
        // SAFETY: CA_GET_SLOT_INFO fills the ca_slot_info_t it is given (slot 0).
        let rc = unsafe { libc::ioctl(self.fd.as_raw_fd(), CA_GET_SLOT_INFO as _, &mut info) };
        check(rc as isize)?;
        Ok(SlotInfo {
            kind: info.kind as u32,
            flags: info.flags,
        })
    }
}

/// Opens real CA devices.
#[derive(Debug, Default, Clone, Copy)]
pub struct DvbCaOpener;

impl CaOpener for DvbCaOpener {
    fn open(&mut self, path: &str) -> io::Result<Box<dyn CaLink>> {
        Ok(Box::new(DvbCaLink::open(path)?))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ioctl_numbers_match_linux_dvb_ca_h() {
        assert_eq!(std::mem::size_of::<CaSlotInfo>(), 12);
        assert_eq!(CA_GET_SLOT_INFO >> 16 & 0x3FFF, 12);
        assert_eq!(CA_RESET, (u32::from(b'o') << 8) | 128);
    }

    #[test]
    fn missing_device_fails_to_open() {
        assert!(DvbCaLink::open("/dev/dvb/adapter99/ca0").is_err());
    }
}
