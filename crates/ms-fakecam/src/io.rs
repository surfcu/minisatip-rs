//! Serving a [`FakeCam`] over a byte link: one host write in, one module reply out.

use crate::cam::FakeCam;
use std::io::{self, Read, Write};
use std::sync::Mutex;

/// Largest message read at once, as `ca_read_tpdu` does.
pub const MAX_MESSAGE: usize = 4096;

fn lock(cam: &Mutex<FakeCam>) -> std::sync::MutexGuard<'_, FakeCam> {
    cam.lock().unwrap_or_else(|e| e.into_inner())
}

/// Serves TPDUs until EOF; every `read` is taken as one host TPDU, like the C++
/// host treats every `read` of the CA device as one module reply.
pub fn serve_tpdu<S: Read + Write>(cam: &Mutex<FakeCam>, link: &mut S) -> io::Result<()> {
    let mut buf = [0u8; MAX_MESSAGE];
    loop {
        let n = match link.read(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let reply = lock(cam).handle_tpdu(&buf[..n]);
        if let Some(reply) = reply {
            link.write_all(&reply)?;
            link.flush()?;
        }
    }
}

/// Serves TPDUs on a datagram socket, which keeps message boundaries; an empty
/// datagram ends the loop.
#[cfg(unix)]
pub fn serve_datagram(
    cam: &Mutex<FakeCam>,
    sock: &std::os::unix::net::UnixDatagram,
) -> io::Result<()> {
    let mut buf = [0u8; MAX_MESSAGE];
    loop {
        let n = match sock.recv(&mut buf) {
            Ok(0) => return Ok(()),
            Ok(n) => n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(e),
        };
        let reply = lock(cam).handle_tpdu(&buf[..n]);
        if let Some(reply) = reply {
            sock.send(&reply)?;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::ManualClock;
    use crate::config::CamConfig;
    use crate::host::{parse_r_tpdu, tpdu};
    use crate::tags::*;
    use std::collections::VecDeque;
    use std::sync::Arc;

    /// In-memory duplex: each queued message is returned by one `read`.
    struct Duplex {
        input: VecDeque<Vec<u8>>,
        output: Vec<Vec<u8>>,
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let Some(m) = self.input.pop_front() else {
                return Ok(0);
            };
            buf[..m.len()].copy_from_slice(&m);
            Ok(m.len())
        }
    }

    impl Write for Duplex {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.output.push(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn cam() -> Mutex<FakeCam> {
        Mutex::new(FakeCam::new(
            CamConfig::default(),
            Arc::new(ManualClock::new(0)),
        ))
    }

    #[test]
    fn duplex_gets_one_reply_per_message() {
        let cam = cam();
        let mut link = Duplex {
            input: VecDeque::from([tpdu(T_CREATE_TC, &[]), tpdu(T_DATA_LAST, &[])]),
            output: Vec::new(),
        };
        serve_tpdu(&cam, &mut link).unwrap();
        assert_eq!(link.output.len(), 2);
        assert_eq!(link.output[0], [0, 1, T_CTC_REPLY, 1, 1, T_SB, 2, 1, 0x80]);
        assert_eq!(parse_r_tpdu(&link.output[1]).unwrap().sb, Some(0x80));
    }

    #[cfg(unix)]
    #[test]
    fn datagram_socket_round_trip() {
        use std::os::unix::net::UnixDatagram;
        let (host, module) = UnixDatagram::pair().unwrap();
        let cam = Arc::new(cam());
        let c = Arc::clone(&cam);
        let t = std::thread::spawn(move || serve_datagram(&c, &module));
        host.send(&tpdu(T_CREATE_TC, &[])).unwrap();
        let mut buf = [0u8; 64];
        let n = host.recv(&mut buf).unwrap();
        assert_eq!(buf[2], T_CTC_REPLY);
        assert!(n > 5);
        host.send(&[]).unwrap();
        t.join().unwrap().unwrap();
    }
}
