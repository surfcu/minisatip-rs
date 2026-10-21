//! libnetceiver ABI structures (lib/defs.h, recv_ccpp.h, recv_tv.h, tca_handler.h).

use std::ffi::{c_char, c_int, c_void};

pub const UUID_SIZE: usize = 256;
const MAX_CAMS: usize = 2;
const MAX_MENU_STR_LEN: usize = 64;
const CA_MAX_SLOTS: usize = 16;

/// `struct dvb_frontend_info` (168 bytes).
#[repr(C)]
pub struct DvbFrontendInfoRaw {
    pub name: [c_char; 128],
    pub fe_type: c_int,
    pub frequency_min: u32,
    pub frequency_max: u32,
    pub frequency_stepsize: u32,
    pub frequency_tolerance: u32,
    pub symbol_rate_min: u32,
    pub symbol_rate_max: u32,
    pub symbol_rate_tolerance: u32,
    pub notifier_delay: u32,
    pub caps: u32,
}

/// `tuner_info_t`.
#[repr(C)]
pub struct TunerInfoRaw {
    pub magic: c_int,
    pub version: c_int,
    pub fe_info: DvbFrontendInfoRaw,
    pub slot: c_int,
    pub preference: c_int,
    pub uuid: [c_char; UUID_SIZE],
    pub satellite_list_name: [c_char; UUID_SIZE],
}

/// `ca_caps_t`.
#[repr(C)]
pub struct CaCapsRaw {
    pub slot_num: u32,
    pub slot_type: u32,
    pub descr_num: u32,
    pub descr_type: u32,
}

/// `ca_slot_info_t`.
#[repr(C)]
pub struct CaSlotInfoRaw {
    pub num: c_int,
    pub ty: c_int,
    pub flags: u32,
}

/// `recv_cacaps_t`.
#[repr(C)]
pub struct RecvCacapsRaw {
    pub cap: CaCapsRaw,
    pub info: [CaSlotInfoRaw; CA_MAX_SLOTS],
}

/// `cam_info_t`.
#[repr(C)]
pub struct CamInfoRaw {
    pub slot: u8,
    pub status: u8,
    pub max_sids: c_int,
    pub use_sids: c_int,
    pub capmt_flag: c_int,
    pub reserved: c_int,
    pub flags: c_int,
    pub menu_string: [c_char; MAX_MENU_STR_LEN],
}

/// `netceiver_info_t`.
#[repr(C)]
pub struct NetceiverInfoRaw {
    pub magic: c_int,
    pub version: c_int,
    pub os_version: [c_char; UUID_SIZE],
    pub app_version: [c_char; UUID_SIZE],
    pub firmware_version: [c_char; UUID_SIZE],
    pub hardware_version: [c_char; UUID_SIZE],
    pub serial: [c_char; UUID_SIZE],
    pub vendor: [c_char; UUID_SIZE],
    pub uuid: [c_char; UUID_SIZE],
    pub description: [c_char; UUID_SIZE],
    pub tuner_timeout: c_int,
    pub ip: libc::in6_addr,
    pub def_con: c_int,
    pub system_uptime: libc::time_t,
    pub process_uptime: libc::time_t,
    pub lastseen: libc::time_t,
    pub tuner: *mut TunerInfoRaw,
    pub ci: RecvCacapsRaw,
    pub sat_list: *mut c_void,
    pub cam: [CamInfoRaw; MAX_CAMS],
    pub tuner_num: c_int,
    pub sat_list_num: c_int,
    pub cam_num: c_int,
}

/// `netceiver_info_list_t`.
#[repr(C)]
pub struct NetceiverInfoListRaw {
    pub nci: *mut NetceiverInfoRaw,
    pub nci_num: c_int,
}

/// `recv_festatus_t`.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct RecvFestatusRaw {
    pub st: c_int,
    pub ber: u32,
    pub strength: u16,
    pub snr: u16,
    pub ucblocks: u32,
}

/// Leading fields of `tra_t`; the TEN handler only reads `s`.
#[repr(C)]
pub struct TraPrefixRaw {
    pub magic: c_int,
    pub version: c_int,
    pub s: RecvFestatusRaw,
}

/// `dvb_pid_t`.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DvbPidRaw {
    pub pid: c_int,
    pub id: c_int,
    pub priority: c_int,
    pub re: c_int,
}

/// `struct dvb_diseqc_master_cmd`.
#[repr(C)]
#[derive(Default)]
pub struct DiseqcMasterCmdRaw {
    pub msg: [u8; 6],
    pub msg_len: u8,
}

/// `recv_sec_t`.
#[repr(C)]
#[derive(Default)]
pub struct RecvSecRaw {
    pub diseqc_cmd: DiseqcMasterCmdRaw,
    pub mini_cmd: c_int,
    pub tone_mode: c_int,
    pub voltage: c_int,
}

/// `struct dvb_frontend_parameters`; the union is 7 ints (`dvb_ofdm_parameters`).
#[repr(C)]
#[derive(Default)]
pub struct FrontendParamsRaw {
    pub frequency: u32,
    pub inversion: c_int,
    pub u: [c_int; 7],
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::offset_of;

    // Printed by a C probe compiled against the libnetceiver headers on x86_64.
    #[cfg(target_pointer_width = "64")]
    #[test]
    fn layouts_match_libnetceiver_headers() {
        assert_eq!(size_of::<DvbFrontendInfoRaw>(), 168);
        assert_eq!(size_of::<TunerInfoRaw>(), 696);
        assert_eq!(
            offset_of!(TunerInfoRaw, fe_info) + offset_of!(DvbFrontendInfoRaw, fe_type),
            136
        );
        assert_eq!(size_of::<CamInfoRaw>(), 88);
        assert_eq!(size_of::<RecvCacapsRaw>(), 208);
        assert_eq!(size_of::<NetceiverInfoRaw>(), 2520);
        assert_eq!(offset_of!(NetceiverInfoRaw, uuid), 1544);
        assert_eq!(offset_of!(NetceiverInfoRaw, ip), 2060);
        assert_eq!(offset_of!(NetceiverInfoRaw, system_uptime), 2080);
        assert_eq!(offset_of!(NetceiverInfoRaw, tuner), 2104);
        assert_eq!(offset_of!(NetceiverInfoRaw, ci), 2112);
        assert_eq!(offset_of!(NetceiverInfoRaw, sat_list), 2320);
        assert_eq!(offset_of!(NetceiverInfoRaw, cam), 2328);
        assert_eq!(offset_of!(NetceiverInfoRaw, tuner_num), 2504);
        assert_eq!(offset_of!(NetceiverInfoRaw, cam_num), 2512);
        assert_eq!(size_of::<NetceiverInfoListRaw>(), 16);
        assert_eq!(size_of::<RecvFestatusRaw>(), 16);
        assert_eq!(offset_of!(TraPrefixRaw, s), 8);
        assert_eq!(size_of::<DvbPidRaw>(), 16);
        assert_eq!(size_of::<RecvSecRaw>(), 20);
        assert_eq!(size_of::<FrontendParamsRaw>(), 36);
    }
}
