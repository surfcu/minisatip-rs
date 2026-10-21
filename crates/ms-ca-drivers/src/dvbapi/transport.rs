//! Opening the DVBAPI transport: a TCP connection or a Unix socket for `/` paths.

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::time::Duration;

use super::client::DvbapiConfig;

/// A connection to the DVBAPI server (`sock` in `dvbapi.cpp`).
#[derive(Debug)]
pub enum Stream {
    /// TCP connection to `host:port`.
    Tcp(TcpStream),
    /// Local socket (`-o /path`).
    #[cfg(unix)]
    Unix(std::os::unix::net::UnixStream),
}

impl Stream {
    /// Puts the stream in nonblocking mode or back.
    pub fn set_nonblocking(&self, nonblocking: bool) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.set_nonblocking(nonblocking),
            #[cfg(unix)]
            Self::Unix(s) => s.set_nonblocking(nonblocking),
        }
    }

    /// Descriptor of the connection (the C++ `sock`), -1 where unavailable.
    #[cfg(unix)]
    pub fn raw_fd(&self) -> i32 {
        use std::os::unix::io::AsRawFd;
        match self {
            Self::Tcp(s) => s.as_raw_fd(),
            Self::Unix(s) => s.as_raw_fd(),
        }
    }

    /// Descriptor of the connection (the C++ `sock`), -1 where unavailable.
    #[cfg(not(unix))]
    pub fn raw_fd(&self) -> i32 {
        -1
    }
}

impl Read for Stream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.read(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.read(buf),
        }
    }
}

impl Write for Stream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self {
            Self::Tcp(s) => s.write(buf),
            #[cfg(unix)]
            Self::Unix(s) => s.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        match self {
            Self::Tcp(s) => s.flush(),
            #[cfg(unix)]
            Self::Unix(s) => s.flush(),
        }
    }
}

fn invalid(msg: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, msg.to_string())
}

/// Connects like `connect_dvbapi`; a host starting with `/` selects the local socket.
pub fn connect(cfg: &DvbapiConfig, timeout: Duration) -> io::Result<Stream> {
    if cfg.host.is_empty() || cfg.port <= 0 {
        return Err(invalid("dvbapi is not configured"));
    }
    if !cfg.network_mode() {
        return connect_local(&cfg.host);
    }
    let port = u16::try_from(cfg.port).map_err(|_| invalid("dvbapi port out of range"))?;
    let mut addrs = (cfg.host.as_str(), port).to_socket_addrs()?;
    let addr = addrs.next().ok_or_else(|| invalid("no dvbapi address"))?;
    TcpStream::connect_timeout(&addr, timeout).map(Stream::Tcp)
}

#[cfg(unix)]
fn connect_local(path: &str) -> io::Result<Stream> {
    std::os::unix::net::UnixStream::connect(path).map(Stream::Unix)
}

#[cfg(not(unix))]
fn connect_local(_path: &str) -> io::Result<Stream> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "local dvbapi sockets need unix",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;

    fn cfg(host: &str, port: i32) -> DvbapiConfig {
        DvbapiConfig {
            host: host.to_string(),
            port,
            ..DvbapiConfig::default()
        }
    }

    #[test]
    fn connects_tcp_and_exchanges_bytes() {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut b = [0u8; 4];
            s.read_exact(&mut b).unwrap();
            s.write_all(b"pong").unwrap();
            b
        });
        let mut stream = connect(&cfg("127.0.0.1", port as i32), Duration::from_secs(1)).unwrap();
        stream.write_all(b"ping").unwrap();
        let mut reply = [0u8; 4];
        stream.read_exact(&mut reply).unwrap();
        assert_eq!(reply, *b"pong");
        assert_eq!(server.join().unwrap(), *b"ping");
    }

    #[test]
    fn handshake_with_a_tcp_fake_oscam() {
        use super::super::client::DvbapiClient;
        use super::super::proto;
        use ms_ca::{CaEvent, CaService};

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let server = std::thread::spawn(move || {
            let (mut s, _) = listener.accept().unwrap();
            let mut head = [0u8; 7];
            s.read_exact(&mut head).unwrap();
            assert_eq!(&head[..4], &proto::DVBAPI_CLIENT_INFO.to_be_bytes());
            let mut name = vec![0u8; head[6] as usize];
            s.read_exact(&mut name).unwrap();
            let info = b"OSCam v1.20, build r11720 (x86_64-linux)";
            let mut reply = proto::DVBAPI_SERVER_INFO.to_be_bytes().to_vec();
            reply.extend_from_slice(&2u16.to_be_bytes());
            reply.push(info.len() as u8);
            reply.extend_from_slice(info);
            s.write_all(&reply).unwrap();
            let mut pmt = [0u8; 6];
            s.read_exact(&mut pmt).unwrap();
            assert_eq!(&pmt[..4], &0x9F80_3282u32.to_be_bytes());
            let len = u16::from_be_bytes([pmt[4], pmt[5]]) as usize;
            let mut body = vec![0u8; len];
            s.read_exact(&mut body).unwrap();
            (name, len)
        });

        let config = cfg("127.0.0.1", port as i32);
        let mut stream = connect(&config, Duration::from_secs(5)).unwrap();
        let mut client = DvbapiClient::new(config);
        client.on_connected(0);
        stream.write_all(&client.take_output().unwrap()).unwrap();

        let mut head = [0u8; 7];
        stream.read_exact(&mut head).unwrap();
        let mut info = vec![0u8; head[6] as usize];
        stream.read_exact(&mut info).unwrap();
        let mut reply = head.to_vec();
        reply.extend_from_slice(&info);
        client.on_data(&reply, 0);
        assert_eq!(client.next_event(), Some(CaEvent::Ready));

        let svc = CaService {
            pmt_id: 0,
            adapter: 0,
            sid: 100,
            pmt_pid: 48,
            ..CaService::default()
        };
        client.add_service(&svc, false, 0).unwrap();
        stream.write_all(&client.take_output().unwrap()).unwrap();

        let (name, len) = server.join().unwrap();
        assert_eq!(&name[..9], b"minisatip");
        assert_eq!(len, 0x14);
    }

    #[cfg(unix)]
    #[test]
    fn connects_a_local_socket() {
        let path = std::env::temp_dir().join(format!(
            "ms-ca-drivers-dvbapi-{}-{}.sock",
            std::process::id(),
            line!()
        ));
        let _ = std::fs::remove_file(&path);
        let listener = std::os::unix::net::UnixListener::bind(&path).unwrap();
        let server = std::thread::spawn(move || listener.accept().map(|_| ()));
        let stream = connect(&cfg(path.to_str().unwrap(), 9000), Duration::from_secs(1));
        assert!(stream.is_ok());
        assert!(server.join().unwrap().is_ok());
        std::fs::remove_file(&path).unwrap();
    }

    #[test]
    fn rejects_unconfigured_addresses() {
        let err = connect(&cfg("", 9000), Duration::from_millis(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        let err = connect(&cfg("127.0.0.1", 0), Duration::from_millis(1)).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidInput);
        assert!(connect(&cfg("999.999.999.999", 9000), Duration::from_millis(1)).is_err());
    }
}
