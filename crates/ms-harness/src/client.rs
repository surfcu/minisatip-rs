//! Client endpoints: RTSP connections (with interleaved RTP), UDP pairs, HTTP.

use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream, UdpSocket};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::tsgen::TS;

const POLL: Duration = Duration::from_millis(50);
const MAX_UNITS: usize = 200_000;
const MAX_BYTES: usize = 64 * 1024 * 1024;

/// Data received from one output since the last [`Buffer::clear`].
#[derive(Default)]
pub struct Buffer {
    pub datagrams: Vec<Vec<u8>>,
    pub bytes: Vec<u8>,
    pub closed: bool,
}

impl Buffer {
    fn push_datagram(&mut self, datagram: &[u8]) {
        if self.datagrams.len() < MAX_UNITS {
            self.datagrams.push(datagram.to_vec());
        }
    }

    fn push_bytes(&mut self, bytes: &[u8]) {
        if self.bytes.len() < MAX_BYTES {
            self.bytes.extend_from_slice(bytes);
        }
    }

    /// Drops everything received so far, keeping TS byte streams packet aligned.
    pub fn clear(&mut self) {
        self.datagrams.clear();
        let partial = self.bytes.len() % TS;
        let whole = self.bytes.len() - partial;
        self.bytes.drain(..whole);
    }
}

pub type Shared = Arc<Mutex<Buffer>>;

fn lock(buffer: &Shared) -> std::sync::MutexGuard<'_, Buffer> {
    buffer
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Clears a shared buffer.
pub fn clear(buffer: &Shared) {
    lock(buffer).clear();
}

/// Runs `f` on the current buffer contents.
pub fn with<T>(buffer: &Shared, f: impl FnOnce(&Buffer) -> T) -> T {
    f(&lock(buffer))
}

struct Worker {
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl Worker {
    fn spawn(f: impl FnOnce(Arc<AtomicBool>) + Send + 'static) -> io::Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let handle = thread::Builder::new()
            .name("harness-io".to_owned())
            .spawn(move || f(flag))?;
        Ok(Self {
            stop,
            handle: Some(handle),
        })
    }
}

impl Drop for Worker {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn would_block(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut | io::ErrorKind::Interrupted
    )
}

/// An RTP/RTCP client port pair.
pub struct UdpPair {
    pub rtp_port: u16,
    pub rtp: Shared,
    pub rtcp: Shared,
    _workers: Vec<Worker>,
    _lease: crate::server::PortLease,
}

impl UdpPair {
    /// Binds a free RTP/RTCP pair that uses none of the `avoid` port numbers.
    pub fn bind(avoid: &[u16]) -> io::Result<Self> {
        let lease = crate::server::free_udp_range(2, avoid)?;
        let base = lease.base();
        let rtp_socket = UdpSocket::bind(("127.0.0.1", base))?;
        let rtcp_socket = UdpSocket::bind(("127.0.0.1", base + 1))?;
        let rtp = Shared::default();
        let rtcp = Shared::default();
        let workers = vec![
            receive(rtp_socket, Arc::clone(&rtp))?,
            receive(rtcp_socket, Arc::clone(&rtcp))?,
        ];
        Ok(Self {
            rtp_port: base,
            rtp,
            rtcp,
            _workers: workers,
            _lease: lease,
        })
    }
}

fn receive(socket: UdpSocket, buffer: Shared) -> io::Result<Worker> {
    socket.set_read_timeout(Some(POLL))?;
    Worker::spawn(move |stop| {
        let mut datagram = vec![0u8; 65_536];
        while !stop.load(Ordering::SeqCst) {
            match socket.recv(&mut datagram) {
                Ok(length) => lock(&buffer).push_datagram(&datagram[..length]),
                Err(error) if would_block(&error) => {}
                Err(_) => break,
            }
        }
    })
}

/// A complete RTSP/HTTP message, or the end of the connection.
pub enum Reply {
    Message(Vec<u8>),
    Closed,
}

/// An RTSP control connection; interleaved channel 0 lands in `interleaved`.
pub struct RtspConn {
    stream: TcpStream,
    replies: Receiver<Reply>,
    pub interleaved: Shared,
    pub cseq: u32,
    pub session: Option<String>,
    pub stream_id: Option<String>,
    closed: bool,
    _worker: Worker,
}

impl RtspConn {
    pub fn connect(port: u16) -> io::Result<Self> {
        let stream = TcpStream::connect(("127.0.0.1", port))?;
        stream.set_read_timeout(Some(POLL))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let reader = stream.try_clone()?;
        let interleaved = Shared::default();
        let (sender, replies) = mpsc::channel();
        let buffer = Arc::clone(&interleaved);
        let worker = Worker::spawn(move |stop| read_rtsp(reader, buffer, sender, stop))?;
        Ok(Self {
            stream,
            replies,
            interleaved,
            cseq: 0,
            session: None,
            stream_id: None,
            closed: false,
            _worker: worker,
        })
    }

    /// Sends `text` and waits for one reply.
    pub fn exchange(&mut self, text: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
        while self.replies.try_recv().is_ok() {}
        if self.closed {
            return Err("<closed>".to_owned());
        }
        if self
            .stream
            .write_all(text)
            .and_then(|()| self.stream.flush())
            .is_err()
        {
            return Err("<write failed>".to_owned());
        }
        match self.replies.recv_timeout(timeout) {
            Ok(Reply::Message(bytes)) => Ok(bytes),
            Ok(Reply::Closed) => {
                self.closed = true;
                Err("<closed>".to_owned())
            }
            Err(_) => Err("<no response>".to_owned()),
        }
    }

    pub fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn read_rtsp(
    mut reader: TcpStream,
    interleaved: Shared,
    replies: Sender<Reply>,
    stop: Arc<AtomicBool>,
) {
    let mut buffer = Vec::new();
    let mut chunk = vec![0u8; 65_536];
    while !stop.load(Ordering::SeqCst) {
        match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if would_block(&error) => continue,
            Err(_) => break,
        }
        loop {
            if buffer.first() == Some(&b'$') {
                if buffer.len() < 4 {
                    break;
                }
                let length = usize::from(u16::from_be_bytes([buffer[2], buffer[3]]));
                if buffer.len() < 4 + length {
                    break;
                }
                if buffer[1] == 0 {
                    lock(&interleaved).push_datagram(&buffer[4..4 + length]);
                }
                buffer.drain(..4 + length);
                continue;
            }
            let Some(total) = message_length(&buffer) else {
                break;
            };
            let message: Vec<u8> = buffer.drain(..total).collect();
            if replies.send(Reply::Message(message)).is_err() {
                return;
            }
        }
    }
    lock(&interleaved).closed = true;
    let _ = replies.send(Reply::Closed);
}

/// Length of the first complete message (head plus `Content-Length` body).
fn message_length(buffer: &[u8]) -> Option<usize> {
    let end = buffer.windows(4).position(|window| window == b"\r\n\r\n")? + 4;
    let head = String::from_utf8_lossy(&buffer[..end]);
    let length = content_length(&head).unwrap_or(0);
    (buffer.len() >= end + length).then_some(end + length)
}

fn content_length(head: &str) -> Option<usize> {
    head.lines()
        .find_map(|line| crate::normalize::header(line, "content-length"))
        .and_then(|value| value.parse().ok())
}

/// Splits a message into its head text and body bytes.
pub fn split(message: &[u8]) -> (String, Vec<u8>) {
    match message.windows(4).position(|window| window == b"\r\n\r\n") {
        Some(end) => (
            String::from_utf8_lossy(&message[..end]).into_owned(),
            message[end + 4..].to_vec(),
        ),
        None => (String::from_utf8_lossy(message).into_owned(), Vec::new()),
    }
}

/// One HTTP request on a new connection; reads until `Content-Length` or EOF.
pub fn http_request(port: u16, request: &[u8], timeout: Duration) -> Result<Vec<u8>, String> {
    let mut stream =
        TcpStream::connect(("127.0.0.1", port)).map_err(|_| "<connect failed>".to_owned())?;
    let _ = stream.set_read_timeout(Some(POLL));
    stream
        .write_all(request)
        .map_err(|_| "<write failed>".to_owned())?;
    let deadline = Instant::now() + timeout;
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 16_384];
    loop {
        if let Some(total) = message_length(&buffer) {
            let (head, _) = split(&buffer);
            if content_length(&head).is_some() {
                buffer.truncate(total);
                return Ok(buffer);
            }
        }
        if Instant::now() >= deadline {
            return if buffer.is_empty() {
                Err("<no response>".to_owned())
            } else {
                Ok(buffer)
            };
        }
        match stream.read(&mut chunk) {
            Ok(0) => {
                return if buffer.is_empty() {
                    Err("<closed>".to_owned())
                } else {
                    Ok(buffer)
                };
            }
            Ok(read) => buffer.extend_from_slice(&chunk[..read]),
            Err(error) if would_block(&error) => {}
            Err(_) => return Err("<read failed>".to_owned()),
        }
    }
}

/// An HTTP streaming connection; the body after the head lands in `data`.
pub struct HttpStream {
    stream: TcpStream,
    pub data: Shared,
    _worker: Worker,
}

impl HttpStream {
    /// Sends `request` and waits for the response head.
    pub fn open(port: u16, request: &[u8], timeout: Duration) -> Result<(Self, String), String> {
        let mut stream =
            TcpStream::connect(("127.0.0.1", port)).map_err(|_| "<connect failed>".to_owned())?;
        let _ = stream.set_read_timeout(Some(POLL));
        stream
            .write_all(request)
            .map_err(|_| "<write failed>".to_owned())?;
        let reader = stream.try_clone().map_err(|error| error.to_string())?;
        let data = Shared::default();
        let (sender, heads) = mpsc::channel();
        let buffer = Arc::clone(&data);
        let worker = Worker::spawn(move |stop| read_http_stream(reader, buffer, sender, stop))
            .map_err(|error| error.to_string())?;
        let head = heads
            .recv_timeout(timeout)
            .unwrap_or_else(|_| "<no response>".to_owned());
        Ok((
            Self {
                stream,
                data,
                _worker: worker,
            },
            head,
        ))
    }

    pub fn close(&mut self) {
        let _ = self.stream.shutdown(Shutdown::Both);
    }
}

fn read_http_stream(
    mut reader: TcpStream,
    data: Shared,
    head: Sender<String>,
    stop: Arc<AtomicBool>,
) {
    let mut pending = Vec::new();
    let mut in_body = false;
    let mut chunk = vec![0u8; 65_536];
    while !stop.load(Ordering::SeqCst) {
        let read = match reader.read(&mut chunk) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if would_block(&error) => continue,
            Err(_) => break,
        };
        if in_body {
            lock(&data).push_bytes(&chunk[..read]);
            continue;
        }
        pending.extend_from_slice(&chunk[..read]);
        if let Some(end) = pending.windows(4).position(|window| window == b"\r\n\r\n") {
            let _ = head.send(String::from_utf8_lossy(&pending[..end]).into_owned());
            lock(&data).push_bytes(&pending[end + 4..]);
            in_body = true;
        }
    }
    if !in_body && !pending.is_empty() {
        let _ = head.send(String::from_utf8_lossy(&pending).into_owned());
    }
    lock(&data).closed = true;
}
