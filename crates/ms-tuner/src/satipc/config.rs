//! `-s` server list (`find_satip_adapter`) and `--satip-xml` discovery
//! (`satip_getxml_data`, `http_client`).

use std::io::{self, Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::time::{Duration, Instant};

use super::client::Transport;
use super::util::{delsys_code, delsys_from_code, delsys_lookup, delsys_name, parse_int, split};
use crate::tuner::DeliverySystem;

/// `satipc.cpp`'s `#define DEFAULT_LOG LOG_SATIPC`; the `httpc.cpp` lines in
/// this module use an explicit `LOG_HTTP` mask instead.
const DEFAULT_LOG: u32 = ms_log::LOG_SATIPC;

/// `MAX_DVBAPI_SYSTEMS`: size of the per-delivery-system tuner table.
pub const MAX_DVBAPI_SYSTEMS: usize = 22;
/// `sizeof(Ssatip_xml_data::xml)`.
const XML_SIZE: usize = 4000;
/// `http_client` socket timeout.
const HTTP_TIMEOUT: Duration = Duration::from_secs(2);

/// One remote tuner, the arguments of `add_satip_server`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatipServer {
    /// Server host name or address.
    pub host: String,
    /// RTSP port.
    pub port: u16,
    /// Remote frontend (`fe=`), `-1` when unset.
    pub fe: i32,
    /// Delivery system of the adapter (`ad->sys[0]`).
    pub delsys: DeliverySystem,
    /// Local source address, empty when unset.
    pub source_ip: String,
    /// Media transport.
    pub transport: Transport,
    /// `~`: replace `pids=all` with PIDs 0-20.
    pub no_pids_all: bool,
}

impl SatipServer {
    /// `ad->sys[]`: the delivery system plus its first-generation sibling.
    pub fn systems(&self) -> [DeliverySystem; 2] {
        let second = match self.delsys {
            DeliverySystem::DvbS2 => DeliverySystem::DvbS,
            DeliverySystem::DvbT2 => DeliverySystem::DvbT,
            DeliverySystem::DvbC2 => DeliverySystem::DvbC,
            _ => DeliverySystem::Undefined,
        };
        [self.delsys, second]
    }
}

/// `find_satip_adapter`: parses `[^][*][~][DELSYS:][FE_ID@][source_ip/]host[:port],...`.
pub fn parse_satip_servers(spec: &str, rtsp_over_tcp: bool) -> Result<Vec<SatipServer>, String> {
    let mut servers = Vec::new();
    for token in split(spec, ',') {
        let mut token = token;
        let mut transport = if rtsp_over_tcp {
            Transport::Tcp
        } else {
            Transport::Udp
        };
        let mut no_pids_all = false;
        if let Some(rest) = token.strip_prefix('^') {
            #[cfg(not(feature = "srt"))]
            {
                let _ = rest;
                // satipc.cpp:2094 `FAIL`: log then give up (Rust returns Err).
                ms_log::log0!("SRT support is disabled, install libsrt-openssl-dev");
                return Err("SRT support is disabled, install libsrt-openssl-dev".to_owned());
            }
            #[cfg(feature = "srt")]
            {
                transport = Transport::Srt;
                token = rest;
            }
        }
        if let Some(rest) = token.strip_prefix('*') {
            transport = match transport {
                Transport::Tcp => Transport::Udp,
                Transport::Udp => Transport::Tcp,
                #[cfg(feature = "srt")]
                Transport::Srt => Transport::Srt,
            };
            token = rest;
        }
        if let Some(rest) = token.strip_prefix('~') {
            no_pids_all = true;
            token = rest;
        }

        let sep1 = token.find(':');
        let sep2 = sep1.and_then(|first| token[first + 1..].find(':').map(|i| first + 1 + i));
        let first_segment = sep1.map_or(token, |i| &token[..i]);
        let (system_part, host_part, port_part) = if delsys_lookup(first_segment).is_some() {
            let Some(first) = sep1 else {
                // satipc.cpp:2126-2127; only the system was given.
                ms_log::log!("Found only the system for satip adapter {}", first_segment);
                continue;
            };
            match sep2 {
                Some(second) => (
                    first_segment,
                    &token[first + 1..second],
                    &token[second + 1..],
                ),
                None => (first_segment, &token[first + 1..], ""),
            }
        } else {
            match sep1 {
                Some(first) => ("", &token[..first], &token[first + 1..]),
                None => ("", token, ""),
            }
        };
        let system_part = if system_part.is_empty() {
            "dvbs2"
        } else {
            system_part
        };
        let port_part = if port_part.is_empty() {
            "554"
        } else {
            port_part
        };
        let delsys = delsys_lookup(system_part).unwrap_or(DeliverySystem::Undefined);
        let port = u16::try_from(parse_int(port_part, 0)).unwrap_or(0);

        let mut fe = -1;
        let mut host = host_part;
        if let Some(at) = host.find('@') {
            fe = parse_int(&host[..at], 0);
            host = &host[at + 1..];
        }
        let mut source_ip = "";
        if let Some(slash) = host.find('/') {
            source_ip = &host[..slash];
            host = &host[slash + 1..];
        }
        servers.push(SatipServer {
            host: host.to_owned(),
            port,
            fe,
            delsys,
            source_ip: source_ip.to_owned(),
            transport,
            no_pids_all,
        });
    }
    Ok(servers)
}

/// The tuner list advertised by a SAT>IP description (`Ssatip_xml_data`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SatipXml {
    /// Host taken from the description URL.
    pub host: String,
    /// `X-SATIP-RTSP-Port`, 554 by default.
    pub port: u16,
    /// Tuners per `fe_delivery_system_t` value.
    pub tuners: [i32; MAX_DVBAPI_SYSTEMS],
    /// Delivery systems in `X_SATIPCAP` order.
    pub order: Vec<DeliverySystem>,
}

impl SatipXml {
    /// Tuners for `sys`.
    pub fn tuners_for(&self, sys: DeliverySystem) -> i32 {
        usize::try_from(delsys_code(sys))
            .ok()
            .and_then(|i| self.tuners.get(i))
            .copied()
            .unwrap_or(0)
    }

    /// The `add_satip_server` calls made for this description, `fe` counting from 0.
    pub fn servers(&self, rtsp_over_tcp: bool) -> Vec<SatipServer> {
        let mut servers = Vec::new();
        let mut fe = 0;
        for sys in &self.order {
            for _ in 0..self.tuners_for(*sys) {
                servers.push(SatipServer {
                    host: self.host.clone(),
                    port: self.port,
                    fe,
                    delsys: *sys,
                    source_ip: String::new(),
                    transport: if rtsp_over_tcp {
                        Transport::Tcp
                    } else {
                        Transport::Udp
                    },
                    no_pids_all: false,
                });
                fe += 1;
            }
        }
        servers
    }
}

/// `satip_getxml_data(NULL, ...)`: parses the collected response for `host`.
pub fn parse_satip_xml(xml: &str, host: &str) -> Option<SatipXml> {
    let mut result = SatipXml {
        host: host.to_owned(),
        port: 554,
        tuners: [0; MAX_DVBAPI_SYSTEMS],
        order: Vec::new(),
    };
    if let Some(pos) = xml.find("X-SATIP-RTSP-Port:") {
        result.port = u16::try_from(parse_int(&xml[pos + 18..], 554)).unwrap_or(0);
    }
    let start = xml
        .find("<satip:X_SATIPCAP")
        .and_then(|cap| xml[cap..].find('>').map(|gt| cap + gt + 1));
    let Some(start) = start else {
        // satipc.cpp:2223; the XML URL is not available here.
        ms_log::log!("No satipcap found for {}, XML:\n{}", host, xml);
        return None;
    };
    let caps = &xml[start..];
    let caps = caps.find('<').map_or(caps, |end| &caps[..end]);
    for item in split(caps, ',') {
        let (name, count) = match item.find('-') {
            Some(dash) => (&item[..dash], &item[dash + 1..]),
            None => (item, ""),
        };
        let Some(sys) = delsys_lookup(name) else {
            // satipc.cpp:2239-2240
            ms_log::log!("Could not determine the delivery system for {}", item);
            continue;
        };
        let code = delsys_code(sys);
        let tuners = parse_int(count, -1);
        if code < 0
            || code as usize >= MAX_DVBAPI_SYSTEMS
            || tuners < 0
            || result.order.len() >= MAX_DVBAPI_SYSTEMS
        {
            // satipc.cpp:2250-2254
            ms_log::log!(
                "Could not determine the delivery system for {} ({}) tuners {}, order {}",
                item,
                code,
                tuners,
                result.order.len()
            );
            continue;
        }
        result.tuners[code as usize] = tuners;
        result.order.push(delsys_from_code(code));
        // satipc.cpp:2258; the XML URL is not available here.
        ms_log::logm!("{}: {} tuners for {}", delsys_name(sys), tuners, host);
    }
    Some(result)
}

/// `http_client`: `GET` over HTTP/1.0; returns the URL host and the raw response.
pub fn fetch_satip_xml(url: &str) -> io::Result<(String, String)> {
    if !url.starts_with("http") {
        // httpc.cpp:107
        ms_log::log_and_return!(
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("Only http support for {url}")
            )),
            "Only http support for {}",
            url
        );
    }
    let rest = url.get(7..).unwrap_or("");
    let host_end = rest.find([':', '/']).unwrap_or(rest.len());
    let host = &rest[..host_end];
    let port = if rest[host_end..].starts_with(':') {
        u16::try_from(parse_int(&rest[host_end + 1..], 80)).unwrap_or(80)
    } else {
        80
    };
    let request = rest.find('/').map_or("/", |slash| &rest[slash..]);
    let address = (host, port)
        .to_socket_addrs()?
        .next()
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("cannot resolve {host}")))?;
    let mut stream = match TcpStream::connect_timeout(&address, HTTP_TIMEOUT) {
        Ok(stream) => stream,
        // httpc.cpp:131-132
        Err(error) => ms_log::log_and_return!(
            Err(error),
            "http_client: connect to {}:{} failed",
            host,
            port
        ),
    };
    stream.set_read_timeout(Some(HTTP_TIMEOUT))?;
    stream.set_write_timeout(Some(HTTP_TIMEOUT))?;
    // httpc.cpp:144-145; the socketworks s_id is not tracked.
    let fd = stream.as_raw_fd();
    ms_log::logl!(
        ms_log::LOG_HTTP,
        "http_client url {} using handle {} s_id {}",
        url,
        fd,
        0
    );
    // httpc.cpp:88
    ms_log::logl!(
        ms_log::LOG_HTTP,
        "http_client_read: sending to {}: {}",
        fd,
        request
    );
    stream.write_all(format!("GET {request} HTTP/1.0\r\n\r\n").as_bytes())?;
    let deadline = Instant::now() + HTTP_TIMEOUT;
    let mut data = Vec::new();
    let mut chunk = [0u8; 2048];
    while Instant::now() < deadline {
        match stream.read(&mut chunk) {
            Ok(0) => break,
            // A chunk that would overflow `xml[4000]` is dropped, like C++.
            Ok(read) if data.len() + read >= XML_SIZE => {
                // satipc.cpp:2284: drop the chunk and stop, like the returning
                // C++ callback (otherwise every later chunk logs again).
                ms_log::log!("too much data from the satip server for URL {}", url);
                break;
            }
            Ok(read) => {
                data.extend_from_slice(&chunk[..read]);
                // satipc.cpp:2289
                ms_log::debugm!("satip_getxml_data: {}", String::from_utf8_lossy(&data));
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(error) => return Err(error),
        }
    }
    Ok((host.to_owned(), String::from_utf8_lossy(&data).into_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn server_list_forms() {
        let servers = parse_satip_servers(
            "192.168.1.2,dvbt:10.0.0.1:8554,*~dvbc:2@192.168.1.5/10.0.0.9:555,dvbs2",
            false,
        )
        .expect("parse");
        assert_eq!(servers.len(), 3);
        assert_eq!(servers[0].host, "192.168.1.2");
        assert_eq!(servers[0].port, 554);
        assert_eq!(servers[0].delsys, DeliverySystem::DvbS2);
        assert_eq!(servers[0].fe, -1);
        assert_eq!(servers[0].transport, Transport::Udp);
        assert_eq!(servers[1].delsys, DeliverySystem::DvbT);
        assert_eq!(servers[1].port, 8554);
        assert_eq!(servers[2].delsys, DeliverySystem::DvbC);
        assert_eq!(servers[2].fe, 2);
        assert_eq!(servers[2].source_ip, "192.168.1.5");
        assert_eq!(servers[2].host, "10.0.0.9");
        assert_eq!(servers[2].port, 555);
        assert_eq!(servers[2].transport, Transport::Tcp);
        assert!(servers[2].no_pids_all);
    }

    #[test]
    fn rtsp_over_tcp_default_and_toggle() {
        let servers = parse_satip_servers("a,*b", true).expect("parse");
        assert_eq!(servers[0].transport, Transport::Tcp);
        assert_eq!(servers[1].transport, Transport::Udp);
    }

    #[cfg(not(feature = "srt"))]
    #[test]
    fn srt_servers_need_the_srt_feature() {
        assert!(parse_satip_servers("^a", false).is_err());
    }

    #[cfg(feature = "srt")]
    #[test]
    fn srt_servers_ignore_the_tcp_toggle() {
        let servers = parse_satip_servers("^a,^*b:8554,*c", false).expect("parse");
        assert_eq!(servers[0].transport, Transport::Srt);
        assert_eq!(servers[0].host, "a");
        assert_eq!(servers[1].transport, Transport::Srt);
        assert_eq!(servers[1].host, "b");
        assert_eq!(servers[1].port, 8554);
        assert_eq!(servers[2].transport, Transport::Tcp);
    }

    #[test]
    fn xml_servers_count_frontends() {
        let xml = "HTTP/1.0 200 OK\r\nX-SATIP-RTSP-Port: 8554\r\n\r\n<satip:X_SATIPCAP xmlns:satip=\"urn:ses-com:satip\">DVBS2-2,DVBT-1</satip:X_SATIPCAP>";
        let parsed = parse_satip_xml(xml, "1.2.3.4").expect("caps");
        assert_eq!(parsed.port, 8554);
        let servers = parsed.servers(false);
        assert_eq!(servers.len(), 3);
        assert_eq!(servers[2].fe, 2);
        assert_eq!(servers[2].delsys, DeliverySystem::DvbT);
        assert!(parse_satip_xml("<root/>", "h").is_none());
    }
}
