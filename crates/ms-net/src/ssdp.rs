//! SSDP / UPnP discovery for SAT>IP servers.
//!
//! Messages match the C++ `ssdp_notify` / `ssdp_reply` format strings; network
//! I/O is behind [`SsdpIo`] so tests can use a fake.

use std::fs::{self, File};
use std::io::{self, Read};
use std::net::{Ipv4Addr, UdpSocket};
use std::path::Path;
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
}

/// Real UDP socket bound to port 1900 and joined to the SSDP group.
#[derive(Debug)]
pub struct UdpSsdpIo {
    socket: UdpSocket,
}

impl UdpSsdpIo {
    /// Opens the discovery socket; `bind` selects the local interface address.
    pub fn open(bind: Option<&str>, disc_host: &str) -> io::Result<Self> {
        let interface = bind
            .and_then(|host| host.parse::<Ipv4Addr>().ok())
            .unwrap_or(Ipv4Addr::UNSPECIFIED);
        let socket = UdpSocket::bind((interface, SSDP_PORT))?;
        if let Ok(group) = disc_host.parse::<Ipv4Addr>() {
            socket.join_multicast_v4(&group, &interface)?;
        }
        Ok(Self { socket })
    }

    /// Local bound address.
    pub fn local_addr(&self) -> io::Result<std::net::SocketAddr> {
        self.socket.local_addr()
    }

    /// Sets the receive timeout, matching the C++ SSDP socket timeout.
    pub fn set_read_timeout(&self, timeout: Option<std::time::Duration>) -> io::Result<()> {
        self.socket.set_read_timeout(timeout)
    }

    /// Receives one datagram and its source address.
    pub fn recv_from(&self, buf: &mut [u8]) -> io::Result<(usize, std::net::SocketAddr)> {
        self.socket.recv_from(buf)
    }
}

impl SsdpIo for UdpSsdpIo {
    fn send_to(&mut self, data: &[u8], host: &str, port: u16) -> io::Result<usize> {
        self.socket.send_to(data, (host, port))
    }
}

/// Discovery state machine; owns the transport and the identity counters.
#[derive(Debug)]
pub struct SsdpServer<I: SsdpIo> {
    io: I,
    config: SsdpConfig,
    announced: bool,
    announce_time_ms: Option<i64>,
}

impl<I: SsdpIo> SsdpServer<I> {
    /// Creates a server that has not announced itself yet.
    pub fn new(io: I, config: SsdpConfig) -> Self {
        Self {
            io,
            config,
            announced: false,
            announce_time_ms: None,
        }
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
            return Ok(action);
        }

        if starts_with_ignoring_case(message, "NOTIFY") {
            let Some(device_id) = header_value(message, "DEVICEID.SES.COM:") else {
                return Ok(action);
            };
            if parse_int_prefix(&device_id, 0) == self.config.device_id {
                let conflict = device_id_conflict(&self.config);
                self.io.send_to(&conflict, peer_host, peer_port)?;
                action.device_id_conflict = true;
            }
            return Ok(action);
        }

        let man = contains_ignoring_case(message, "MAN");
        let discover = contains_ignoring_case(message, "ssdp:discover");
        let didsescom = header_value(message, "DEVICEID.SES.COM:");
        let mut did = didsescom
            .as_deref()
            .map(|value| parse_int_prefix(value, 0))
            .unwrap_or(0);
        let in_window = self
            .announce_time_ms
            .is_some_and(|time| now_ms - time < CONFLICT_WINDOW_MS);
        if man && discover && didsescom.is_some() && in_window && did == self.config.device_id {
            self.config.device_id += 1;
            self.config.bootid += 1;
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
            action.device_id_conflict = true;
        } else {
            did = self.config.device_id;
        }

        if starts_with_ignoring_case(message, "HTTP/1") {
            return Ok(action);
        }

        // C++ replies to every remaining datagram without looking at `ST`
        // (minisatip.cpp:1848-1865).
        let reply = search_reply(&self.config, date, did);
        self.io.send_to(&reply, peer_host, peer_port)?;
        action.replied = true;
        Ok(action)
    }

    fn send_notify(&mut self, alive: bool) -> io::Result<()> {
        for message in notify_messages(&self.config, alive) {
            self.io
                .send_to(&message, &self.config.disc_host, SSDP_PORT)?;
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
    fs::write(&path, format!("{bootid} {device_id} {uuid}"))?;
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
