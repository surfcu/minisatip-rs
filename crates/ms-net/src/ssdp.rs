//! SSDP / UPnP discovery for SAT>IP servers.
//!
//! Messages match the C++ `ssdp_notify` / `ssdp_reply` format strings; network
//! I/O is behind [`SsdpIo`] so tests can use a fake.

use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::os::fd::AsRawFd;
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::rtsp::parse_int_prefix;

/// SSDP multicast address used by C++ (`opts.disc_host` default).
pub const SSDP_MULTICAST: &str = "239.255.255.250";
/// SSDP port.
pub const SSDP_PORT: u16 = 1900;
/// Socket timeout / announcement interval used by C++ (minisatip.cpp:1975).
pub const ANNOUNCE_INTERVAL_MS: i64 = 60_000;
/// Window in which a DEVICEID clash triggers an increment (minisatip.cpp:1833).
pub const CONFLICT_WINDOW_MS: i64 = 15_000;
/// Search target for the SAT>IP server device.
pub const SATIP_SERVER_ST: &str = "urn:ses-com:device:SatIPServer:1";
/// Root device search target.
pub const ROOT_DEVICE_ST: &str = "upnp:rootdevice";

/// Device identity that SSDP and the HTTP endpoints must agree on.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceIdentity {
    /// UUID without the `uuid:` prefix.
    pub uuid: String,
    /// SAT>IP device id.
    pub device_id: i32,
    /// UPnP boot id.
    pub bootid: i32,
}

/// Identity shared between the SSDP thread and the web service.
///
/// C++ keeps these in `opts`, so an SSDP DEVICEID conflict is immediately
/// visible to `/state.json` (minisatip.cpp:1837-1844).
#[derive(Debug, Clone)]
pub struct SharedIdentity(Arc<Mutex<DeviceIdentity>>);

impl SharedIdentity {
    /// Creates a shared identity.
    pub fn new(uuid: String, device_id: i32, bootid: i32) -> Self {
        Self(Arc::new(Mutex::new(DeviceIdentity {
            uuid,
            device_id,
            bootid,
        })))
    }

    /// Copies the current values.
    pub fn snapshot(&self) -> DeviceIdentity {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }

    /// Replaces the current values.
    pub fn set(&self, identity: &DeviceIdentity) {
        *self
            .0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = identity.clone();
    }
}

/// Identity and URL values used in SSDP messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdpConfig {
    /// UUID without the `uuid:` prefix.
    pub uuid: String,
    /// SAT>IP device id.
    pub device_id: i32,
    /// UPnP boot id.
    pub bootid: i32,
    /// Service name in `SERVER` / `USER-AGENT`.
    pub name_app: String,
    /// Version in `SERVER` / `USER-AGENT`.
    pub version: String,
    /// `host:port` in `LOCATION`.
    pub http_host: String,
    /// Device description path.
    pub xml_path: String,
    /// Multicast address in `HOST`.
    pub disc_host: String,
    /// Local address used in conflict messages.
    pub local_ip: String,
    /// Directory of the persisted `bootid` file, or `None` to skip persistence.
    pub cache_dir: Option<std::path::PathBuf>,
    /// Whether SSDP is disabled (`--disable-ssdp`).
    pub disabled: bool,
}

impl Default for SsdpConfig {
    fn default() -> Self {
        Self {
            uuid: String::new(),
            device_id: 0,
            bootid: 0,
            name_app: "minisatip".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
            http_host: "127.0.0.1:8080".to_owned(),
            xml_path: "desc.xml".to_owned(),
            disc_host: SSDP_MULTICAST.to_owned(),
            local_ip: "127.0.0.1".to_owned(),
            cache_dir: None,
            disabled: false,
        }
    }
}

/// Builds the three NOTIFY messages (minisatip.cpp:1720-1736).
pub fn notify_messages(config: &SsdpConfig, alive: bool) -> Vec<Vec<u8>> {
    let root = "::upnp:rootdevice";
    let uuid_nt = format!("::uuid:{}", config.uuid);
    let satip = format!("::{SATIP_SERVER_ST}");
    [root.to_owned(), uuid_nt, satip]
        .iter()
        .enumerate()
        .map(|(index, nt)| {
            let mut out = String::new();
            out.push_str("NOTIFY * HTTP/1.1\r\n");
            out.push_str(&format!("HOST: {}:1900\r\n", config.disc_host));
            if alive {
                out.push_str("CACHE-CONTROL: max-age=1800\r\n");
            }
            out.push_str(&format!(
                "LOCATION: http://{}/{}\r\n",
                config.http_host, config.xml_path
            ));
            out.push_str(&format!("NT: {}\r\n", &nt[2..]));
            out.push_str(&format!(
                "NTS: ssdp:{}\r\n",
                if alive { "alive" } else { "byebye" }
            ));
            if alive {
                out.push_str(&format!(
                    "SERVER: Linux/1.0 UPnP/1.1 {}/{}\r\n",
                    config.name_app, config.version
                ));
            }
            out.push_str(&format!(
                "USN: uuid:{}{}\r\n",
                config.uuid,
                if index == 1 { "" } else { nt.as_str() }
            ));
            out.push_str(&format!("BOOTID.UPNP.ORG: {}\r\n", config.bootid));
            out.push_str("CONFIGID.UPNP.ORG: 0\r\n");
            if alive {
                out.push_str(&format!("DEVICEID.SES.COM: {}", config.device_id));
            }
            out.push_str("\r\n\r\n");
            out.into_bytes()
        })
        .collect()
}

/// Builds an M-SEARCH reply (minisatip.cpp:1764-1774).
pub fn search_reply(config: &SsdpConfig, date: &str, device_id: i32) -> Vec<u8> {
    format!(
        concat!(
            "HTTP/1.1 200 OK\r\n",
            "CACHE-CONTROL: max-age=1800\r\n",
            "DATE: {}\r\n",
            "EXT:\r\n",
            "LOCATION: http://{}/{}\r\n",
            "SERVER: Linux/1.0 UPnP/1.1 {}/{}\r\n",
            "ST: urn:ses-com:device:SatIPServer:1\r\n",
            "USN: uuid:{}::urn:ses-com:device:SatIPServer:1\r\n",
            "BOOTID.UPNP.ORG: {}\r\n",
            "CONFIGID.UPNP.ORG: 0\r\n",
            "DEVICEID.SES.COM: {}\r\n\r\n"
        ),
        date,
        config.http_host,
        config.xml_path,
        config.name_app,
        config.version,
        config.uuid,
        config.bootid,
        device_id
    )
    .into_bytes()
}

/// Builds the DEVICEID conflict M-SEARCH (minisatip.cpp:1775-1780).
pub fn device_id_conflict(config: &SsdpConfig) -> Vec<u8> {
    format!(
        concat!(
            "M-SEARCH * HTTP/1.1\r\n",
            "HOST: {}:1900\r\n",
            "MAN: \"ssdp:discover\"\r\n",
            "ST: urn:ses-com:device:SatIPServer:1\r\n",
            "USER-AGENT: Linux/1.0 UPnP/1.1 {}/{}\r\n",
            "DEVICEID.SES.COM: {}\r\n\r\n"
        ),
        config.local_ip, config.name_app, config.version, config.device_id
    )
    .into_bytes()
}

/// Case-insensitive substring search, like C++ `strcasestr`.
pub fn find_ignoring_case(haystack: &[u8], needle: &str) -> Option<usize> {
    let needle = needle.as_bytes();
    if needle.is_empty() {
        return Some(0);
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Whether `haystack` contains `needle` ignoring ASCII case.
pub fn contains_ignoring_case(haystack: &[u8], needle: &str) -> bool {
    find_ignoring_case(haystack, needle).is_some()
}

/// Extracts and trims the value of the header line starting with `name`.
pub fn header_value(message: &[u8], name: &str) -> Option<String> {
    let needle = name.as_bytes();
    message.split(|byte| *byte == b'\n').find_map(|line| {
        let line = line.strip_suffix(b"\r").unwrap_or(line);
        let start = line.get(..needle.len())?;
        start.eq_ignore_ascii_case(needle).then(|| {
            String::from_utf8_lossy(&line[needle.len()..])
                .trim()
                .to_owned()
        })
    })
}

/// Whether an `ST` value addresses this server.
pub fn st_matches(st: &str, uuid: &str) -> bool {
    let st = st.trim();
    st.eq_ignore_ascii_case("ssdp:all")
        || st.eq_ignore_ascii_case(ROOT_DEVICE_ST)
        || st.eq_ignore_ascii_case(SATIP_SERVER_ST)
        || st
            .get(..5)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("uuid:"))
            && st
                .get(5..)
                .is_some_and(|value| value.eq_ignore_ascii_case(uuid))
}

/// Result of handling one received datagram.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SsdpAction {
    /// A DEVICEID conflict was detected and the local id incremented.
    pub device_id_conflict: bool,
    /// A search reply was sent.
    pub replied: bool,
}

/// Datagram operations used by [`SsdpServer`].
pub trait SsdpIo {
    /// Sends `data` to `host:port`.
    fn send_to(&mut self, data: &[u8], host: &str, port: u16) -> io::Result<usize>;

    /// Descriptor used in the C++ `handle`/`fd` log fields; fakes report 0.
    fn fd(&self) -> i32 {
        0
    }
}

/// The two SSDP sockets C++ creates in `main` (minisatip.cpp:1943-1947).
///
/// `announce`/`reply` is `udp_bind(opts.bind, 1900, ...)`; `discovery` is
/// `udp_bind(opts.disc_host, 1900, 1)`, joined to the multicast group. Every
/// outgoing message uses the first socket, like the global `ssdp` descriptor
/// (minisatip.cpp:1848-1865), while both are polled for datagrams.
#[derive(Debug)]
pub struct UdpSsdpIo {
    socket: UdpSocket,
    discovery: UdpSocket,
}

impl UdpSsdpIo {
    /// Opens both sockets; a bind failure is fatal, as `FAIL(...)` in C++.
    pub fn open(
        bind: Option<&str>,
        disc_host: &str,
        ipv4_only: bool,
        bind_dev: Option<&str>,
        bind_dev_ip: Option<Ipv4Addr>,
    ) -> io::Result<Self> {
        let local = match bind.filter(|host| !host.is_empty()) {
            Some(host) => resolve_bind(host, ipv4_only)?,
            None if ipv4_only => SocketAddr::from((Ipv4Addr::UNSPECIFIED, SSDP_PORT)),
            None => SocketAddr::from((std::net::Ipv6Addr::UNSPECIFIED, SSDP_PORT)),
        };
        let socket = crate::sys::udp_bind_reuse(local, bind_dev)?;

        // `fill_sockaddr(disc_host, 1900, 1)` always resolves IPv4.
        let Ok(group) = disc_host.parse::<Ipv4Addr>() else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("SSDP: cannot resolve {disc_host}"),
            ));
        };
        let discovery = crate::sys::udp_bind_reuse(SocketAddr::from((group, SSDP_PORT)), bind_dev)
            .or_else(|_| {
                // udp_bind falls back to INADDR_ANY for multicast addresses
                // (socketworks.cpp:308-322).
                crate::sys::udp_bind_reuse(
                    SocketAddr::from((Ipv4Addr::UNSPECIFIED, SSDP_PORT)),
                    bind_dev,
                )
            })?;
        let interface = bind_dev_ip.unwrap_or(Ipv4Addr::UNSPECIFIED);
        // The C++ logs membership failures instead of aborting.
        let _ = crate::sys::join_multicast_v4(discovery.as_raw_fd(), group, interface);
        if bind_dev_ip.is_some() {
            let _ = crate::sys::set_multicast_if(discovery.as_raw_fd(), interface);
        }
        discovery.set_nonblocking(true)?;
        Ok(Self { socket, discovery })
    }

    /// Local bound address of the announcing socket.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.socket.local_addr()
    }

    /// Sets the receive timeout, matching the C++ SSDP socket timeout.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    /// Receives one datagram from either socket and its source address.
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, std::net::SocketAddr)> {
        match self.discovery.recv_from(buf) {
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock
                        | io::ErrorKind::TimedOut
                        | io::ErrorKind::Interrupted
                ) => {}
            other => return other,
        }
        self.socket.recv_from(buf)
    }
}

impl SsdpIo for UdpSsdpIo {
    fn send_to(&mut self, data: &[u8], host: &str, port: u16) -> io::Result<usize> {
        self.socket.send_to(data, (host, port))
    }

    /// The announcing socket, used for every C++ `sendto` and log field
    /// (minisatip.cpp:1739, 1803, 1855).
    fn fd(&self) -> i32 {
        self.socket.as_raw_fd()
    }
}

/// Logs a short/failed `sendto` exactly like the three C++ checks
/// (minisatip.cpp:1741-1743, 1819-1822, 1862-1864).
fn log_send_result(prefix: &str, result: &io::Result<usize>, len: usize) {
    let (written, code, text) = match result {
        Ok(written) if *written == len => return,
        // A short write leaves the C++ errno untouched; report 0/Success.
        Ok(written) => {
            let success = io::Error::from_raw_os_error(0);
            (*written as i64, 0, ms_log::strerror(&success))
        }
        Err(error) => (-1, ms_log::errno(error), ms_log::strerror(error)),
    };
    ms_log::log!("{prefix}: wrote {written} out of {len}: error {code}: {text}");
}

/// `fill_sockaddr` with `getaddrinfo`: hostnames resolved with the requested
/// family preference; a literal address keeps its family unless `ipv4_only`.
fn resolve_bind(host: &str, ipv4_only: bool) -> io::Result<SocketAddr> {
    let mut addresses = (host, SSDP_PORT).to_socket_addrs()?;
    let preferred = addresses.find(|addr| !ipv4_only || addr.is_ipv4());
    preferred.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AddrNotAvailable,
            format!("SSDP: cannot resolve {host}"),
        )
    })
}

/// Discovery state machine; owns the transport and the identity counters.
#[derive(Debug)]
pub struct SsdpServer<I: SsdpIo> {
    io: I,
    config: SsdpConfig,
    identity: Option<SharedIdentity>,
    announced: bool,
    announce_time_ms: Option<i64>,
}

impl<I: SsdpIo> SsdpServer<I> {
    /// Creates a server that has not announced itself yet.
    pub fn new(io: I, config: SsdpConfig) -> Self {
        Self {
            io,
            config,
            identity: None,
            announced: false,
            announce_time_ms: None,
        }
    }

    /// Publishes DEVICEID/BOOTID/UUID changes to `identity`.
    pub fn with_identity(mut self, identity: SharedIdentity) -> Self {
        self.identity = Some(identity);
        self
    }

    /// Current identity and URLs.
    pub fn config(&self) -> &SsdpConfig {
        &self.config
    }

    /// Mutable identity and URLs.
    pub fn config_mut(&mut self) -> &mut SsdpConfig {
        &mut self.config
    }

    /// Transport, for tests and shutdown.
    pub fn io_mut(&mut self) -> &mut I {
        &mut self.io
    }

    /// Transport inspection, for tests.
    pub fn io(&self) -> &I {
        &self.io
    }

    /// Sends the alive NOTIFY set once, like C++ `ssdp_discovery`.
    pub fn announce(&mut self, now_ms: i64) -> io::Result<bool> {
        if self.config.disabled || self.announced {
            return Ok(false);
        }
        self.send_notify(true)?;
        self.announced = true;
        self.announce_time_ms = Some(now_ms);
        Ok(true)
    }

    /// Sends the byebye NOTIFY set, like C++ `ssdp_byebye`.
    pub fn byebye(&mut self) -> io::Result<()> {
        if self.config.disabled {
            return Ok(());
        }
        self.send_notify(false)
    }

    /// Handles one datagram from `peer_host:peer_port`.
    pub fn handle(
        &mut self,
        message: &[u8],
        peer_host: &str,
        peer_port: u16,
        date: &str,
        now_ms: i64,
    ) -> io::Result<SsdpAction> {
        let mut action = SsdpAction::default();
        if self.config.disabled {
            return Ok(action);
        }
        self.announce(now_ms)?;

        // C++ uses a whole-buffer search here, so `USN: uuid:...` also matches.
        if find_ignoring_case(message, "uuid:").is_some_and(|start| {
            String::from_utf8_lossy(&message[start + 5..])
                .trim_start()
                .starts_with(&self.config.uuid)
        }) {
            // minisatip.cpp:1794-1796
            ms_log::logl!(
                ms_log::LOG_SSDP,
                "Dropping packet from the same UUID as mine (from {}:{})",
                peer_host,
                peer_port
            );
            return Ok(action);
        }

        // minisatip.cpp:1802-1805 (`LOGM`, DEFAULT_LOG is LOG_SSDP here).
        ms_log::logl!(
            ms_log::LOG_SSDP,
            "Received SSDP packet (handle {}) from {}:{} \n{}",
            self.io.fd(),
            peer_host,
            peer_port,
            String::from_utf8_lossy(message)
        );

        if starts_with_ignoring_case(message, "NOTIFY") {
            // C++ uses strcasestr, so the header need not start a line.
            let Some(start) = find_ignoring_case(message, "DEVICEID.SES.COM:") else {
                return Ok(action);
            };
            let device_id = String::from_utf8_lossy(&message[start + 17..]);
            if parse_int_prefix(device_id.trim_start(), 0) == self.config.device_id {
                let conflict = device_id_conflict(&self.config);
                // minisatip.cpp:1814-1816 (note the double space after ':').
                ms_log::log!(
                    "A new device joined the network with the same Device ID:  {}, asking to change DEVICEID.SES.COM",
                    peer_host
                );
                let result = self.io.send_to(&conflict, peer_host, peer_port);
                log_send_result("incomplete ssdp_reply notify", &result, conflict.len());
                result?;
                action.device_id_conflict = true;
            }
            return Ok(action);
        }

        // minisatip.cpp:1827-1835: strcasestr for all three, and the window is
        // `s->rtime < 15000` where rtime is reset to the *announcement* tick
        // (`s->rtime = s->wtime`), so it is effectively always true.
        let man = contains_ignoring_case(message, "MAN");
        let discover = contains_ignoring_case(message, "ssdp:discover");
        let didsescom = find_ignoring_case(message, "DEVICEID.SES.COM:").map(|start| {
            String::from_utf8_lossy(&message[start + 17..])
                .trim_start()
                .to_owned()
        });
        let mut did = didsescom
            .as_deref()
            .map(|value| parse_int_prefix(value, 0))
            .unwrap_or(0);
        let in_window = self
            .announce_time_ms
            .is_some_and(|time| time < CONFLICT_WINDOW_MS);
        if man && discover && didsescom.is_some() && in_window && did == self.config.device_id {
            self.config.device_id += 1;
            self.config.bootid += 1;
            // minisatip.cpp:1840-1843: logged after the increment and before
            // readBootID().
            ms_log::log!(
                "Device ID conflict, changing our device id to {}, destination SAT>IP server {}",
                self.config.device_id,
                peer_host
            );
            // C++ calls readBootID() here (minisatip.cpp:1844).
            if let Some(cache_dir) = self.config.cache_dir.clone() {
                let mut uuid = self.config.uuid.clone();
                if let Ok((bootid, device_id)) =
                    read_boot_id(&cache_dir, self.config.device_id, &mut uuid)
                {
                    self.config.bootid = bootid;
                    self.config.device_id = device_id;
                    self.config.uuid = uuid;
                }
            }
            // C++ readBootID mutates the globals read by /state.json and /desc.xml.
            if let Some(identity) = &self.identity {
                identity.set(&DeviceIdentity {
                    uuid: self.config.uuid.clone(),
                    device_id: self.config.device_id,
                    bootid: self.config.bootid,
                });
            }
            action.device_id_conflict = true;
        } else {
            did = self.config.device_id;
        }

        // minisatip.cpp:1848-1849: strncmp is case-sensitive here.
        if message.starts_with(b"HTTP/1") {
            ms_log::log_and_return!(
                Ok(action),
                "ssdp_reply: the message is a reply, ignoring...."
            );
        }

        // C++ replies to every remaining datagram without looking at `ST`
        // (minisatip.cpp:1848-1865).
        let reply = search_reply(&self.config, date, did);
        // minisatip.cpp:1855-1858
        ms_log::logl!(
            ms_log::LOG_SSDP,
            "Send Reply SSDP packet (fd: {}) {}:{}, bootid: {} deviceid: {} http: {}\n{}",
            self.io.fd(),
            peer_host,
            peer_port,
            self.config.bootid,
            did,
            self.config.http_host,
            String::from_utf8_lossy(&reply)
        );
        let result = self.io.send_to(&reply, peer_host, peer_port);
        log_send_result("incomplete ssdp_reply", &result, reply.len());
        result?;
        action.replied = true;
        Ok(action)
    }

    fn send_notify(&mut self, alive: bool) -> io::Result<()> {
        // minisatip.cpp:1716-1717 (`__FUNCTION__` is "ssdp_notify").
        ms_log::logl!(
            ms_log::LOG_SSDP,
            "ssdp_notify: bootid: {} deviceid: {} http: {}",
            self.config.bootid,
            self.config.device_id,
            self.config.http_host
        );
        let op = if alive { "Discovery" } else { "ByeBye" };
        for (index, message) in notify_messages(&self.config, alive).into_iter().enumerate() {
            // minisatip.cpp:1738
            ms_log::logl!(
                ms_log::LOG_SSDP,
                "{} packet {}:\n{}",
                op,
                index + 1,
                String::from_utf8_lossy(&message)
            );
            let result = self.io.send_to(&message, &self.config.disc_host, SSDP_PORT);
            log_send_result("incomplete ssdp_discovery", &result, message.len());
            result?;
        }
        Ok(())
    }
}

fn starts_with_ignoring_case(data: &[u8], prefix: &str) -> bool {
    data.get(..prefix.len())
        .is_some_and(|start| start.eq_ignore_ascii_case(prefix.as_bytes()))
}

/// Reads or creates `cache_dir/bootid`, like C++ `readBootID`
/// (minisatip.cpp:2040): increments BOOTID, generates a UUID when empty and
/// keeps DEVICEID at least the stored value. Returns `(bootid, device_id)`.
pub fn read_boot_id(
    cache_dir: &Path,
    mut device_id: i32,
    uuid: &mut String,
) -> io::Result<(i32, i32)> {
    let path = cache_dir.join("bootid");
    let mut bootid = 0;
    let mut local_device_id = 0;
    // minisatip.cpp:2047-2048
    ms_log::log!(
        "Initial values bootid {}, device_id {}, UUID {} (reading new from {})",
        bootid,
        device_id,
        uuid.as_str(),
        path.display()
    );
    if let Ok(contents) = fs::read_to_string(&path) {
        let mut fields = contents.split_whitespace();
        bootid = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        local_device_id = fields.next().and_then(|v| v.parse().ok()).unwrap_or(0);
        if let Some(stored_uuid) = fields.next() {
            *uuid = stored_uuid.to_owned();
        }
    }
    bootid += 1;
    if local_device_id < 1 {
        local_device_id = 1;
    }
    if uuid.is_empty() {
        *uuid = uuid4();
    }
    if device_id < local_device_id {
        device_id = local_device_id;
    }
    let written = fs::write(&path, format!("{bootid} {device_id} {uuid}"));
    // minisatip.cpp:2075-2076 is emitted even when the file cannot be written.
    ms_log::log!(
        "Running with bootid {}, device_id {}, UUID {}",
        bootid,
        device_id,
        uuid.as_str()
    );
    written?;
    Ok((bootid, device_id))
}

/// Generates a random UUID v4 without external crates.
pub fn uuid4() -> String {
    let mut bytes = [0u8; 16];
    let random = File::open("/dev/urandom").and_then(|mut file| file.read_exact(&mut bytes));
    if random.is_err() {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        bytes.copy_from_slice(&nanos.to_le_bytes());
        bytes[..8].copy_from_slice(&(std::process::id() as u64).to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}
