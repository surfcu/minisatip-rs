//! The CI device under the host stack: byte I/O plus the slot reset/info ioctls.

use std::io;

/// `CA_CI_LINK` from `linux/dvb/ca.h`: a link-layer (TPDU) interface.
pub const CA_CI_LINK: u32 = 2;
pub const CA_CI_MODULE_PRESENT: u32 = 1;
pub const CA_CI_MODULE_READY: u32 = 2;

/// `struct ca_slot_info` without the slot number.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SlotInfo {
    pub kind: u32,
    pub flags: u32,
}

impl SlotInfo {
    /// `ca_slot_has_module`: present or ready proceeds.
    pub fn has_module(&self) -> bool {
        self.flags & (CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY) != 0
    }
}

/// An open `/dev/dvb/adapterN/caM` (or `/dev/ciN` on enigma) and its ioctls.
pub trait CaLink: Send {
    /// One message; `WouldBlock` when nothing is queued, `Ok(0)` at EOF.
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize>;
    fn write(&mut self, buf: &[u8]) -> io::Result<usize>;
    /// `CA_RESET`.
    fn reset(&mut self) -> io::Result<()>;
    /// `CA_GET_SLOT_INFO` for slot 0.
    fn slot_info(&mut self) -> io::Result<SlotInfo>;
    fn sleep_ms(&mut self, ms: u32) {
        std::thread::sleep(std::time::Duration::from_millis(u64::from(ms)));
    }
    /// `flush_handle`: drops queued input; the real device stops after 1 s.
    fn flush_input(&mut self) {
        let mut buf = [0u8; 256];
        for _ in 0..1024 {
            if !matches!(self.read(&mut buf), Ok(n) if n > 0) {
                break;
            }
        }
    }
}

/// Outcome of [`reset_and_wait_ready`]: C++ returns 0, 1 and -1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResetResult {
    Ready,
    Timeout,
    IoError,
}

/// `ca_reset_and_wait_ready`: resets, polls for READY, retries while a module is present.
pub fn reset_and_wait_ready(
    link: &mut dyn CaLink,
    info: &mut SlotInfo,
    id: usize,
    attempts: u32,
    polls: u32,
) -> ResetResult {
    for attempt in 0..attempts {
        if link.reset().is_err() {
            return ResetResult::IoError;
        }
        let mut seen_present = false;
        for _ in 0..polls {
            match link.slot_info() {
                Ok(i) => *info = i,
                Err(_) => return ResetResult::IoError,
            }
            if info.flags & CA_CI_MODULE_READY != 0 {
                return ResetResult::Ready;
            }
            seen_present |= info.flags & CA_CI_MODULE_PRESENT != 0;
            link.sleep_ms(10);
        }
        match link.slot_info() {
            Ok(i) => *info = i,
            Err(_) => return ResetResult::IoError,
        }
        if info.flags & CA_CI_MODULE_READY != 0 {
            return ResetResult::Ready;
        }
        if info.flags & CA_CI_MODULE_PRESENT == 0 && !seen_present {
            ms_log::log!("CA {}: no module present, not retrying reset", id);
            return ResetResult::Timeout;
        }
        if attempt + 1 < attempts {
            ms_log::log!(
                "CA {}: module not ready after reset {}/{}, retrying",
                id,
                attempt + 1,
                attempts
            );
        }
    }
    ResetResult::Timeout
}

/// Adapter type, as far as `dvbca_init_dev` cares (`ADAPTER_DVB`, `ADAPTER_CI`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    Dvb,
    Ci,
    Other,
}

/// The adapter fields `dvbca_init_dev` and `ca_reconnect` read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterInfo {
    pub id: i32,
    pub enabled: bool,
    pub kind: AdapterKind,
    /// Physical adapter and CA device number (`ad->pa`, `ad->fn`).
    pub pa: u32,
    pub fe: u32,
}

impl AdapterInfo {
    pub fn dvb(id: i32, pa: u32, fe: u32) -> Self {
        Self {
            id,
            enabled: true,
            kind: AdapterKind::Dvb,
            pa,
            fe,
        }
    }

    /// The device `dvbca_init_dev` opens.
    pub fn ca_path(&self, enigma: bool) -> String {
        match enigma {
            true => format!("/dev/ci{}", self.id),
            false => format!("/dev/dvb/adapter{}/ca{}", self.pa, self.fe),
        }
    }
}

/// Opens CA devices and answers the host-file probes of `dvbca_init_dev`.
pub trait CaOpener {
    fn open(&mut self, path: &str) -> io::Result<Box<dyn CaLink>>;
    /// `access("/etc/ssl/certs/root.pem")`: plain CI is forced either way (DEV-151);
    /// with it the slot init logs that CI+ is not supported.
    fn has_ciplus_root_cert(&self) -> bool {
        std::path::Path::new("/etc/ssl/certs/root.pem").exists()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Slot that becomes ready after a number of info queries.
    struct FakeSlot {
        resets: u32,
        present: bool,
        queries_until_ready: u32,
        fail_reset: bool,
        fail_info: bool,
    }

    fn slot(present: bool, queries_until_ready: u32) -> FakeSlot {
        FakeSlot {
            resets: 0,
            present,
            queries_until_ready,
            fail_reset: false,
            fail_info: false,
        }
    }

    impl CaLink for FakeSlot {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::WouldBlock.into())
        }
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            Ok(buf.len())
        }
        fn reset(&mut self) -> io::Result<()> {
            self.resets += 1;
            match self.fail_reset {
                true => Err(io::ErrorKind::Other.into()),
                false => Ok(()),
            }
        }
        fn slot_info(&mut self) -> io::Result<SlotInfo> {
            if self.fail_info {
                return Err(io::ErrorKind::Other.into());
            }
            let mut flags = 0;
            if self.present {
                flags |= CA_CI_MODULE_PRESENT;
            }
            if self.queries_until_ready == 0 {
                flags |= CA_CI_MODULE_READY;
            } else {
                self.queries_until_ready -= 1;
            }
            Ok(SlotInfo {
                kind: CA_CI_LINK,
                flags,
            })
        }
        fn sleep_ms(&mut self, _: u32) {}
    }

    fn run(s: &mut FakeSlot) -> ResetResult {
        reset_and_wait_ready(s, &mut SlotInfo::default(), 0, 3, 10)
    }

    // Ported from tests/test_ca.cpp:964
    #[test]
    fn ca_reset_ready_immediately() {
        let mut s = slot(true, 0);
        assert_eq!(run(&mut s), ResetResult::Ready, "expected ready");
        assert_eq!(s.resets, 1, "expected a single reset");
    }

    // Ported from tests/test_ca.cpp:975
    #[test]
    fn ca_reset_retries_present_module() {
        let mut s = slot(true, 25);
        assert_eq!(
            run(&mut s),
            ResetResult::Ready,
            "expected ready after retry"
        );
        assert_eq!(s.resets, 3, "expected resets until ready");
    }

    // Ported from tests/test_ca.cpp:986
    #[test]
    fn ca_reset_no_module_no_retry() {
        let mut s = slot(false, 1_000_000);
        assert_eq!(run(&mut s), ResetResult::Timeout, "expected timeout");
        assert_eq!(s.resets, 1, "expected no retry without a module");
    }

    // Ported from tests/test_ca.cpp:997
    #[test]
    fn ca_reset_gives_up_after_attempts() {
        let mut s = slot(true, 1_000_000);
        assert_eq!(run(&mut s), ResetResult::Timeout, "expected timeout");
        assert_eq!(s.resets, 3, "expected all attempts used");
    }

    // Ported from tests/test_ca.cpp:1007
    #[test]
    fn ca_reset_io_error() {
        let mut s = slot(true, 0);
        s.fail_info = true;
        assert_eq!(
            run(&mut s),
            ResetResult::IoError,
            "expected io error from slot info"
        );
        let mut s = slot(true, 0);
        s.fail_reset = true;
        assert_eq!(
            run(&mut s),
            ResetResult::IoError,
            "expected io error from reset"
        );
    }

    // Ported from tests/test_ca.cpp:1184
    #[test]
    fn ca_slot_has_module() {
        let mut info = SlotInfo::default();
        assert!(!info.has_module(), "empty slot defers");
        info.flags = CA_CI_MODULE_PRESENT;
        assert!(info.has_module(), "present proceeds");
        info.flags = CA_CI_MODULE_READY;
        assert!(info.has_module(), "ready proceeds");
    }
}
