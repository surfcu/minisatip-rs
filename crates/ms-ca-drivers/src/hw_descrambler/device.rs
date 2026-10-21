//! Typed view of the Enigma2 CA descrambler ioctls, so the slot logic runs against fakes.

use std::io;

use ms_tuner::linux_dvb::IocLayout;

/// Open CA device handle owned by a [`CaDevice`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct CaFd(pub i32);

/// `struct ca_descr_info`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaDescrInfo {
    /// Number of hardware descrambler slots (keys).
    pub num: u32,
    /// Supported scrambling systems (`CA_ECD`, `CA_NDS`, `CA_DSS`).
    pub ty: u32,
}

/// `struct ca_descr_mode` (`CA_SET_DESCR_MODE`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaDescrMode {
    /// Descrambler slot.
    pub index: u32,
    /// `CA_ALGO_*` value.
    pub algo: u32,
    /// 0 = ECB, 1 = CBC.
    pub cipher_mode: u32,
}

/// `struct ca_pid` (`CA_SET_PID`); `index` -1 unbinds the PID.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaPid {
    /// Elementary PID.
    pub pid: u32,
    /// Descrambler slot, or -1.
    pub index: i32,
}

/// `struct ca_descr` (`CA_SET_DESCR`): an 8-byte CSA control word.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaDescr {
    /// Descrambler slot.
    pub index: u32,
    /// 0 = even, 1 = odd.
    pub parity: u32,
    /// Control word.
    pub cw: [u8; 8],
}

/// `ca_descr_data::data_type` of a key.
pub const DATA_TYPE_CW: u32 = 0;
/// `ca_descr_data::data_type` of an IV.
pub const DATA_TYPE_IV: u32 = 1;

/// `struct ca_descr_data` (`CA_SET_DESCR_DATA`): a 16-byte AES key or IV.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct CaDescrData {
    /// Descrambler slot.
    pub index: u32,
    /// 0 = even, 1 = odd.
    pub parity: u32,
    /// [`DATA_TYPE_CW`] or [`DATA_TYPE_IV`].
    pub data_type: u32,
    /// Bytes used in `data`.
    pub data_len: u32,
    /// Key or IV bytes.
    pub data: [u8; 16],
}

/// CA device operations used by the hardware descrambler.
pub trait CaDevice {
    /// `open(path, O_RDWR | O_CLOEXEC)`; `NotFound` maps `ENOENT`.
    fn open(&mut self, path: &str) -> io::Result<CaFd>;
    /// `close`.
    fn close(&mut self, fd: CaFd);
    /// `CA_GET_DESCR_INFO`.
    fn get_descr_info(&mut self, fd: CaFd) -> io::Result<CaDescrInfo>;
    /// `CA_SET_DESCR_MODE`.
    fn set_descr_mode(&mut self, fd: CaFd, mode: &CaDescrMode) -> io::Result<()>;
    /// `CA_SET_PID`.
    fn set_pid(&mut self, fd: CaFd, pid: &CaPid) -> io::Result<()>;
    /// `CA_SET_DESCR`.
    fn set_descr(&mut self, fd: CaFd, descr: &CaDescr) -> io::Result<()>;
    /// `CA_SET_DESCR_DATA`.
    fn set_descr_data(&mut self, fd: CaFd, data: &CaDescrData) -> io::Result<()>;
}

/// `sizeof(struct ca_descr_info)`.
pub const CA_DESCR_INFO_SIZE: usize = 8;
/// `sizeof(struct ca_descr)`.
pub const CA_DESCR_SIZE: usize = 16;
/// `sizeof(struct ca_pid)`.
pub const CA_PID_SIZE: usize = 8;
/// `sizeof(struct ca_descr_mode)`.
pub const CA_DESCR_MODE_SIZE: usize = 12;
/// `sizeof(struct ca_descr_data)`.
pub const CA_DESCR_DATA_SIZE: usize = 32;

/// CA request numbers; `CA_GET_DESCR_INFO` is the kernel's (131), the rest hw_descrambler.h's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaIoctls {
    pub get_descr_info: u32,
    pub set_descr: u32,
    pub set_pid: u32,
    pub set_descr_mode: u32,
    pub set_descr_data: u32,
}

impl CaIoctls {
    /// Request numbers for `layout`.
    pub const fn new(layout: IocLayout) -> Self {
        let o = b'o';
        Self {
            get_descr_info: layout.ior(o, 131, CA_DESCR_INFO_SIZE),
            set_descr: layout.iow(o, 134, CA_DESCR_SIZE),
            set_pid: layout.iow(o, 135, CA_PID_SIZE),
            set_descr_mode: layout.iow(o, 136, CA_DESCR_MODE_SIZE),
            set_descr_data: layout.iow(o, 137, CA_DESCR_DATA_SIZE),
        }
    }
}

/// Request numbers of the compilation target.
pub const NATIVE_CA_IOCTLS: CaIoctls = CaIoctls::new(ms_tuner::linux_dvb::NATIVE);

#[cfg(test)]
mod tests {
    use super::*;
    use ms_tuner::linux_dvb::{GENERIC, MIPS};

    // Golden values printed by a C program built against x86_64 linux/dvb/ca.h plus the
    // hw_descrambler.h fallback definitions.
    #[test]
    fn generic_matches_kernel_headers() {
        let io = CaIoctls::new(GENERIC);
        assert_eq!(io.get_descr_info, 0x8008_6f83);
        assert_eq!(io.set_descr, 0x4010_6f86);
        assert_eq!(io.set_pid, 0x4008_6f87);
        assert_eq!(io.set_descr_mode, 0x400c_6f88);
        assert_eq!(io.set_descr_data, 0x4020_6f89);
    }

    // mips (Enigma2 boxes): _IOC_READ=2, _IOC_WRITE=4 at bit 29.
    #[test]
    fn mips_encoding() {
        let io = CaIoctls::new(MIPS);
        assert_eq!(io.get_descr_info, 0x4008_6f83);
        assert_eq!(io.set_descr, 0x8010_6f86);
        assert_eq!(io.set_pid, 0x8008_6f87);
        assert_eq!(io.set_descr_mode, 0x800c_6f88);
        assert_eq!(io.set_descr_data, 0x8020_6f89);
    }
}
