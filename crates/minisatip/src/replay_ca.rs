//! Harness CA device for replay adapters (`MINISATIP_REPLAY_CA`, DEV-283): a unix
//! datagram socket to a fake CAM stands in for `/dev/dvb/adapterN/caM`.

use std::io;
use std::os::unix::net::UnixDatagram;

use ms_ca_drivers::en50221::link::{CA_CI_LINK, CA_CI_MODULE_PRESENT, CA_CI_MODULE_READY};
use ms_ca_drivers::en50221::{CaLink, CaOpener, SlotInfo};

/// Opens the fake CAM socket of the replay adapter in the device path.
pub struct ReplayCaOpener {
    /// Socket path; `%d` is replaced by the adapter number, else only adapter 0 has one.
    template: String,
}

impl ReplayCaOpener {
    pub fn new(template: String) -> Self {
        Self { template }
    }

    /// The fake CAM socket of `adapter`, if it has one.
    fn socket_path(&self, adapter: u32) -> Option<String> {
        if self.template.contains("%d") {
            Some(self.template.replacen("%d", &adapter.to_string(), 1))
        } else {
            (adapter == 0).then(|| self.template.clone())
        }
    }
}

/// The adapter number of `/dev/dvb/adapterN/caM`.
fn adapter_of(path: &str) -> Option<u32> {
    path.strip_prefix("/dev/dvb/adapter")?
        .split('/')
        .next()?
        .parse()
        .ok()
}

impl CaOpener for ReplayCaOpener {
    fn open(&mut self, path: &str) -> io::Result<Box<dyn CaLink>> {
        let not_found = || io::Error::from(io::ErrorKind::NotFound);
        let adapter = adapter_of(path).ok_or_else(not_found)?;
        let server = self.socket_path(adapter).ok_or_else(not_found)?;
        // One client path per adapter; a reconnect rebinds it.
        let client = format!("{server}.c{adapter}");
        let _ = std::fs::remove_file(&client);
        let sock = UnixDatagram::bind(&client)?;
        sock.connect(&server)?;
        // The CA worker polls on a timer, so reads must not block.
        sock.set_nonblocking(true)?;
        Ok(Box::new(ReplayCaLink { sock }))
    }
}

/// One TPDU per datagram, as one per read on the device; the slot is always ready.
struct ReplayCaLink {
    sock: UnixDatagram,
}

impl CaLink for ReplayCaLink {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.sock.recv(buf)
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.sock.send(buf)
    }

    fn reset(&mut self) -> io::Result<()> {
        Ok(())
    }

    fn slot_info(&mut self) -> io::Result<SlotInfo> {
        Ok(SlotInfo {
            kind: CA_CI_LINK,
            flags: CA_CI_MODULE_PRESENT | CA_CI_MODULE_READY,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn socket_paths_follow_the_template() {
        let one = ReplayCaOpener::new("/tmp/cam".to_owned());
        assert_eq!(one.socket_path(0).as_deref(), Some("/tmp/cam"));
        assert_eq!(one.socket_path(1), None);
        let each = ReplayCaOpener::new("/tmp/cam%d".to_owned());
        assert_eq!(each.socket_path(2).as_deref(), Some("/tmp/cam2"));
        assert_eq!(adapter_of("/dev/dvb/adapter3/ca0"), Some(3));
        assert_eq!(adapter_of("/dev/ci3"), None);
    }

    #[test]
    fn link_exchanges_datagrams_with_the_cam() {
        let dir = std::env::temp_dir().join(format!("msrs-rca-{}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("dir");
        let server = dir.join("cam");
        let cam = UnixDatagram::bind(&server).expect("bind");
        let mut opener = ReplayCaOpener::new(server.display().to_string());
        let mut link = opener.open("/dev/dvb/adapter0/ca0").expect("open");
        let mut buf = [0u8; 16];
        assert_eq!(
            link.read(&mut buf).map_err(|e| e.kind()),
            Err(io::ErrorKind::WouldBlock)
        );
        link.write(&[1, 2, 3]).expect("write");
        let (n, from) = cam.recv_from(&mut buf).expect("recv");
        assert_eq!(&buf[..n], [1, 2, 3]);
        cam.send_to_addr(&[4, 5], &from).expect("reply");
        std::thread::sleep(std::time::Duration::from_millis(20));
        assert_eq!(link.read(&mut buf).expect("read"), 2);
        assert!(link.slot_info().expect("info").flags & CA_CI_MODULE_READY != 0);
        assert!(
            opener.open("/dev/dvb/adapter1/ca0").is_err(),
            "only adapter 0 without %d"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
