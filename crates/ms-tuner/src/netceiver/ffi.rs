//! Hand-written libnetceiver (libmcli) bindings; layouts follow lib/defs.h, recv_ccpp.h,
//! recv_tv.h and tca_handler.h of vdr-projects/libnetceiver.

use std::collections::HashMap;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::sync::Arc;

use super::api::{
    FeStatus, McliDiscoveryApi, McliReceiverApi, NetceiverInfo, NetcvShared, NetcvTunerInfo,
    ReceiverId,
};
use super::backend::MAX_PIDS;
use super::ffi_types::*;
use super::params::{FrontendUnion, McliTune};

type TsHandler = extern "C" fn(*mut u8, libc::size_t, *mut c_void) -> c_int;
type TenHandler = extern "C" fn(*mut TraPrefixRaw, *mut c_void) -> c_int;

#[link(name = "netceiver")]
unsafe extern "C" {
    fn recv_init(intf: *mut c_char, port: c_int) -> c_int;
    fn api_sock_init(cmd_sock_path: *const c_char) -> c_int;
    fn nc_get_list() -> *mut NetceiverInfoListRaw;
    fn nc_lock_list() -> c_int;
    fn nc_unlock_list() -> c_int;
    fn recv_add() -> *mut c_void;
    fn recv_del(r: *mut c_void);
    fn recv_stop(r: *mut c_void) -> c_int;
    fn recv_pids(r: *mut c_void, pids: *mut DvbPidRaw) -> c_int;
    fn recv_tune(
        r: *mut c_void,
        fe_type: c_int,
        satpos: c_int,
        sec: *mut RecvSecRaw,
        fe_parms: *mut FrontendParamsRaw,
        pids: *mut DvbPidRaw,
    ) -> c_int;
    fn register_ts_handler(r: *mut c_void, p: Option<TsHandler>, c: *mut c_void) -> c_int;
    fn register_ten_handler(r: *mut c_void, p: Option<TenHandler>, c: *mut c_void) -> c_int;
}

/// libmcli does not export these, but its debug macros reference them (netceiver.cpp:41).
#[unsafe(no_mangle)]
static mut m_debugmask: c_int = 0;
#[unsafe(no_mangle)]
static mut m_logskipmask: c_int = 0;

extern "C" fn handle_ts(buffer: *mut u8, len: libc::size_t, ctx: *mut c_void) -> c_int {
    if buffer.is_null() || ctx.is_null() {
        return len as c_int;
    }
    // SAFETY: ctx is the Arc<NetcvShared> pointer registered in recv_add and released only
    // after unregistering; libmcli passes a buffer of len readable bytes.
    let (shared, data) = unsafe {
        (
            &*(ctx as *const NetcvShared),
            std::slice::from_raw_parts(buffer, len),
        )
    };
    shared.handle_ts(data) as c_int
}

extern "C" fn handle_ten(ten: *mut TraPrefixRaw, ctx: *mut c_void) -> c_int {
    if ctx.is_null() {
        return 0;
    }
    // SAFETY: ctx is the registered Arc<NetcvShared>; ten is NULL or a valid tra_t whose
    // leading fields match TraPrefixRaw.
    let (shared, ten) = unsafe { (&*(ctx as *const NetcvShared), ten.as_ref()) };
    shared.handle_ten(
        ten.map(|t| FeStatus {
            st: t.s.st,
            ber: t.s.ber,
            strength: t.s.strength,
            snr: t.s.snr,
        })
        .as_ref(),
    );
    0
}

fn c_string(chars: &[c_char]) -> String {
    let bytes: Vec<u8> = chars
        .iter()
        .map(|&c| c as u8)
        .take_while(|&b| b != 0)
        .collect();
    String::from_utf8_lossy(&bytes).into_owned()
}

/// libmcli through FFI; one instance per adapter thread plus one for discovery.
#[derive(Debug, Default)]
pub struct LibMcli {
    contexts: HashMap<usize, usize>,
}

impl LibMcli {
    /// Binding with no receivers.
    pub fn new() -> Self {
        Self::default()
    }
}

impl Drop for LibMcli {
    fn drop(&mut self) {
        for rx in self.contexts.keys().copied().collect::<Vec<_>>() {
            self.recv_del(ReceiverId(rx));
        }
    }
}

impl McliReceiverApi for LibMcli {
    fn recv_add(&mut self, shared: Arc<NetcvShared>) -> Option<ReceiverId> {
        // SAFETY: recv_add takes no arguments and returns NULL or a receiver it owns.
        let r = unsafe { recv_add() };
        if r.is_null() {
            return None;
        }
        let ctx = Arc::into_raw(shared) as *mut c_void;
        // SAFETY: r is a live receiver; ctx stays valid until recv_del unregisters it.
        unsafe {
            register_ten_handler(r, Some(handle_ten), ctx);
            register_ts_handler(r, Some(handle_ts), ctx);
        }
        self.contexts.insert(r as usize, ctx as usize);
        Some(ReceiverId(r as usize))
    }

    fn recv_del(&mut self, rx: ReceiverId) {
        let Some(ctx) = self.contexts.remove(&rx.0) else {
            return;
        };
        let r = rx.0 as *mut c_void;
        // SAFETY: r came from recv_add and is deleted once; handlers are cleared first so no
        // callback can use ctx after it is released.
        unsafe {
            register_ten_handler(r, None, std::ptr::null_mut());
            register_ts_handler(r, None, std::ptr::null_mut());
            recv_del(r);
            drop(Arc::from_raw(ctx as *const NetcvShared));
        }
    }

    fn recv_tune(&mut self, rx: ReceiverId, tune: &McliTune) -> i32 {
        let mut sec = RecvSecRaw {
            voltage: tune.voltage,
            ..RecvSecRaw::default()
        };
        let mut fep = FrontendParamsRaw {
            frequency: tune.fep.frequency,
            inversion: tune.fep.inversion,
            u: [0; 7],
        };
        match tune.fep.u {
            FrontendUnion::None => {}
            FrontendUnion::Qpsk {
                symbol_rate,
                fec_inner,
            } => fep.u[..2].copy_from_slice(&[symbol_rate as c_int, fec_inner]),
            FrontendUnion::Qam {
                symbol_rate,
                fec_inner,
                modulation,
            } => fep.u[..3].copy_from_slice(&[symbol_rate as c_int, fec_inner, modulation]),
            FrontendUnion::Ofdm {
                bandwidth,
                code_rate_hp,
                code_rate_lp,
                constellation,
                transmission_mode,
                guard_interval,
                hierarchy,
            } => {
                fep.u = [
                    bandwidth,
                    code_rate_hp,
                    code_rate_lp,
                    constellation,
                    transmission_mode,
                    guard_interval,
                    hierarchy,
                ]
            }
        }
        let mut pids = [DvbPidRaw::default(); MAX_PIDS];
        pids[0].pid = -1;
        // SAFETY: rx is a live receiver and every pointer refers to a local that outlives the
        // call; the PID list is terminated by pid -1.
        unsafe {
            recv_tune(
                rx.0 as *mut c_void,
                tune.fe_type,
                tune.satpos,
                &mut sec,
                &mut fep,
                pids.as_mut_ptr(),
            )
        }
    }

    fn recv_pids(&mut self, rx: ReceiverId, pids: &[u16]) -> i32 {
        let mut list: Vec<DvbPidRaw> = pids
            .iter()
            .map(|&pid| DvbPidRaw {
                pid: c_int::from(pid),
                ..DvbPidRaw::default()
            })
            .collect();
        list.push(DvbPidRaw {
            pid: -1,
            ..DvbPidRaw::default()
        });
        // SAFETY: rx is a live receiver; list is -1 terminated and outlives the call.
        unsafe { recv_pids(rx.0 as *mut c_void, list.as_mut_ptr()) }
    }

    fn recv_stop(&mut self, rx: ReceiverId) -> i32 {
        // SAFETY: rx is a live receiver.
        unsafe { recv_stop(rx.0 as *mut c_void) }
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}

impl McliDiscoveryApi for LibMcli {
    fn interface_ready(&mut self, ifname: &str) -> bool {
        const IFF_NETCV: u32 = (libc::IFF_UP | libc::IFF_RUNNING | libc::IFF_MULTICAST) as u32;
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        // SAFETY: getifaddrs fills head with a list freed below with freeifaddrs.
        if unsafe { libc::getifaddrs(&mut head) } != 0 {
            return false;
        }
        let mut ready = false;
        let mut cur = head;
        while !cur.is_null() {
            // SAFETY: cur is a node of the list returned by getifaddrs, still allocated.
            let ifa = unsafe { &*cur };
            // SAFETY: ifa_name is a NUL-terminated name; ifa_addr is NULL or a valid sockaddr.
            let (name, family) = unsafe {
                (
                    CStr::from_ptr(ifa.ifa_name),
                    ifa.ifa_addr.as_ref().map(|a| c_int::from(a.sa_family)),
                )
            };
            if name.to_bytes() == ifname.as_bytes() && family == Some(libc::AF_INET6) {
                ready = ifa.ifa_flags & IFF_NETCV == IFF_NETCV;
                break;
            }
            cur = ifa.ifa_next;
        }
        // SAFETY: head came from getifaddrs and is freed once.
        unsafe { libc::freeifaddrs(head) };
        ready
    }

    fn recv_init(&mut self, ifname: &str, port: u16) -> i32 {
        let Ok(name) = CString::new(ifname) else {
            return -1;
        };
        // libmcli may keep the pointer (C++ passes opts.netcv_if), so it is leaked.
        let name = name.into_raw();
        // SAFETY: name is a valid NUL-terminated string that is never freed.
        unsafe { recv_init(name, c_int::from(port)) }
    }

    fn api_sock_init(&mut self, path: &str) -> i32 {
        let Ok(path) = CString::new(path) else {
            return -1;
        };
        let path = path.into_raw();
        // SAFETY: path is a valid NUL-terminated string that is never freed.
        unsafe { api_sock_init(path) }
    }

    fn netceivers(&mut self) -> Vec<NetceiverInfo> {
        // SAFETY: nc_get_list returns libmcli's list, valid until the next list update; it is
        // copied out immediately.
        let Some(list) = (unsafe { nc_get_list().as_ref() }) else {
            return Vec::new();
        };
        let count = usize::try_from(list.nci_num).unwrap_or(0);
        if list.nci.is_null() || count == 0 {
            return Vec::new();
        }
        // SAFETY: nci points to nci_num netceiver_info_t entries.
        let ncs = unsafe { std::slice::from_raw_parts(list.nci, count) };
        ncs.iter()
            .map(|nc| {
                let tuners = usize::try_from(nc.tuner_num).unwrap_or(0);
                let tuners = if nc.tuner.is_null() || tuners == 0 {
                    &[][..]
                } else {
                    // SAFETY: tuner points to tuner_num tuner_info_t entries.
                    unsafe { std::slice::from_raw_parts(nc.tuner, tuners) }
                };
                NetceiverInfo {
                    uuid: c_string(&nc.uuid),
                    tuners: tuners
                        .iter()
                        .map(|t| NetcvTunerInfo {
                            name: c_string(&t.fe_info.name),
                            fe_type: t.fe_info.fe_type,
                        })
                        .collect(),
                }
            })
            .collect()
    }

    fn lock_list(&mut self) {
        // SAFETY: plain libmcli call without arguments.
        unsafe { nc_lock_list() };
    }

    fn unlock_list(&mut self) {
        // SAFETY: plain libmcli call without arguments.
        unsafe { nc_unlock_list() };
    }

    fn sleep_ms(&mut self, ms: u64) {
        std::thread::sleep(std::time::Duration::from_millis(ms));
    }
}
