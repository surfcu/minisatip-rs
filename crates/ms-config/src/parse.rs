//! getopt_long-compatible option parser and the C++ option sub-syntax.

use crate::getopt::{
    OptError, c_atoi, parse_f32_prefix, parse_int, scan_int_pair, scan_int_triple, split_terms,
};
use crate::options::*;
use ms_log::{ALL_LOG_MODULES, LOG_LEVELS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ArgKind {
    None,
    Required,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OptId {
    RemoteRtp,
    DisableDvb,
    DisableSsdp,
    Bind,
    BindHttp,
    BindDev,
    HttpHost,
    HttpPort,
    Log,
    Debug,
    LogFile,
    Help,
    Playlist,
    Adapters,
    Foreground,
    Buffer,
    AppBuffer,
    Threshold,
    EnableAdapters,
    Unicable,
    Jess,
    Diseqc,
    DiseqcTiming,
    Slave,
    Delsys,
    Dvbapi,
    Syslog,
    RtspPort,
    SatipServers,
    Netceiver,
    NameApp,
    Priority,
    SatipTcp,
    DocumentRoot,
    Xml,
    Lnb,
    DropEncrypted,
    UdpPort,
    AdapterTimeout,
    AbsoluteSrc,
    SatipXml,
    DiseqcMulti,
    Multiplier,
    DeviceId,
    DemuxDev,
    Ci,
    CaPin,
    Ipv6,
    NoPidsAll,
    CaChannels,
    CacheDir,
    DisableCat,
    DisablePmtScan,
    Version,
    SendAllEcm,
    SatipReceiveBuffer,
    ClientSendBuffer,
    HwDescrambler,
    SdtCharset,
}

struct OptSpec {
    long: &'static str,
    short: Option<char>,
    kind: ArgKind,
    id: OptId,
}

const OPTIONS: &[OptSpec] = &[
    OptSpec {
        long: "adapters",
        short: Some('a'),
        kind: ArgKind::Required,
        id: OptId::Adapters,
    },
    OptSpec {
        long: "adapter-timeout",
        short: Some('Z'),
        kind: ArgKind::Required,
        id: OptId::AdapterTimeout,
    },
    OptSpec {
        long: "app-buffer",
        short: Some('B'),
        kind: ArgKind::Required,
        id: OptId::AppBuffer,
    },
    OptSpec {
        long: "buffer",
        short: Some('b'),
        kind: ArgKind::Required,
        id: OptId::Buffer,
    },
    OptSpec {
        long: "bind",
        short: Some('V'),
        kind: ArgKind::Required,
        id: OptId::Bind,
    },
    OptSpec {
        long: "bind-http",
        short: Some('U'),
        kind: ArgKind::Required,
        id: OptId::BindHttp,
    },
    OptSpec {
        long: "bind-dev",
        short: Some('J'),
        kind: ArgKind::Required,
        id: OptId::BindDev,
    },
    OptSpec {
        long: "cache-dir",
        short: Some('z'),
        kind: ArgKind::Required,
        id: OptId::CacheDir,
    },
    OptSpec {
        long: "send-all-ecm",
        short: None,
        kind: ArgKind::None,
        id: OptId::SendAllEcm,
    },
    OptSpec {
        long: "client-send-buffer",
        short: None,
        kind: ArgKind::Required,
        id: OptId::ClientSendBuffer,
    },
    OptSpec {
        long: "delsys",
        short: Some('Y'),
        kind: ArgKind::Required,
        id: OptId::Delsys,
    },
    OptSpec {
        long: "debug",
        short: Some('v'),
        kind: ArgKind::Required,
        id: OptId::Debug,
    },
    OptSpec {
        long: "device-id",
        short: Some('D'),
        kind: ArgKind::Required,
        id: OptId::DeviceId,
    },
    OptSpec {
        long: "demux-dev",
        short: Some('1'),
        kind: ArgKind::Required,
        id: OptId::DemuxDev,
    },
    OptSpec {
        long: "disable-dvb",
        short: Some('N'),
        kind: ArgKind::None,
        id: OptId::DisableDvb,
    },
    OptSpec {
        long: "disable-ssdp",
        short: Some('G'),
        kind: ArgKind::None,
        id: OptId::DisableSsdp,
    },
    OptSpec {
        long: "diseqc",
        short: Some('d'),
        kind: ArgKind::Required,
        id: OptId::Diseqc,
    },
    OptSpec {
        long: "diseqc-timing",
        short: Some('q'),
        kind: ArgKind::Required,
        id: OptId::DiseqcTiming,
    },
    OptSpec {
        long: "diseqc-multi",
        short: Some('0'),
        kind: ArgKind::Required,
        id: OptId::DiseqcMulti,
    },
    OptSpec {
        long: "document-root",
        short: Some('R'),
        kind: ArgKind::Required,
        id: OptId::DocumentRoot,
    },
    OptSpec {
        long: "drop-encrypted",
        short: Some('E'),
        kind: ArgKind::None,
        id: OptId::DropEncrypted,
    },
    OptSpec {
        long: "enable-adapters",
        short: Some('e'),
        kind: ArgKind::Required,
        id: OptId::EnableAdapters,
    },
    OptSpec {
        long: "foreground",
        short: Some('f'),
        kind: ArgKind::None,
        id: OptId::Foreground,
    },
    OptSpec {
        long: "name-app",
        short: Some('I'),
        kind: ArgKind::Required,
        id: OptId::NameApp,
    },
    OptSpec {
        long: "http-port",
        short: Some('x'),
        kind: ArgKind::Required,
        id: OptId::HttpPort,
    },
    OptSpec {
        long: "http-host",
        short: Some('w'),
        kind: ArgKind::Required,
        id: OptId::HttpHost,
    },
    OptSpec {
        long: "ipv6",
        short: Some('4'),
        kind: ArgKind::None,
        id: OptId::Ipv6,
    },
    OptSpec {
        long: "jess",
        short: Some('j'),
        kind: ArgKind::Required,
        id: OptId::Jess,
    },
    OptSpec {
        long: "lnb",
        short: Some('L'),
        kind: ArgKind::Required,
        id: OptId::Lnb,
    },
    OptSpec {
        long: "log",
        short: Some('l'),
        kind: ArgKind::Required,
        id: OptId::Log,
    },
    OptSpec {
        long: "logfile",
        short: Some('F'),
        kind: ArgKind::Required,
        id: OptId::LogFile,
    },
    OptSpec {
        long: "multiplier",
        short: Some('M'),
        kind: ArgKind::Required,
        id: OptId::Multiplier,
    },
    OptSpec {
        long: "no-all-pids",
        short: Some('k'),
        kind: ArgKind::None,
        id: OptId::NoPidsAll,
    },
    OptSpec {
        long: "priority",
        short: Some('i'),
        kind: ArgKind::Required,
        id: OptId::Priority,
    },
    OptSpec {
        long: "playlist",
        short: Some('p'),
        kind: ArgKind::Required,
        id: OptId::Playlist,
    },
    OptSpec {
        long: "remote-rtp",
        short: Some('r'),
        kind: ArgKind::Required,
        id: OptId::RemoteRtp,
    },
    OptSpec {
        long: "rtsp-port",
        short: Some('y'),
        kind: ArgKind::Required,
        id: OptId::RtspPort,
    },
    OptSpec {
        long: "sdt-charset",
        short: None,
        kind: ArgKind::Required,
        id: OptId::SdtCharset,
    },
    OptSpec {
        long: "syslog",
        short: Some('g'),
        kind: ArgKind::None,
        id: OptId::Syslog,
    },
    OptSpec {
        long: "slave",
        short: Some('S'),
        kind: ArgKind::Required,
        id: OptId::Slave,
    },
    OptSpec {
        long: "threshold",
        short: Some('H'),
        kind: ArgKind::Required,
        id: OptId::Threshold,
    },
    OptSpec {
        long: "udp",
        short: Some('P'),
        kind: ArgKind::Required,
        id: OptId::UdpPort,
    },
    OptSpec {
        long: "unicable",
        short: Some('u'),
        kind: ArgKind::Required,
        id: OptId::Unicable,
    },
    OptSpec {
        long: "xml",
        short: Some('X'),
        kind: ArgKind::Required,
        id: OptId::Xml,
    },
    OptSpec {
        long: "help",
        short: Some('h'),
        kind: ArgKind::None,
        id: OptId::Help,
    },
    OptSpec {
        long: "virtual-diseqc",
        short: Some('A'),
        kind: ArgKind::Required,
        id: OptId::AbsoluteSrc,
    },
    OptSpec {
        long: "version",
        short: None,
        kind: ArgKind::None,
        id: OptId::Version,
    },
    OptSpec {
        long: "disable-pmt-scan",
        short: Some('9'),
        kind: ArgKind::None,
        id: OptId::DisablePmtScan,
    },
    OptSpec {
        long: "disable-cat",
        short: Some('5'),
        kind: ArgKind::Required,
        id: OptId::DisableCat,
    },
    OptSpec {
        long: "dvbapi",
        short: Some('o'),
        kind: ArgKind::Required,
        id: OptId::Dvbapi,
    },
    OptSpec {
        long: "satip-servers",
        short: Some('s'),
        kind: ArgKind::Required,
        id: OptId::SatipServers,
    },
    OptSpec {
        long: "satip-tcp",
        short: Some('O'),
        kind: ArgKind::None,
        id: OptId::SatipTcp,
    },
    OptSpec {
        long: "satip-xml",
        short: Some('6'),
        kind: ArgKind::Required,
        id: OptId::SatipXml,
    },
    OptSpec {
        long: "satip-receive-buffer",
        short: None,
        kind: ArgKind::Required,
        id: OptId::SatipReceiveBuffer,
    },
    OptSpec {
        long: "netceiver",
        short: Some('n'),
        kind: ArgKind::Required,
        id: OptId::Netceiver,
    },
    OptSpec {
        long: "ca-pin",
        short: Some('3'),
        kind: ArgKind::Required,
        id: OptId::CaPin,
    },
    OptSpec {
        long: "ci",
        short: Some('C'),
        kind: ArgKind::Required,
        id: OptId::Ci,
    },
    OptSpec {
        long: "ca-channels",
        short: Some('c'),
        kind: ArgKind::Required,
        id: OptId::CaChannels,
    },
    OptSpec {
        long: "hw-descrambler",
        short: None,
        kind: ArgKind::None,
        id: OptId::HwDescrambler,
    },
];

const PLAYLIST_PREFIX: &str = "<satip:X_SATIPM3U xmlns:satip=\"urn:ses-com:satip\">";
const PLAYLIST_SUFFIX: &str = "</satip:X_SATIPM3U>\r\n";

struct Parser<'a> {
    args: &'a [String],
    opts: Options,
    warnings: Vec<String>,
    is_log: bool,
    disabled: [bool; MAX_ADAPTERS],
    allocated: [bool; MAX_ADAPTERS],
    slave_master: [i32; MAX_ADAPTERS],
}

/// The options of this build: without NETCVCLIENT the C++ `long_options` table,
/// and the short options built from it, have no `netceiver` (minisatip.cpp:206).
fn compiled_options() -> impl Iterator<Item = &'static OptSpec> {
    OPTIONS
        .iter()
        .filter(|spec| cfg!(feature = "netceiver") || !matches!(spec.id, OptId::Netceiver))
}

/// Result of [`parse_args`]: the parsed options, the getopt diagnostics that
/// glibc would print to stderr, and a fatal error when the C++ binary exits.
#[derive(Debug, Clone, PartialEq)]
pub struct ParseOutcome {
    /// Options applied before parsing stopped. Valid when `error` is `None`.
    pub options: Options,
    /// getopt diagnostics in command-line order, without the `program: ` prefix.
    pub warnings: Vec<String>,
    /// Set when the C++ `set_options` would call `exit(1)`; parsing stops there.
    pub error: Option<OptError>,
}

impl ParseOutcome {
    /// Returns the options, or the fatal error that stopped parsing.
    pub fn into_options(self) -> Result<Options, OptError> {
        match self.error {
            Some(error) => Err(error),
            None => Ok(self.options),
        }
    }
}

enum LongMatch<'a> {
    Found(&'static OptSpec, Option<&'a str>),
    Ambiguous(String),
    Unknown,
}

/// Parses program arguments (without the executable name) like the C++ `set_options`.
///
/// Unknown options, ambiguous prefixes, missing required arguments and
/// arguments given to no-argument options are reported as warnings and parsing
/// continues, as in the C++ binary (its switch has no `default` case, so getopt
/// diagnostics are printed and the loop keeps going). Invalid values that make
/// the C++ code call `exit(1)` stop parsing and are returned in
/// [`ParseOutcome::error`].
pub fn parse_args(args: &[String]) -> ParseOutcome {
    parse_args_on(args, Host::default())
}

/// What `set_options` learns from the host before it reads argv.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Host {
    /// `/etc/enigma2/settings` exists (minisatip.cpp:631).
    pub enigma: bool,
    /// `/proc/stb/frontend/0/fbc_connect` is writable (minisatip.cpp:1172).
    pub fbc: bool,
}

/// [`parse_args`] on a host probed by the caller: an enigma2 box starts from
/// its defaults (minisatip.cpp:661-665, 688-690), which argv can override.
pub fn parse_args_on(args: &[String], host: Host) -> ParseOutcome {
    let mut opts = Options::default();
    if host.enigma {
        opts.enigma = true;
        // /var/cache is a tmpfs on enigma2, so the cache must persist elsewhere.
        opts.cache_dir = "/var/lib/minisatip".into();
        opts.use_demux_device = 3;
        opts.emulate_pids_all = true;
    }
    let mut parser = Parser {
        args,
        opts,
        warnings: Vec::new(),
        is_log: false,
        disabled: [false; MAX_ADAPTERS],
        allocated: [false; MAX_ADAPTERS],
        slave_master: [-1; MAX_ADAPTERS],
    };
    parser.opts.log = 1;
    let error = match parser.run() {
        // The C++ binary exits inside the getopt loop for -h/-v, before the
        // post-loop bind resolution.
        Ok(()) if !parser.opts.help && !parser.opts.version => {
            let error = parser.finish().err();
            // FBC setup ends set_options; adapters bound by -S keep their master.
            if error.is_none() && host.fbc {
                parser.parse_slave("2-7:0");
            }
            error
        }
        Ok(()) => None,
        Err(error) => Some(error),
    };
    ParseOutcome {
        options: parser.opts,
        warnings: parser.warnings,
        error,
    }
}

impl Parser<'_> {
    fn run(&mut self) -> Result<(), OptError> {
        let mut i = 0;
        while i < self.args.len() {
            let token = &self.args[i];
            if token == "--" {
                break;
            }
            if token == "-" || !token.starts_with('-') {
                i += 1;
                continue;
            }
            if let Some(body) = token.strip_prefix("--") {
                match self.lookup_long(body, "--") {
                    LongMatch::Found(spec, inline) => {
                        i = self.apply_spec(spec, inline, "--", i)?;
                    }
                    LongMatch::Ambiguous(warning) => {
                        self.warnings.push(warning);
                        i += 1;
                    }
                    LongMatch::Unknown => {
                        self.warnings
                            .push(format!("unrecognized option '--{body}'"));
                        i += 1;
                    }
                }
            } else {
                let rest = &token[1..];
                let first = rest.chars().next().unwrap_or('\0');
                // getopt_long_only: a single-dash word is a long option when
                // it has more than one character, or when its first character
                // is not a short option.
                let long_only = rest.len() > 1 || self.lookup_short(first).is_none();
                if long_only {
                    match self.lookup_long(rest, "-") {
                        LongMatch::Found(spec, inline) => {
                            i = self.apply_spec(spec, inline, "-", i)?;
                        }
                        LongMatch::Ambiguous(warning) => {
                            self.warnings.push(warning);
                            i += 1;
                        }
                        LongMatch::Unknown if self.lookup_short(first).is_some() => {
                            i = self.apply_short_cluster(rest, i)?;
                        }
                        LongMatch::Unknown => {
                            self.warnings.push(format!("unrecognized option '-{rest}'"));
                            i += 1;
                        }
                    }
                } else {
                    i = self.apply_short_cluster(rest, i)?;
                }
            }
            if self.opts.help || self.opts.version {
                break;
            }
        }
        Ok(())
    }

    /// Resolves a long option name the way glibc does. `dash` is the prefix as
    /// written (`-` or `--`), used by diagnostic messages.
    fn lookup_long<'a>(&self, text: &'a str, dash: &str) -> LongMatch<'a> {
        let (name, inline) = split_name(text);
        if let Some(spec) = compiled_options().find(|spec| spec.long == name) {
            return LongMatch::Found(spec, inline);
        }
        let candidates: Vec<&'static OptSpec> = compiled_options()
            .filter(|spec| spec.long.starts_with(name))
            .collect();
        match candidates.len() {
            0 => LongMatch::Unknown,
            1 => LongMatch::Found(candidates[0], inline),
            _ => {
                let mut warning = format!("option '{dash}{text}' is ambiguous; possibilities:");
                for candidate in candidates {
                    warning.push_str(&format!(" '{dash}{}'", candidate.long));
                }
                LongMatch::Ambiguous(warning)
            }
        }
    }

    fn lookup_short(&self, c: char) -> Option<&'static OptSpec> {
        compiled_options().find(|spec| spec.short == Some(c))
    }

    fn apply_spec(
        &mut self,
        spec: &OptSpec,
        inline: Option<&str>,
        dash: &str,
        index: usize,
    ) -> Result<usize, OptError> {
        let shown = format!("{dash}{}", spec.long);
        match spec.kind {
            ArgKind::None => {
                if inline.is_some() {
                    self.warnings
                        .push(format!("option '{shown}' doesn't allow an argument"));
                    return Ok(index + 1);
                }
                self.apply(spec.id, None)?;
                Ok(index + 1)
            }
            ArgKind::Required => {
                if let Some(value) = inline {
                    self.apply(spec.id, Some(value))?;
                    return Ok(index + 1);
                }
                if index + 1 >= self.args.len() {
                    self.warnings
                        .push(format!("option '{shown}' requires an argument"));
                    return Ok(index + 1);
                }
                self.apply(spec.id, Some(&self.args[index + 1]))?;
                Ok(index + 2)
            }
        }
    }

    fn apply_short_cluster(&mut self, rest: &str, index: usize) -> Result<usize, OptError> {
        for (offset, c) in rest.char_indices() {
            let Some(spec) = self.lookup_short(c) else {
                self.warnings.push(format!("invalid option -- '{c}'"));
                continue;
            };
            match spec.kind {
                ArgKind::None => self.apply(spec.id, None)?,
                ArgKind::Required => {
                    let after = &rest[offset + c.len_utf8()..];
                    let consumed = if after.is_empty() && index + 1 >= self.args.len() {
                        self.warnings
                            .push(format!("option requires an argument -- '{c}'"));
                        1
                    } else {
                        let value = if after.is_empty() {
                            self.args[index + 1].clone()
                        } else {
                            after.to_string()
                        };
                        self.apply(spec.id, Some(&value))?;
                        if after.is_empty() { 2 } else { 1 }
                    };
                    return Ok(index + consumed);
                }
            }
            if self.opts.help || self.opts.version {
                return Ok(index + 1);
            }
        }
        Ok(index + 1)
    }

    fn apply(&mut self, id: OptId, value: Option<&str>) -> Result<(), OptError> {
        let value = value.unwrap_or("");
        match id {
            OptId::Foreground => self.opts.daemon = 0,
            OptId::RemoteRtp => self.opts.rrtp = Some(value.into()),
            OptId::DisableDvb => self.opts.disable_dvb = true,
            OptId::DisableSsdp => self.opts.disable_ssdp = true,
            OptId::DeviceId => self.opts.device_id = c_atoi(value),
            OptId::NameApp => self.opts.name_app = value.into(),
            OptId::HttpHost => self.opts.http_host = Some(value.into()),
            OptId::Bind => self.opts.bind = Some(value.into()),
            OptId::BindHttp => self.opts.bind_http = Some(value.into()),
            OptId::BindDev => {
                if !valid_bind_dev(value) {
                    // C++ FAIL("Invalid --bind-dev value"): exits 1 without usage.
                    return Err(OptError::InvalidValue {
                        option: "--bind-dev".into(),
                        message: "Invalid --bind-dev value".into(),
                        show_usage: false,
                    });
                }
                self.opts.bind_dev = Some(value.into());
            }
            OptId::Log | OptId::Debug => {
                let debug = id == OptId::Debug;
                self.is_log = true;
                if value.contains("all") {
                    if debug {
                        self.opts.debug |= ALL_LOG_MODULES;
                    }
                    self.opts.log |= ALL_LOG_MODULES;
                } else {
                    for term in split_terms(value, ',') {
                        let level = if term == "-l" {
                            // minisatip.cpp:769-772.
                            ms_log::log!("The LOG option has changed, please run ./minisatip -h");
                            Some(1)
                        } else {
                            match log_level(term) {
                                Some(level) => Some(level),
                                None => {
                                    // minisatip.cpp:773-776.
                                    ms_log::log!("module {} not recognised", term);
                                    None
                                }
                            }
                        };
                        if let Some(level) = level {
                            let bit = 1u32 << level;
                            if debug {
                                self.opts.debug |= bit;
                            }
                            self.opts.log |= bit;
                        }
                    }
                }
            }
            OptId::Syslog => self.opts.slog += 1,
            OptId::LogFile => self.opts.log_file = value.into(),
            OptId::Help => self.opts.help = true,
            OptId::Version => self.opts.version = true,
            OptId::HttpPort => self.opts.http_port = c_atoi(value) as u32,
            OptId::UdpPort => self.opts.start_rtp = c_atoi(value) as u32,
            OptId::Ipv6 => {
                self.opts.use_ipv4_only = 1 - self.opts.use_ipv4_only;
                if self.opts.use_ipv4_only == 0 {
                    // minisatip.cpp:819-820.
                    ms_log::log0!("IPv6 mode is enabled");
                }
            }
            OptId::Buffer => {
                let (adapter, dvr) = scan_int_pair(value);
                if let Some(adapter) = adapter {
                    self.opts.adapter_buffer = adapter;
                }
                if let Some(dvr) = dvr {
                    self.opts.dvr_buffer = dvr;
                }
                self.opts.adapter_buffer = (self.opts.adapter_buffer / 188) * 188;
                if self.opts.adapter_buffer < MAX_UDP_PACKET_SIZE {
                    self.opts.adapter_buffer = MAX_UDP_PACKET_SIZE;
                }
                if self.opts.dvr_buffer == 0 {
                    self.opts.dvr_buffer = DVR_BUFFER;
                }
            }
            OptId::AppBuffer => self.opts.max_sbuf = c_atoi(value),
            OptId::ClientSendBuffer => {
                self.opts.output_buffer = c_atoi(value).wrapping_mul(1024);
            }
            OptId::Threshold => {
                let (udp, tcp) = scan_int_pair(value);
                if let Some(udp) = udp {
                    self.opts.udp_threshold = udp;
                }
                if let Some(tcp) = tcp {
                    self.opts.tcp_threshold = tcp;
                }
                self.opts.udp_threshold = self.opts.udp_threshold.clamp(0, 200);
                self.opts.tcp_threshold = self.opts.tcp_threshold.clamp(0, 200);
            }
            OptId::Adapters => {
                let (sat, ter, cable) = scan_int_triple(value);
                if let Some(v) = sat {
                    self.opts.force_sadapter = v;
                }
                if let Some(v) = ter {
                    self.opts.force_tadapter = v;
                }
                if let Some(v) = cable {
                    self.opts.force_cadapter = v;
                }
            }
            OptId::SendAllEcm => self.opts.send_all_ecm = 1,
            OptId::NoPidsAll => self.opts.emulate_pids_all = true,
            OptId::Playlist => {
                if value.len() < 1000 {
                    self.opts.playlist = Some(format!("{PLAYLIST_PREFIX}{value}{PLAYLIST_SUFFIX}"));
                } else {
                    // minisatip.cpp:887.
                    ms_log::log!("playlist length is too big {} bytes", value.len());
                }
            }
            OptId::EnableAdapters => self.parse_enable_adapters(value),
            OptId::Unicable => self.parse_unicable(value, UnicableKind::Unicable),
            OptId::Jess => self.parse_unicable(value, UnicableKind::Jess),
            OptId::Diseqc => self.parse_diseqc(value),
            OptId::DiseqcTiming => self.parse_diseqc_timing(value),
            OptId::DiseqcMulti => self.parse_diseqc_multi(value),
            OptId::Lnb => self.parse_lnb(value),
            OptId::Slave => self.parse_slave(value),
            OptId::DisableCat => self.parse_disable_cat(value),
            OptId::AdapterTimeout => self.parse_adapter_timeout(value),
            OptId::Delsys => self.parse_delsys(value),
            OptId::Multiplier => self.parse_signal_multiplier(value),
            OptId::Dvbapi => parse_dvbapi_opt(value, &mut self.opts),
            OptId::RtspPort => self.opts.rtsp_port = c_atoi(value),
            OptId::SatipServers => self.append_satip(value, ',', true),
            OptId::SatipXml => self.append_satip(value, ' ', false),
            OptId::SatipTcp => self.opts.satip_rtsp_over_tcp = 1,
            OptId::SatipReceiveBuffer => {
                self.opts.satipc_buffer = c_atoi(value).wrapping_mul(1024);
            }
            OptId::Netceiver => self.parse_netceiver(value),
            OptId::Priority => self.opts.th_priority = parse_int(value, 0),
            OptId::DocumentRoot => self.opts.document_root = value.into(),
            OptId::CacheDir => self.opts.cache_dir = value.into(),
            OptId::DropEncrypted => {
                self.opts.drop_encrypted = match self.opts.drop_encrypted {
                    DropEncrypted::Drop => DropEncrypted::Send,
                    DropEncrypted::Send => DropEncrypted::Stuff,
                    DropEncrypted::Stuff => DropEncrypted::Drop,
                };
            }
            OptId::Xml => {
                let stripped = value.trim_start_matches('/');
                if !stripped.is_empty() {
                    self.opts.xml_path = stripped.into();
                } else {
                    // minisatip.cpp:1070.
                    ms_log::log!("Not a valid path for the xml file");
                }
            }
            OptId::DemuxDev => {
                let mode = c_atoi(value);
                if (0..4).contains(&mode) {
                    self.opts.use_demux_device = mode;
                } else {
                    // minisatip.cpp:1078.
                    ms_log::log!("Demux device can be 0-3 and not {}", mode);
                }
            }
            OptId::AbsoluteSrc => self.parse_absolute_sources(value),
            OptId::DisablePmtScan => self.opts.pmt_scan = 1 - self.opts.pmt_scan,
            OptId::CaPin => self.parse_ca_pins(value),
            OptId::Ci => self.parse_force_ci(value),
            OptId::CaChannels => self.parse_ca_channels(value),
            OptId::HwDescrambler => self.opts.hw_descrambler = true,
            OptId::SdtCharset => {
                let charset = parse_sdt_charset_opt(value);
                if charset < 0 {
                    // C++ logs the message, calls usage(), then exit(1).
                    return Err(OptError::InvalidValue {
                        option: "--sdt-charset".into(),
                        message: "Invalid --sdt-charset value, expected auto or latin1".into(),
                        show_usage: true,
                    });
                }
                self.opts.sdt_charset = charset;
            }
        }
        Ok(())
    }

    fn mark_allocated(&mut self, adapter: i32) {
        if adapter_in_range(adapter) {
            self.allocated[adapter as usize] = true;
        }
    }

    fn parse_enable_adapters(&mut self, value: &str) {
        self.disabled = [true; MAX_ADAPTERS];
        for token in split_terms(value, ',') {
            let (start, end) = match token.find('-') {
                Some(dash) => (
                    parse_int(&token[..dash], -1),
                    parse_int(&token[dash + 1..], -1),
                ),
                None => {
                    let adapter = parse_int(token, -1);
                    (adapter, adapter)
                }
            };
            for adapter in range_bounds(start, end) {
                self.disabled[adapter as usize] = false;
            }
        }
        self.opts.enabled_adapters = Some(
            (0..MAX_ADAPTERS)
                .filter(|&adapter| !self.disabled[adapter])
                .map(|adapter| adapter as u8)
                .collect(),
        );
    }

    fn parse_unicable(&mut self, value: &str, kind: UnicableKind) {
        for token in split_terms(value, ',') {
            let adapter = parse_int(token, -1);
            if !adapter_in_range(adapter) {
                continue;
            }
            self.mark_allocated(adapter);
            let Some(colon) = token.find(':') else {
                continue;
            };
            let Some(mut dash) = token.find('-') else {
                continue;
            };
            let sub_dash = &token[dash..];
            let only_13v = sub_dash.len() > 1 && sub_dash.as_bytes()[1] == b'*';
            if only_13v {
                dash += 1;
            }
            let slot = parse_int(&token[colon + 1..], -1);
            let freq = parse_int(&token[dash + 1..], -1);
            if slot < 0 || freq < 0 {
                continue;
            }
            let pin = token[dash + 1..]
                .find('-')
                .map_or(-1, |next| parse_int(&token[dash + 1 + next + 1..], -1));
            upsert_by(
                &mut self.opts.unicable,
                adapter as u8,
                UnicableConfig {
                    adapter: adapter as u8,
                    kind,
                    slot,
                    freq,
                    pin,
                    only_13v,
                },
                |entry| entry.adapter,
            );
            ms_log::log!(
                "Setting {} adapter {} slot {} freq {} pin {}",
                match kind {
                    UnicableKind::Unicable => "unicable",
                    UnicableKind::Jess => "jess",
                },
                adapter,
                slot,
                freq,
                pin
            );
        }
    }

    fn parse_diseqc(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let wildcard = token.starts_with('*');
            let adapter = if wildcard {
                None
            } else {
                let adapter = parse_int(token, -1);
                if !adapter_in_range(adapter) {
                    continue;
                }
                self.mark_allocated(adapter);
                Some(adapter as u8)
            };
            let Some(colon) = token.find(':') else {
                continue;
            };
            let Some(dash) = token.find('-') else {
                continue;
            };
            let mut offset = colon + 1;
            let mut fast = false;
            let mut addr = 0x10;
            while offset < token.len() {
                match token.as_bytes()[offset] {
                    b'*' => fast = true,
                    b'@' => addr = 0,
                    b'.' => addr = 0x11,
                    _ => break,
                }
                offset += 1;
            }
            let committed_no = parse_int(&token[offset..], -1);
            let uncommitted_no = parse_int(&token[dash + 1..], -1);
            if committed_no < 0 || uncommitted_no < 0 {
                continue;
            }
            if let Some(adapter) = adapter {
                upsert_by(
                    &mut self.opts.diseqc,
                    adapter,
                    DiseqcConfig {
                        adapter,
                        fast,
                        addr,
                        committed_no,
                        uncommitted_no,
                    },
                    |entry| entry.adapter,
                );
            } else {
                self.opts.diseqc_fast = i32::from(fast);
                self.opts.diseqc_addr = i32::from(addr);
                self.opts.diseqc_committed_no = committed_no;
                self.opts.diseqc_uncommitted_no = uncommitted_no;
                for entry in &mut self.opts.diseqc {
                    entry.fast = fast;
                    entry.addr = addr;
                    entry.committed_no = committed_no;
                    entry.uncommitted_no = uncommitted_no;
                }
            }
            ms_log::log!(
                "Setting diseqc adapter {} fast {} addr 0x{:02x} committed_no {} uncommitted_no {}",
                adapter.map_or(-1, i32::from),
                i32::from(fast),
                addr,
                committed_no,
                uncommitted_no
            );
        }
    }

    fn parse_diseqc_timing(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let wildcard = token.starts_with('*');
            let adapter = if wildcard {
                None
            } else {
                let adapter = parse_int(token, -1);
                if !adapter_in_range(adapter) {
                    continue;
                }
                self.mark_allocated(adapter);
                Some(adapter as u8)
            };
            let Some(colon) = token.find(':') else {
                continue;
            };
            let dashes: Vec<usize> = token.match_indices('-').map(|(pos, _)| pos).collect();
            if dashes.len() < 5 {
                continue;
            }
            let before_cmd = parse_int(&token[colon + 1..], -1);
            let after_cmd = parse_int(&token[dashes[0] + 1..], -1);
            let after_repeated_cmd = parse_int(&token[dashes[1] + 1..], -1);
            let after_switch = parse_int(&token[dashes[2] + 1..], -1);
            let after_burst = parse_int(&token[dashes[3] + 1..], -1);
            let after_tone = parse_int(&token[dashes[4] + 1..], -1);
            if [
                before_cmd,
                after_cmd,
                after_repeated_cmd,
                after_switch,
                after_burst,
                after_tone,
            ]
            .iter()
            .any(|&v| v < 0)
            {
                continue;
            }
            if let Some(adapter) = adapter {
                upsert_by(
                    &mut self.opts.diseqc_timings,
                    adapter,
                    DiseqcTiming {
                        adapter,
                        before_cmd,
                        after_cmd,
                        after_repeated_cmd,
                        after_switch,
                        after_burst,
                        after_tone,
                    },
                    |entry| entry.adapter,
                );
            } else {
                self.opts.diseqc_before_cmd = before_cmd;
                self.opts.diseqc_after_cmd = after_cmd;
                self.opts.diseqc_after_repeated_cmd = after_repeated_cmd;
                self.opts.diseqc_after_switch = after_switch;
                self.opts.diseqc_after_burst = after_burst;
                self.opts.diseqc_after_tone = after_tone;
                for entry in &mut self.opts.diseqc_timings {
                    entry.before_cmd = before_cmd;
                    entry.after_cmd = after_cmd;
                    entry.after_repeated_cmd = after_repeated_cmd;
                    entry.after_switch = after_switch;
                    entry.after_burst = after_burst;
                    entry.after_tone = after_tone;
                }
            }
            ms_log::log!(
                "Setting diseqc timing for adapter {} before cmd {} after cmd {} after repeated cmd {} after switch {} after burst {} after tone {}",
                adapter.map_or(-1, i32::from),
                before_cmd,
                after_cmd,
                after_repeated_cmd,
                after_switch,
                after_burst,
                after_tone
            );
        }
    }

    fn parse_diseqc_multi(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let wildcard = token.starts_with('*');
            let adapter = if wildcard {
                None
            } else {
                let adapter = parse_int(token, -1);
                if !adapter_in_range(adapter) {
                    continue;
                }
                self.mark_allocated(adapter);
                Some(adapter as u8)
            };
            let Some(colon) = token.find(':') else {
                continue;
            };
            let position = parse_int(&token[colon + 1..], -1);
            if position < 0 {
                continue;
            }
            if let Some(adapter) = adapter {
                upsert_by(
                    &mut self.opts.diseqc_multis,
                    adapter,
                    DiseqcMulti { adapter, position },
                    |entry| entry.adapter,
                );
            } else {
                self.opts.diseqc_multi = position;
                for entry in &mut self.opts.diseqc_multis {
                    entry.position = position;
                }
            }
            ms_log::log!(
                "Setting diseqc multi adapter {} position {}",
                adapter.map_or(-1, i32::from),
                position
            );
        }
    }

    fn parse_lnb(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let wildcard = token.starts_with('*');
            let adapter = if wildcard {
                None
            } else {
                let adapter = parse_int(token, -1);
                if !adapter_in_range(adapter) {
                    continue;
                }
                self.mark_allocated(adapter);
                Some(adapter as u8)
            };
            let dash1 = token.find('-');
            let dash2 = dash1.and_then(|first| {
                token[first + 1..]
                    .find('-')
                    .map(|position| first + 1 + position)
            });
            let (Some(colon), Some(dash1), Some(dash2)) = (token.find(':'), dash1, dash2) else {
                ms_log::log!("LNB parameters not correctly specified: {}", token);
                continue;
            };
            let low = parse_int(&token[colon + 1..], -1).saturating_mul(1000);
            let high = parse_int(&token[dash1 + 1..], -1).saturating_mul(1000);
            let switch = parse_int(&token[dash2 + 1..], -1).saturating_mul(1000);
            if low < 0 || high < 0 || switch < 0 {
                continue;
            }
            if let Some(adapter) = adapter {
                upsert_by(
                    &mut self.opts.lnb,
                    adapter,
                    LnbConfig {
                        adapter,
                        low,
                        high,
                        switch,
                        circular: 0,
                    },
                    |entry| entry.adapter,
                );
            } else {
                self.opts.lnb_low = low;
                self.opts.lnb_high = high;
                self.opts.lnb_switch = switch;
                self.opts.lnb_circular = 0;
                for entry in &mut self.opts.lnb {
                    entry.low = low;
                    entry.high = high;
                    entry.switch = switch;
                    entry.circular = 0;
                }
            }
            ms_log::log!(
                "Setting diseqc adapter {} lnb_low {} lnb_high {} lnb_switch {}",
                adapter.map_or(-1, i32::from),
                low,
                high,
                switch
            );
        }
    }

    fn parse_slave(&mut self, value: &str) {
        let mut master = 0i32;
        for token in split_terms(value, ',') {
            let adapter = parse_int(token, -1);
            if !adapter_in_range(adapter) {
                continue;
            }
            let adapter_end = token
                .find('-')
                .map_or(adapter, |dash| parse_int(&token[dash + 1..], -1));
            if !adapter_in_range(adapter_end) {
                continue;
            }
            if let Some(colon) = token.find(':') {
                master = parse_int(&token[colon + 1..], 0);
            }
            for current in adapter..=adapter_end {
                self.mark_allocated(current);
                if self.slave_master[current as usize] != -1 {
                    continue;
                }
                let master_available = adapter_in_range(master)
                    && self.slave_master[master as usize] < 0
                    && (self.allocated[master as usize] || !self.disabled[master as usize]);
                if master_available {
                    self.mark_allocated(master);
                    self.slave_master[master as usize] = -2;
                    self.slave_master[current as usize] = master;
                    self.opts.slaves.push(SlaveConfig {
                        adapter: current as u8,
                        master,
                    });
                    ms_log::log!("Setting master adapter {} for adapter {}", master, current);
                }
            }
        }
    }

    fn parse_disable_cat(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let (start, end) = adapter_range(token);
            for adapter in range_bounds(start, end) {
                if !self.opts.disable_cat.contains(&(adapter as u8)) {
                    // `ddci.cpp` `disable_cat_adapters`.
                    ms_log::log!("Disable passing the CAT to DDCI {}", adapter);
                    self.opts.disable_cat.push(adapter as u8);
                }
            }
        }
    }

    fn parse_adapter_timeout(&mut self, value: &str) {
        let mut timeout = self.opts.adapter_timeout / 1000;
        if let Some(colon) = value.find(':') {
            timeout = parse_int(&value[colon + 1..], timeout);
        }
        let terms = split_terms(value, ',');
        if terms.first().is_some_and(|term| term.starts_with('*')) {
            let timeout_ms = timeout.saturating_mul(1000);
            self.opts.adapter_timeout = timeout_ms;
            for entry in &mut self.opts.adapter_timeouts {
                entry.timeout_ms = timeout_ms;
            }
            ms_log::log!("Set default timeout to {}", timeout_ms);
            return;
        }
        for token in terms {
            let adapter = parse_int(token, -1);
            if !adapter_in_range(adapter) {
                continue;
            }
            let adapter_end = token
                .find('-')
                .map_or(adapter, |dash| parse_int(&token[dash + 1..], -1));
            if !adapter_in_range(adapter_end) {
                continue;
            }
            let timeout_ms = timeout.saturating_mul(1000);
            for current in adapter..=adapter_end {
                self.mark_allocated(current);
                upsert_by(
                    &mut self.opts.adapter_timeouts,
                    current as u8,
                    AdapterTimeout {
                        adapter: current as u8,
                        timeout_ms,
                    },
                    |entry| entry.adapter,
                );
                ms_log::log!("Set timeout for adapter {} to {}", current, timeout_ms);
            }
        }
    }

    fn parse_delsys(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let adapter = parse_int(token, -1);
            if !adapter_in_range(adapter) {
                continue;
            }
            let Some(colon) = token.find(':') else {
                ms_log::log!(
                    "Delivery system is missing, the format is adapter_number:delivery_system\n example: 2:dvbs2"
                );
                return;
            };
            self.mark_allocated(adapter);
            let system = DeliverySystem::from_name(&token[colon + 1..]);
            upsert_by(
                &mut self.opts.delsys,
                adapter as u8,
                DelsysConfig {
                    adapter: adapter as u8,
                    system,
                    secondary: system.secondary(),
                },
                |entry| entry.adapter,
            );
            ms_log::log!(
                "Setting delivery system for adapter {} to {} and {}",
                adapter,
                system.as_str(),
                system.secondary().as_str()
            );
        }
    }

    fn parse_signal_multiplier(&mut self, value: &str) {
        let mut force = SignalForce::default();
        for token in split_terms(value, ',') {
            let wildcard = token.starts_with('*');
            let adapter = if wildcard {
                None
            } else {
                let adapter = parse_int(token, -1);
                if !adapter_in_range(adapter) {
                    continue;
                }
                self.mark_allocated(adapter);
                Some(adapter as u8)
            };
            let Some(colon) = token.find(':') else {
                continue;
            };
            let Some(dash) = token.find('-') else {
                continue;
            };
            let strength_text = &token[colon + 1..];
            let snr_text = &token[dash + 1..];
            let strength = match strength_text.as_bytes().first() {
                Some(b'%') => {
                    force.strength_percent = true;
                    parse_f32_prefix(&strength_text[1..])
                }
                Some(b'#') => {
                    force.strength_decibel = true;
                    parse_f32_prefix(&strength_text[1..])
                }
                _ => parse_f32_prefix(strength_text),
            };
            let snr = match snr_text.as_bytes().first() {
                Some(b'%') => {
                    force.snr_percent = true;
                    parse_f32_prefix(&snr_text[1..])
                }
                Some(b'#') => {
                    force.snr_decibel = true;
                    parse_f32_prefix(&snr_text[1..])
                }
                _ => parse_f32_prefix(snr_text),
            };
            if strength < 0.0 || snr < 0.0 {
                continue;
            }
            if let Some(adapter) = adapter {
                upsert_by(
                    &mut self.opts.signal_multipliers,
                    adapter,
                    SignalMultiplier {
                        adapter,
                        strength,
                        snr,
                        force,
                    },
                    |entry| entry.adapter,
                );
            } else {
                self.opts.strength_multiplier = strength;
                self.opts.snr_multiplier = snr;
                for entry in &mut self.opts.signal_multipliers {
                    entry.strength = strength;
                    entry.snr = snr;
                }
            }
            let mut force_text = String::new();
            if force.strength_percent
                || force.strength_decibel
                || force.snr_percent
                || force.snr_decibel
            {
                force_text.push_str(" (forcing driver:");
                if force.strength_percent {
                    force_text.push_str(" strength_in_percentage");
                }
                if force.strength_decibel {
                    force_text.push_str(" strength_in_decibels");
                }
                if force.snr_percent {
                    force_text.push_str(" snr_in_percentage");
                }
                if force.snr_decibel {
                    force_text.push_str(" snr_in_decibels");
                }
                force_text.push(')');
            }
            ms_log::log!(
                "Setting signal multipler for adapter {}{} strength_multiplier {:.2} snr_multiplier {:.2}",
                adapter.map_or(-1, i32::from),
                force_text,
                strength,
                snr
            );
        }
    }

    fn parse_absolute_sources(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let Some(colon1) = token.find(':') else {
                continue;
            };
            let Some(colon2) = token[colon1 + 1..].find(':').map(|p| colon1 + 1 + p) else {
                continue;
            };
            let src = parse_int(token, -1);
            let adapter = parse_int(&token[colon1 + 1..], -1);
            let position = parse_int(&token[colon2 + 1..], -1);
            let mut src_end = src;
            if let Some(dash) = token.find('-')
                && dash < colon1
            {
                src_end = parse_int(&token[dash + 1..], -1);
            }
            if !(0..i32::from(MAX_SOURCES)).contains(&src)
                || !(0..i32::from(MAX_SOURCES)).contains(&src_end)
                || !(0..15).contains(&position)
                || !adapter_in_range(adapter)
            {
                continue;
            }
            self.opts.absolute_sources.push(AbsoluteSource {
                src: src as u8,
                src_end: src_end as u8,
                adapter: adapter as u8,
                position: (position + 1) as u8,
            });
            if src <= src_end {
                for value in src..=src_end {
                    ms_log::log!(
                        "Setting source {} (src={}) to adapter {} position {}",
                        value,
                        value + 1,
                        adapter,
                        position + (value - src)
                    );
                }
                self.mark_allocated(adapter);
            }
        }
    }

    fn append_satip(&mut self, value: &str, sep: char, comma_join: bool) {
        let target = if comma_join {
            &mut self.opts.satip_servers
        } else {
            &mut self.opts.satip_xml
        };
        let current = target.get_or_insert_with(String::new);
        if current.len() + value.len() > SATIP_STR_LEN {
            return;
        }
        if !current.is_empty() {
            current.push(sep);
        }
        current.push_str(value);
    }

    fn parse_netceiver(&mut self, value: &str) {
        if let Some(colon) = value.find(':') {
            self.opts.netcv_if = Some(value[..colon].into());
            self.opts.netcv_count = parse_int(&value[colon + 1..], 0) as u8;
        } else {
            self.opts.netcv_if = Some("vlan4".into());
            self.opts.netcv_count = parse_int(value, 0) as u8;
        }
    }

    fn parse_ca_pins(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let Some(colon) = token.find(':') else {
                continue;
            };
            let (start, end) = adapter_range(token);
            let pin = truncate_bytes(&token[colon + 1..], 9).to_string();
            for adapter in range_bounds(start, end) {
                upsert_by(
                    &mut self.opts.ca_pins,
                    adapter as u8,
                    CaPin {
                        adapter: adapter as u8,
                        pin: pin.clone(),
                    },
                    |entry| entry.adapter,
                );
                // C++ logs the untruncated option value, not the stored PIN.
                ms_log::log!("Setting CA {} pin to {}", adapter, &token[colon + 1..]);
            }
        }
    }

    fn parse_force_ci(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let (start, end) = adapter_range(token);
            for adapter in range_bounds(start, end) {
                if !self.opts.force_ci.contains(&(adapter as u8)) {
                    self.opts.force_ci.push(adapter as u8);
                }
                ms_log::log!("Forcing CA {} to CI", adapter);
            }
        }
    }

    fn parse_ca_channels(&mut self, value: &str) {
        for token in split_terms(value, ',') {
            let Some(colon) = token.find(':') else {
                continue;
            };
            let mut rem = &token[colon + 1..];
            let mut multiple_pmt = 0;
            while let Some(stripped) = rem.strip_prefix('*') {
                multiple_pmt += 1;
                rem = stripped;
            }
            multiple_pmt = multiple_pmt.min(MAX_PMT_PER_CAPMT - 1);
            let max_ca_pmt = parse_int(rem, 0).min(MAX_CA_PMT);
            let adapter = parse_int(token, -1);
            if !adapter_in_range(adapter) {
                continue;
            }
            ms_log::log!(
                "Forcing CA {} to use maximum {} CAPMTs with {} PMTs each",
                adapter,
                max_ca_pmt,
                multiple_pmt + 1
            );
            let mut caids = Vec::new();
            let mut search = 0;
            while let Some(offset) = rem[search..].find('-') {
                let segment = &rem[search + offset + 1..];
                if let Some(caid) = parse_hex_prefix(segment) {
                    if caids.len() < MAX_CAID {
                        caids.push(caid);
                        if caid > 0 {
                            ms_log::log!("Forcing CA {} to use CAID {:04X}", adapter, caid);
                        }
                    } else if caid > 0 {
                        ms_log::log!(
                            "CA {}: reached maximum CAID limit ({}), ignoring CAID {:04X}",
                            adapter,
                            MAX_CAID,
                            caid
                        );
                    }
                }
                search += offset + 1;
            }
            upsert_by(
                &mut self.opts.ca_channels,
                adapter as u8,
                CaChannels {
                    adapter: adapter as u8,
                    multiple_pmt,
                    max_ca_pmt,
                    caids,
                },
                |entry| entry.adapter,
            );
        }
    }

    fn finish(&mut self) -> Result<(), OptError> {
        if !self.is_log {
            self.opts.log = 0;
        }
        let bind = self.opts.bind.take().filter(|value| !value.is_empty());
        let bind_http = self.opts.bind_http.take().filter(|value| !value.is_empty());
        if self.opts.bind_dev.is_some() && (bind.is_some() || bind_http.is_some()) {
            // C++ resolve_bind_opts() returns non-zero (socketworks.cpp:153)
            // and set_options calls FAIL("Invalid bind configuration"):
            // exits 1 without usage (minisatip.cpp:1122-1123).
            ms_log::log!("--bind-dev cannot be combined with --bind or --bind-http");
            return Err(OptError::InvalidValue {
                option: "--bind-dev".into(),
                message: "Invalid bind configuration".into(),
                show_usage: false,
            });
        }
        let bind_http = if self.opts.bind_dev.is_none() && bind.is_some() && bind_http.is_none() {
            bind.clone()
        } else {
            bind_http
        };
        self.opts.bind = bind;
        self.opts.bind_http = bind_http;
        self.opts.command_line = self.args.join(" ");
        Ok(())
    }
}

fn split_name(text: &str) -> (&str, Option<&str>) {
    match text.split_once('=') {
        Some((name, value)) => (name, Some(value)),
        None => (text, None),
    }
}

fn log_level(term: &str) -> Option<usize> {
    let term = term.trim_start_matches([' ', '\t', '\r', '\n', '\x0b', '\x0c']);
    if term.is_empty() {
        // utils.cpp:136 (check_strs, driven by minisatip.cpp:767)
        ms_log::log!("check_strs: s is empty");
        return None;
    }
    let mut found = None;
    for (index, name) in LOG_LEVELS.iter().enumerate() {
        if name
            .get(..term.len())
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case(term))
        {
            found = Some(index);
        }
    }
    found
}

fn adapter_in_range(adapter: i32) -> bool {
    (0..MAX_ADAPTERS as i32).contains(&adapter)
}

fn range_bounds(start: i32, end: i32) -> std::ops::RangeInclusive<i32> {
    start.max(0)..=end.min(MAX_ADAPTERS as i32 - 1)
}

/// Replaces the entry for `adapter`, or appends it when the adapter is new.
fn upsert_by<T>(list: &mut Vec<T>, adapter: u8, entry: T, adapter_of: impl Fn(&T) -> u8) {
    match list.iter_mut().find(|item| adapter_of(item) == adapter) {
        Some(existing) => *existing = entry,
        None => list.push(entry),
    }
}

fn adapter_range(token: &str) -> (i32, i32) {
    match token.find('-') {
        Some(dash) => (
            parse_int(&token[..dash], 0),
            parse_int(&token[dash + 1..], 0),
        ),
        None => {
            let value = parse_int(token, 0);
            (value, value)
        }
    }
}

fn parse_hex_prefix(text: &str) -> Option<u32> {
    let mut value: u64 = 0;
    let mut digits = 0usize;
    for byte in text.bytes() {
        let digit = match byte {
            b'0'..=b'9' => u64::from(byte - b'0'),
            b'a'..=b'f' => u64::from(byte - b'a' + 10),
            b'A'..=b'F' => u64::from(byte - b'A' + 10),
            _ => break,
        };
        value = value.checked_mul(16)?.checked_add(digit)?;
        digits += 1;
    }
    if digits == 0 || value == 0 || value > u64::from(u32::MAX) {
        return None;
    }
    Some(value as u32)
}

fn valid_bind_dev(value: &str) -> bool {
    !value.is_empty()
        && value.len() < 16
        && !value
            .chars()
            .any(|c| matches!(c, '/' | ' ' | '\t' | '\r' | '\n'))
}

/// Parses `-o`/`--dvbapi` into `opts`, like the C++ `parse_dvbapi_opt`.
pub fn parse_dvbapi_opt(optarg: &str, opts: &mut Options) {
    let mut buf = truncate_bytes(optarg, 99).to_string();
    if buf.starts_with('~') {
        opts.pids_all_no_dec = 1;
        buf.remove(0);
    }
    if let Some(comma) = buf.find(',') {
        opts.dvbapi_offset = parse_int(&buf[comma + 1..], 0);
        buf.truncate(comma);
    }
    if let Some(colon) = buf.find(':') {
        opts.dvbapi_host = buf[..colon].to_string();
        opts.dvbapi_port = parse_int(&buf[colon + 1..], 0);
    } else {
        opts.dvbapi_host = buf;
        opts.dvbapi_port = 9000;
    }

    if opts.dvbapi_host.starts_with('/') {
        // opts.cpp:56
        ms_log::log!("Using DVBAPI socket at {}", opts.dvbapi_host);
    } else {
        // opts.cpp:58
        ms_log::log!(
            "Using DVBAPI server at {}:{}, offset {}",
            opts.dvbapi_host,
            opts.dvbapi_port,
            opts.dvbapi_offset
        );
    }

    if opts.pids_all_no_dec != 0 {
        // opts.cpp:63
        ms_log::log!("Not filtering out encrypted packets from pids=all streams");
    }
}

/// Parses `--sdt-charset`: returns `DVB_CHARSET_AUTO`, `DVB_CHARSET_LATIN1` or -1.
pub fn parse_sdt_charset_opt(optarg: &str) -> i32 {
    match optarg {
        "auto" => DVB_CHARSET_AUTO,
        "latin1" | "iso-8859-1" | "iso8859-1" => DVB_CHARSET_LATIN1,
        _ => -1,
    }
}

fn truncate_bytes(text: &str, max: usize) -> &str {
    if text.len() <= max {
        return text;
    }
    let mut end = max;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}
