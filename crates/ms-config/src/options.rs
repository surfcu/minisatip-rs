//! Parsed option values, mirroring the C++ `struct_opts` and its per-adapter state.

/// Maximum number of adapters (`MAX_ADAPTERS` in the C++ source).
pub const MAX_ADAPTERS: usize = 100;
/// Maximum number of virtual sources (`MAX_SOURCES` in the C++ source).
pub const MAX_SOURCES: u8 = 64;
/// Default app adapter buffer size (`ADAPTER_BUFFER`).
pub const ADAPTER_BUFFER: i32 = 2000 * 188;
/// Default kernel DVR buffer size (`DVR_BUFFER`).
pub const DVR_BUFFER: i32 = 30 * 1024 * 188;
/// Minimum adapter buffer (`MAX_UDP_PACKET_SIZE`).
pub const MAX_UDP_PACKET_SIZE: i32 = 1316;
/// Capacity of the satip server/xml option strings (`SATIP_STR_LEN`).
pub const SATIP_STR_LEN: usize = 5000;
/// Maximum CAPMTs per CAM (`MAX_CA_PMT`).
pub const MAX_CA_PMT: i32 = 8;
/// Maximum PMTs packed in one CAPMT (`MAX_PMT_PER_CAPMT`).
pub const MAX_PMT_PER_CAPMT: i32 = 3;
/// Maximum forced CAIDs per CAM (`MAX_CAID`).
pub const MAX_CAID: usize = 20;
/// DVB charset value meaning "auto-detect".
pub const DVB_CHARSET_AUTO: i32 = 0;
/// DVB charset value meaning ISO-8859-1.
pub const DVB_CHARSET_LATIN1: i32 = 1;

/// Unicable/JESS switch type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnicableKind {
    /// Unicable (EN 50494).
    Unicable,
    /// JESS (EN 50607).
    Jess,
}

/// One `-u`/`-j` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnicableConfig {
    /// Adapter number (0..99).
    pub adapter: u8,
    /// Switch type selected by the option.
    pub kind: UnicableKind,
    /// User band slot.
    pub slot: i32,
    /// User band frequency in MHz.
    pub freq: i32,
    /// PIN, or -1 when absent.
    pub pin: i32,
    /// Force 13 V during setup (`*` before the frequency).
    pub only_13v: bool,
}

/// One `-d` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiseqcConfig {
    /// Adapter number.
    pub adapter: u8,
    /// Fast switch (only voltage/tone).
    pub fast: bool,
    /// DiSEqC address, normally 0x10.
    pub addr: u8,
    /// Number of committed commands.
    pub committed_no: i32,
    /// Number of uncommitted commands.
    pub uncommitted_no: i32,
}

/// One `-q` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiseqcTiming {
    /// Adapter number.
    pub adapter: u8,
    /// Delay before a command (ms).
    pub before_cmd: i32,
    /// Delay after a command (ms).
    pub after_cmd: i32,
    /// Delay after a repeated command (ms).
    pub after_repeated_cmd: i32,
    /// Delay after a switch (ms).
    pub after_switch: i32,
    /// Delay after a burst (ms).
    pub after_burst: i32,
    /// Delay after a tone change (ms).
    pub after_tone: i32,
}

/// One `-0` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiseqcMulti {
    /// Adapter number.
    pub adapter: u8,
    /// DiSEqC position sent before others.
    pub position: i32,
}

/// One `-L` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LnbConfig {
    /// Adapter number.
    pub adapter: u8,
    /// Low local oscillator frequency (Hz).
    pub low: i32,
    /// High local oscillator frequency (Hz).
    pub high: i32,
    /// Band switch frequency (Hz).
    pub switch: i32,
    /// Circular LNB frequency (Hz), always 0 after an explicit `-L`.
    pub circular: i32,
}

/// One `-S` binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SlaveConfig {
    /// Slave adapter number.
    pub adapter: u8,
    /// Master adapter number.
    pub master: i32,
}

/// One `-Z` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdapterTimeout {
    /// Adapter number.
    pub adapter: u8,
    /// Timeout in milliseconds.
    pub timeout_ms: i32,
}

/// Delivery systems accepted by `-Y`/`--delsys`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(i32)]
pub enum DeliverySystem {
    /// Unknown or invalid name (SYS_UNDEFINED).
    Undefined = 0,
    /// DVB-C annex A.
    Dvbc = 1,
    /// DVB-C annex B.
    Dvbcb = 2,
    /// DVB-T.
    Dvbt = 3,
    /// DSS.
    Dss = 4,
    /// DVB-S.
    Dvbs = 5,
    /// DVB-S2.
    Dvbs2 = 6,
    /// DVB-H.
    Dvbh = 7,
    /// ISDB-T.
    Isdbt = 8,
    /// ISDB-S.
    Isdbs = 9,
    /// ISDB-C.
    Isdbc = 10,
    /// ATSC.
    Atsc = 11,
    /// ATSC-MH.
    Atscmh = 12,
    /// DMB-TH.
    Dmbth = 13,
    /// CMMB.
    Cmmb = 14,
    /// DAB.
    Dab = 15,
    /// DVB-T2.
    Dvbt2 = 16,
    /// Turbo.
    Turbo = 17,
    /// DVB-C annex C.
    Dvbcc = 18,
    /// DVB-C2.
    Dvbc2 = 19,
}

impl DeliverySystem {
    /// Case-insensitive lookup of a `--delsys` name, unknown names map to `Undefined`.
    pub fn from_name(name: &str) -> Self {
        const NAMES: &[(&str, DeliverySystem)] = &[
            ("undefined", DeliverySystem::Undefined),
            ("dvbc", DeliverySystem::Dvbc),
            ("dvbcb", DeliverySystem::Dvbcb),
            ("dvbt", DeliverySystem::Dvbt),
            ("dss", DeliverySystem::Dss),
            ("dvbs", DeliverySystem::Dvbs),
            ("dvbs2", DeliverySystem::Dvbs2),
            ("dvbh", DeliverySystem::Dvbh),
            ("isdbt", DeliverySystem::Isdbt),
            ("isdbs", DeliverySystem::Isdbs),
            ("isdbc", DeliverySystem::Isdbc),
            ("atsc", DeliverySystem::Atsc),
            ("atscmh", DeliverySystem::Atscmh),
            ("dmbth", DeliverySystem::Dmbth),
            ("cmmb", DeliverySystem::Cmmb),
            ("dab", DeliverySystem::Dab),
            ("dvbt2", DeliverySystem::Dvbt2),
            ("turbo", DeliverySystem::Turbo),
            ("dvbcc", DeliverySystem::Dvbcc),
            ("dvbc2", DeliverySystem::Dvbc2),
        ];
        NAMES
            .iter()
            .find(|(n, _)| n.eq_ignore_ascii_case(name))
            .map_or(DeliverySystem::Undefined, |(_, s)| *s)
    }

    /// Secondary system the C++ `set_adapters_delsys` stores in `sys[1]`.
    pub fn secondary(self) -> Self {
        match self {
            DeliverySystem::Dvbs2 => DeliverySystem::Dvbs,
            DeliverySystem::Dvbt2 => DeliverySystem::Dvbt,
            DeliverySystem::Dvbc2 => DeliverySystem::Dvbc,
            _ => DeliverySystem::Undefined,
        }
    }

    /// Canonical name as used by `--delsys`.
    pub fn as_str(self) -> &'static str {
        match self {
            DeliverySystem::Undefined => "undefined",
            DeliverySystem::Dvbc => "dvbc",
            DeliverySystem::Dvbcb => "dvbcb",
            DeliverySystem::Dvbt => "dvbt",
            DeliverySystem::Dss => "dss",
            DeliverySystem::Dvbs => "dvbs",
            DeliverySystem::Dvbs2 => "dvbs2",
            DeliverySystem::Dvbh => "dvbh",
            DeliverySystem::Isdbt => "isdbt",
            DeliverySystem::Isdbs => "isdbs",
            DeliverySystem::Isdbc => "isdbc",
            DeliverySystem::Atsc => "atsc",
            DeliverySystem::Atscmh => "atscmh",
            DeliverySystem::Dmbth => "dmbth",
            DeliverySystem::Cmmb => "cmmb",
            DeliverySystem::Dab => "dab",
            DeliverySystem::Dvbt2 => "dvbt2",
            DeliverySystem::Turbo => "turbo",
            DeliverySystem::Dvbcc => "dvbcc",
            DeliverySystem::Dvbc2 => "dvbc2",
        }
    }
}

/// One `-Y` entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DelsysConfig {
    /// Adapter number.
    pub adapter: u8,
    /// Requested delivery system.
    pub system: DeliverySystem,
    /// Secondary system implied by C++ (`dvbs2`→`dvbs`, `dvbt2`→`dvbt`, `dvbc2`→`dvbc`).
    pub secondary: DeliverySystem,
}

/// Signal format forcing requested with `%`/`#` in `-M`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SignalForce {
    /// Force strength to be reported as a percentage.
    pub strength_percent: bool,
    /// Force strength to be reported in decibels.
    pub strength_decibel: bool,
    /// Force SNR to be reported as a percentage.
    pub snr_percent: bool,
    /// Force SNR to be reported in decibels.
    pub snr_decibel: bool,
}

/// One `-M` entry.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SignalMultiplier {
    /// Adapter number.
    pub adapter: u8,
    /// Strength multiplier.
    pub strength: f32,
    /// SNR multiplier.
    pub snr: f32,
    /// Format forcing flags.
    pub force: SignalForce,
}

/// One `-3` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaPin {
    /// Adapter number.
    pub adapter: u8,
    /// PIN string.
    pub pin: String,
}

/// One `-c` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CaChannels {
    /// Adapter number.
    pub adapter: u8,
    /// Extra PMTs packed per CAPMT (`*` count).
    pub multiple_pmt: i32,
    /// Maximum number of CAPMTs.
    pub max_ca_pmt: i32,
    /// Forced CAIDs, in option order.
    pub caids: Vec<u32>,
}

/// One `-A` mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AbsoluteSource {
    /// First client source position.
    pub src: u8,
    /// Last client source position (inclusive).
    pub src_end: u8,
    /// Internal adapter number.
    pub adapter: u8,
    /// DiSEqC position assigned to `src` (C++ stores `pos + 1`).
    pub position: u8,
}

/// Value of `-E`/`--drop-encrypted`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DropEncrypted {
    /// Forward undecrypted packets (C++ value 0).
    Send = 0,
    /// Drop undecrypted packets (C++ value 1, the default).
    Drop = 1,
    /// Forward undecrypted packets as stuffing TS packets (C++ value 2).
    Stuff = 2,
}

/// All values set by the C++ `set_options`, with the same defaults.
#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// `-r` remote RTP host.
    pub rrtp: Option<String>,
    /// `-I` advertised service name.
    pub name_app: String,
    /// Command line, excluding the program name.
    pub command_line: String,
    /// HTTP host:port, normally derived after parsing.
    pub http_host: Option<String>,
    /// RTSP host:port, normally derived after parsing.
    pub rtsp_host: Option<String>,
    /// `-V` bind address.
    pub bind: Option<String>,
    /// `-U` HTTP bind address.
    pub bind_http: Option<String>,
    /// `-J` bind device.
    pub bind_dev: Option<String>,
    /// Build timestamp (not populated by the parser).
    pub datetime_compile: String,
    /// Process start time (not populated by the parser).
    pub start_time: i64,
    /// Formatted process start time (not populated by the parser).
    pub datetime_start: String,
    /// Current timestamp placeholder (C++ initialises it to a space).
    pub datetime_current: String,
    /// Running time placeholder (C++ initialises it to "0").
    pub time_running: String,
    /// Effective user id (not populated by the parser).
    pub run_user: u32,
    /// Process id (not populated by the parser).
    pub run_pid: u32,
    /// SSDP discovery multicast address.
    pub disc_host: String,
    /// UUID (not populated by the parser).
    pub uuid: String,
    /// `-l`/`-v` log module bitmask.
    pub log: u32,
    /// `-v` debug module bitmask.
    pub debug: u32,
    /// Number of `-g` occurrences.
    pub slog: u32,
    /// `-P` first RTP port.
    pub start_rtp: u32,
    /// `-x` HTTP port.
    pub http_port: u32,
    /// Stream timeout in milliseconds.
    pub timeout_sec: i32,
    /// `-a` simulated DVB-S adapters.
    pub force_sadapter: i32,
    /// `-a` simulated DVB-T adapters.
    pub force_tadapter: i32,
    /// `-a` simulated DVB-C adapters.
    pub force_cadapter: i32,
    /// `-f` clears this; 1 means daemonise.
    pub daemon: i32,
    /// `-D` device id.
    pub device_id: i32,
    /// Boot id (set after parsing in C++).
    pub bootid: i32,
    /// `-b` kernel DVR buffer size.
    pub dvr_buffer: i32,
    /// `--satip-receive-buffer` size in bytes.
    pub satipc_buffer: i32,
    /// `-b` app adapter buffer size.
    pub adapter_buffer: i32,
    /// `--client-send-buffer` size in bytes.
    pub output_buffer: i32,
    /// `-H` UDP write threshold (ms).
    pub udp_threshold: i32,
    /// `-H` TCP write threshold (ms).
    pub tcp_threshold: i32,
    /// Forced channel scan.
    pub force_scan: i32,
    /// `--send-all-ecm`.
    pub send_all_ecm: i32,
    /// Source line in log output.
    pub file_line: i32,
    /// `-o` DVBAPI port.
    pub dvbapi_port: i32,
    /// `-o` DVBAPI host or socket path.
    pub dvbapi_host: String,
    /// `-o` DVBAPI client offset.
    pub dvbapi_offset: i32,
    /// `-E` undecrypted packet handling.
    pub drop_encrypted: DropEncrypted,
    /// `-o ~` disables filtering for `pids=all`.
    pub pids_all_no_dec: i32,
    /// `--sdt-charset` value (`DVB_CHARSET_*`).
    pub sdt_charset: i32,
    /// `-y` RTSP port.
    pub rtsp_port: i32,
    /// `-n` netceiver count.
    pub netcv_count: u8,
    /// `-n` netceiver interface.
    pub netcv_if: Option<String>,
    /// `-p` wrapped playlist XML fragment.
    pub playlist: Option<String>,
    /// `-F` log file.
    pub log_file: String,
    /// `--log-max-size` limit for the `-F` file; 0 disables rotation.
    pub log_max_size: u64,
    /// `--log-remote` TCP syslog sink address.
    pub log_remote: Option<String>,
    /// `--log-remote-queue` queue depth for the remote sink.
    pub log_remote_queue: usize,
    /// `--log-ring` flight-recorder capacity; 0 disables it.
    pub log_ring: usize,
    /// `-4` toggles this; 1 means IPv4 only.
    pub use_ipv4_only: i32,
    /// `-1` demux device mode.
    pub use_demux_device: i32,
    /// `-M` default strength multiplier.
    pub strength_multiplier: f32,
    /// `-M` default SNR multiplier.
    pub snr_multiplier: f32,
    /// Enigma2 mode (default detection is not done by the parser).
    pub enigma: bool,
    /// `--hw-descrambler`.
    pub hw_descrambler: bool,
    /// `-s` satip servers, comma separated.
    pub satip_servers: Option<String>,
    /// `-6` satip XML URLs, space separated.
    pub satip_xml: Option<String>,
    /// `-O` use RTSP over TCP for satip.
    pub satip_rtsp_over_tcp: u8,
    /// `-R` document root.
    pub document_root: String,
    /// `-X` XML path.
    pub xml_path: String,
    /// `-i` thread priority.
    pub th_priority: i32,
    /// `-d *` fast switch flag.
    pub diseqc_fast: i32,
    /// `-d *` DiSEqC address.
    pub diseqc_addr: i32,
    /// `-d *` committed command count.
    pub diseqc_committed_no: i32,
    /// `-d *` uncommitted command count.
    pub diseqc_uncommitted_no: i32,
    /// `-q *` before-command delay.
    pub diseqc_before_cmd: i32,
    /// `-q *` after-command delay.
    pub diseqc_after_cmd: i32,
    /// `-q *` after-repeated-command delay.
    pub diseqc_after_repeated_cmd: i32,
    /// `-q *` after-switch delay.
    pub diseqc_after_switch: i32,
    /// `-q *` after-burst delay.
    pub diseqc_after_burst: i32,
    /// `-q *` after-tone delay.
    pub diseqc_after_tone: i32,
    /// `-0 *` DiSEqC multi position.
    pub diseqc_multi: i32,
    /// `-L *` low LNB frequency (Hz).
    pub lnb_low: i32,
    /// `-L *` high LNB frequency (Hz).
    pub lnb_high: i32,
    /// `-L *` LNB switch frequency (Hz).
    pub lnb_switch: i32,
    /// `-L *` circular LNB frequency (Hz).
    pub lnb_circular: i32,
    /// `-Z *` adapter timeout (ms).
    pub adapter_timeout: i32,
    /// Maximum PIDs per adapter.
    pub max_pids: i32,
    /// `-B` app buffer size (MB).
    pub max_sbuf: i32,
    /// `-N` disable DVB detection.
    pub disable_dvb: bool,
    /// `-G` disable SSDP.
    pub disable_ssdp: bool,
    /// `-9` toggles PMT scanning.
    pub pmt_scan: i32,
    /// `-k` emulate `pids=all`.
    pub emulate_pids_all: bool,
    /// `-z` cache directory.
    pub cache_dir: String,
    /// `--ci-plus-recovery` seconds; 0 disables the key watchdog (CI+ builds only).
    pub ci_plus_recovery: i32,
    /// `--cam-recovery` seconds; 0 disables the scrambled-output watchdog (all builds).
    pub cam_recovery: i32,
    /// `--cam-recovery-no-zap-guard`: a CAPMT change does not clear the arming state.
    pub cam_recovery_no_zap_guard: bool,
    /// `-h`/`--help` was requested; parsing stopped there.
    pub help: bool,
    /// `--version` was requested; parsing stopped there.
    pub version: bool,
    /// Per-adapter `-u`/`-j` state, newest value wins.
    pub unicable: Vec<UnicableConfig>,
    /// Per-adapter `-d` state, newest value wins.
    pub diseqc: Vec<DiseqcConfig>,
    /// Per-adapter `-q` state, newest value wins.
    pub diseqc_timings: Vec<DiseqcTiming>,
    /// Per-adapter `-0` state, newest value wins.
    pub diseqc_multis: Vec<DiseqcMulti>,
    /// Per-adapter `-L` state, newest value wins.
    pub lnb: Vec<LnbConfig>,
    /// `-S` bindings in option order.
    pub slaves: Vec<SlaveConfig>,
    /// Per-adapter `-Z` state, newest value wins.
    pub adapter_timeouts: Vec<AdapterTimeout>,
    /// Per-adapter `-Y` state, newest value wins.
    pub delsys: Vec<DelsysConfig>,
    /// Per-adapter `-M` state, newest value wins.
    pub signal_multipliers: Vec<SignalMultiplier>,
    /// `-e` result: `None` means all adapters, `Some` is the enabled list.
    pub enabled_adapters: Option<Vec<u8>>,
    /// `-5` adapters that must not receive the CAT.
    pub disable_cat: Vec<u8>,
    /// Per-adapter `-3` state, newest value wins.
    pub ca_pins: Vec<CaPin>,
    /// `-C` adapters forced to CI, without duplicates.
    pub force_ci: Vec<u8>,
    /// Per-adapter `-c` state, newest value wins.
    pub ca_channels: Vec<CaChannels>,
    /// `-A` source mappings in option order.
    pub absolute_sources: Vec<AbsoluteSource>,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            rrtp: None,
            name_app: "minisatip".into(),
            command_line: String::new(),
            http_host: None,
            rtsp_host: None,
            bind: None,
            bind_http: None,
            bind_dev: None,
            datetime_compile: String::new(),
            start_time: 0,
            datetime_start: String::new(),
            datetime_current: " ".into(),
            time_running: "0".into(),
            run_user: 0,
            run_pid: 0,
            disc_host: "239.255.255.250".into(),
            uuid: String::new(),
            log: 0,
            debug: 0,
            slog: 0,
            start_rtp: 5500,
            http_port: 8080,
            timeout_sec: 30000,
            force_sadapter: 0,
            force_tadapter: 0,
            force_cadapter: 0,
            daemon: 1,
            device_id: 0,
            bootid: 0,
            dvr_buffer: DVR_BUFFER,
            satipc_buffer: 0,
            adapter_buffer: ADAPTER_BUFFER,
            output_buffer: 0,
            udp_threshold: 25,
            tcp_threshold: 50,
            force_scan: 0,
            send_all_ecm: 0,
            file_line: 0,
            dvbapi_port: 0,
            dvbapi_host: String::new(),
            dvbapi_offset: 0,
            drop_encrypted: DropEncrypted::Drop,
            pids_all_no_dec: 0,
            sdt_charset: DVB_CHARSET_AUTO,
            rtsp_port: 554,
            netcv_count: 0,
            netcv_if: None,
            playlist: None,
            log_file: "/tmp/minisatip.log".into(),
            log_max_size: 0,
            log_remote: None,
            log_remote_queue: 10_000,
            log_ring: 20_000,
            use_ipv4_only: 1,
            use_demux_device: 0,
            strength_multiplier: 1.0,
            snr_multiplier: 1.0,
            enigma: false,
            hw_descrambler: false,
            satip_servers: None,
            satip_xml: None,
            satip_rtsp_over_tcp: 0,
            document_root: "html".into(),
            xml_path: "desc.xml".into(),
            th_priority: -1,
            diseqc_fast: 0,
            diseqc_addr: 0x10,
            diseqc_committed_no: 1,
            diseqc_uncommitted_no: 0,
            diseqc_before_cmd: 15,
            diseqc_after_cmd: 54,
            diseqc_after_repeated_cmd: 15,
            diseqc_after_switch: 15,
            diseqc_after_burst: 15,
            diseqc_after_tone: 0,
            diseqc_multi: -1,
            lnb_low: 9750 * 1000,
            lnb_high: 10600 * 1000,
            lnb_switch: 11700 * 1000,
            lnb_circular: 10750 * 1000,
            adapter_timeout: 30000,
            max_pids: 0,
            max_sbuf: 10,
            disable_dvb: false,
            disable_ssdp: false,
            pmt_scan: 1,
            emulate_pids_all: false,
            cache_dir: "/var/cache/minisatip".into(),
            ci_plus_recovery: 0,
            cam_recovery: 0,
            cam_recovery_no_zap_guard: false,
            help: false,
            version: false,
            unicable: Vec::new(),
            diseqc: Vec::new(),
            diseqc_timings: Vec::new(),
            diseqc_multis: Vec::new(),
            lnb: Vec::new(),
            slaves: Vec::new(),
            adapter_timeouts: Vec::new(),
            delsys: Vec::new(),
            signal_multipliers: Vec::new(),
            enabled_adapters: None,
            disable_cat: Vec::new(),
            ca_pins: Vec::new(),
            force_ci: Vec::new(),
            ca_channels: Vec::new(),
            absolute_sources: Vec::new(),
        }
    }
}
