//! `_IOC` request-number encoding for each Linux architecture family.

/// Bit layout of `_IOC` (`asm-generic/ioctl.h` and the mips/powerpc/sparc overrides).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IocLayout {
    /// Width of the size field.
    pub size_bits: u32,
    /// `_IOC_NONE`.
    pub none: u32,
    /// `_IOC_READ`.
    pub read: u32,
    /// `_IOC_WRITE`.
    pub write: u32,
}

/// x86, arm, aarch64, riscv and other asm-generic architectures.
pub const GENERIC: IocLayout = IocLayout {
    size_bits: 14,
    none: 0,
    read: 2,
    write: 1,
};

/// mips, powerpc and sparc: 13 size bits and 3 direction bits.
pub const MIPS: IocLayout = IocLayout {
    size_bits: 13,
    none: 1,
    read: 2,
    write: 4,
};

/// Layout of the compilation target.
#[cfg(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "mips32r6",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64"
))]
pub const NATIVE: IocLayout = MIPS;
/// Layout of the compilation target.
#[cfg(not(any(
    target_arch = "mips",
    target_arch = "mips64",
    target_arch = "mips32r6",
    target_arch = "mips64r6",
    target_arch = "powerpc",
    target_arch = "powerpc64",
    target_arch = "sparc",
    target_arch = "sparc64"
)))]
pub const NATIVE: IocLayout = GENERIC;

impl IocLayout {
    /// `_IOC(dir, type, nr, size)`.
    pub const fn ioc(self, dir: u32, ty: u8, nr: u8, size: usize) -> u32 {
        let size_shift = 16;
        let dir_shift = size_shift + self.size_bits;
        (dir << dir_shift) | ((size as u32) << size_shift) | ((ty as u32) << 8) | nr as u32
    }

    /// `_IO(type, nr)`.
    pub const fn io(self, ty: u8, nr: u8) -> u32 {
        self.ioc(self.none, ty, nr, 0)
    }

    /// `_IOR(type, nr, size)`.
    pub const fn ior(self, ty: u8, nr: u8, size: usize) -> u32 {
        self.ioc(self.read, ty, nr, size)
    }

    /// `_IOW(type, nr, size)`.
    pub const fn iow(self, ty: u8, nr: u8, size: usize) -> u32 {
        self.ioc(self.write, ty, nr, size)
    }
}

/// Frontend and demux request numbers for a given layout and pointer width.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DvbIoctls {
    pub fe_get_info: u32,
    pub fe_diseqc_send_master_cmd: u32,
    pub fe_diseqc_send_burst: u32,
    pub fe_set_tone: u32,
    pub fe_set_voltage: u32,
    pub fe_read_status: u32,
    pub fe_read_ber: u32,
    pub fe_read_signal_strength: u32,
    pub fe_read_snr: u32,
    pub fe_get_event: u32,
    pub fe_set_property: u32,
    pub fe_get_property: u32,
    pub dmx_stop: u32,
    pub dmx_set_filter: u32,
    pub dmx_set_pes_filter: u32,
    pub dmx_set_buffer_size: u32,
    pub dmx_set_source: u32,
    pub dmx_add_pid: u32,
    pub dmx_remove_pid: u32,
}

/// `sizeof(struct dvb_frontend_info)`.
pub const FRONTEND_INFO_SIZE: usize = 168;
/// `sizeof(struct dvb_diseqc_master_cmd)`.
pub const DISEQC_MASTER_CMD_SIZE: usize = 7;
/// `sizeof(struct dvb_frontend_event)`.
pub const FRONTEND_EVENT_SIZE: usize = 40;
/// `sizeof(struct dmx_pes_filter_params)`.
pub const PES_FILTER_SIZE: usize = 20;
/// `sizeof(struct dmx_sct_filter_params)`.
pub const SCT_FILTER_SIZE: usize = 60;

impl DvbIoctls {
    /// Request numbers; `ptr_size` is `sizeof(void *)`, which sizes `struct dtv_properties`.
    pub const fn new(layout: IocLayout, ptr_size: usize) -> Self {
        let o = b'o';
        let dtv_properties = 2 * ptr_size;
        Self {
            fe_get_info: layout.ior(o, 61, FRONTEND_INFO_SIZE),
            fe_diseqc_send_master_cmd: layout.iow(o, 63, DISEQC_MASTER_CMD_SIZE),
            fe_diseqc_send_burst: layout.io(o, 65),
            fe_set_tone: layout.io(o, 66),
            fe_set_voltage: layout.io(o, 67),
            fe_read_status: layout.ior(o, 69, 4),
            fe_read_ber: layout.ior(o, 70, 4),
            fe_read_signal_strength: layout.ior(o, 71, 2),
            fe_read_snr: layout.ior(o, 72, 2),
            fe_get_event: layout.ior(o, 78, FRONTEND_EVENT_SIZE),
            fe_set_property: layout.iow(o, 82, dtv_properties),
            fe_get_property: layout.ior(o, 83, dtv_properties),
            dmx_stop: layout.io(o, 42),
            dmx_set_filter: layout.iow(o, 43, SCT_FILTER_SIZE),
            dmx_set_pes_filter: layout.iow(o, 44, PES_FILTER_SIZE),
            dmx_set_buffer_size: layout.io(o, 45),
            dmx_set_source: layout.iow(o, 49, 4),
            dmx_add_pid: layout.iow(o, 51, 2),
            dmx_remove_pid: layout.iow(o, 52, 2),
        }
    }
}

/// Request numbers of the compilation target.
pub const NATIVE_IOCTLS: DvbIoctls = DvbIoctls::new(NATIVE, size_of::<usize>());

#[cfg(test)]
mod tests {
    use super::*;

    // Golden values printed by a C program built against x86_64 linux/dvb headers (API 5.12).
    #[test]
    fn generic_64bit_matches_kernel_headers() {
        let io = DvbIoctls::new(GENERIC, 8);
        assert_eq!(io.fe_get_info, 0x80a8_6f3d);
        assert_eq!(io.fe_diseqc_send_master_cmd, 0x4007_6f3f);
        assert_eq!(io.fe_diseqc_send_burst, 0x0000_6f41);
        assert_eq!(io.fe_set_tone, 0x0000_6f42);
        assert_eq!(io.fe_set_voltage, 0x0000_6f43);
        assert_eq!(io.fe_read_status, 0x8004_6f45);
        assert_eq!(io.fe_read_ber, 0x8004_6f46);
        assert_eq!(io.fe_read_signal_strength, 0x8002_6f47);
        assert_eq!(io.fe_read_snr, 0x8002_6f48);
        assert_eq!(io.fe_get_event, 0x8028_6f4e);
        assert_eq!(io.fe_set_property, 0x4010_6f52);
        assert_eq!(io.fe_get_property, 0x8010_6f53);
        assert_eq!(io.dmx_stop, 0x0000_6f2a);
        assert_eq!(io.dmx_set_pes_filter, 0x4014_6f2c);
        assert_eq!(io.dmx_set_filter, 0x403c_6f2b);
        assert_eq!(io.dmx_set_buffer_size, 0x0000_6f2d);
        assert_eq!(io.dmx_set_source, 0x4004_6f31);
        assert_eq!(io.dmx_add_pid, 0x4002_6f33);
        assert_eq!(io.dmx_remove_pid, 0x4002_6f34);
    }

    // 32-bit ARM differs only in sizeof(struct dtv_properties) == 8.
    #[test]
    fn generic_32bit_property_size() {
        let io = DvbIoctls::new(GENERIC, 4);
        assert_eq!(io.fe_set_property, 0x4008_6f52);
        assert_eq!(io.fe_get_property, 0x8008_6f53);
        assert_eq!(io.fe_set_tone, 0x0000_6f42);
    }

    // mips: _IOC_NONE=1, _IOC_READ=2, _IOC_WRITE=4 at bit 29 (arch/mips/include/uapi/asm/ioctl.h).
    #[test]
    fn mips_encoding() {
        let io = DvbIoctls::new(MIPS, 4);
        assert_eq!(io.fe_set_tone, 0x2000_6f42);
        assert_eq!(io.fe_set_voltage, 0x2000_6f43);
        assert_eq!(io.fe_diseqc_send_burst, 0x2000_6f41);
        assert_eq!(io.fe_diseqc_send_master_cmd, 0x8007_6f3f);
        assert_eq!(io.fe_read_status, 0x4004_6f45);
        assert_eq!(io.fe_get_info, 0x40a8_6f3d);
        assert_eq!(io.fe_set_property, 0x8008_6f52);
        assert_eq!(io.fe_get_property, 0x4008_6f53);
        assert_eq!(io.dmx_stop, 0x2000_6f2a);
        assert_eq!(io.dmx_set_pes_filter, 0x8014_6f2c);
        assert_eq!(io.dmx_set_filter, 0x803c_6f2b);
        assert_eq!(io.dmx_add_pid, 0x8002_6f33);
    }

    #[cfg(target_arch = "x86_64")]
    #[test]
    fn native_is_generic_64() {
        assert_eq!(NATIVE_IOCTLS, DvbIoctls::new(GENERIC, 8));
    }
}
