//! Fake CAM configuration (PLAN §6).

use crate::tags;

/// CAPMT handling quirks seen on real CAMs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Quirks {
    /// `update` is ignored: the CAM keeps what it had.
    pub ignore_update: bool,
    /// `only`/`first` restart every program, even unchanged ones.
    pub reset_on_only: bool,
    /// `update` restarts the program even when its content is unchanged.
    pub restart_on_update: bool,
}

/// When the CAM answers a CA_PMT with CA_PMT_REPLY.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PmtReplyMode {
    Never,
    /// Only for `ca_pmt_cmd_id` query, as EN 50221 requires.
    #[default]
    OnQuery,
    /// For every CA_PMT that changes the program list.
    Always,
}

/// Body of the APPLICATION_INFO reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub application_type: u8,
    pub manufacturer: u16,
    pub manufacturer_code: u16,
    pub menu: String,
}

impl Default for AppInfo {
    fn default() -> Self {
        Self {
            application_type: 0x01,
            manufacturer: 0xFA4E,
            manufacturer_code: 0x0001,
            menu: "Fake CAM".to_owned(),
        }
    }
}

/// Configuration of one fake CAM instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CamConfig {
    /// Instance id, printed in the recorded log (`cam<id>`).
    pub id: u32,
    /// CA system ids reported in CA_INFO.
    pub caids: Vec<u16>,
    /// Resources the CAM offers in its profile and opens sessions to, in order.
    pub resources: Vec<u32>,
    pub app_info: AppInfo,
    /// `response_interval` of DATE_TIME_ENQUIRY in seconds (0: send once).
    pub datetime_interval_s: u8,
    /// Programs (CA_PMT program numbers) descrambled at the same time.
    pub max_services: usize,
    /// Delay between a program starting and its first clear packet.
    pub descramble_latency_ms: u64,
    /// Delay of every TS packet through the DDCI interface.
    pub ts_latency_ms: u64,
    /// CI TS input limit in bit/s; packets above it are dropped.
    pub ci_bandwidth_bps: Option<u64>,
    /// Burst the bandwidth limit tolerates, in milliseconds of traffic.
    pub bandwidth_burst_ms: u64,
    pub quirks: Quirks,
    pub pmt_reply: PmtReplyMode,
    /// Split SPDUs larger than this into T_DATA_MORE chunks.
    pub max_tpdu_data: Option<usize>,
    /// CI+ content control credentials; `None` keeps the CI+ resources inert.
    #[cfg(feature = "ci-plus")]
    pub ciplus: Option<crate::ciplus::CiPlusCam>,
}

impl Default for CamConfig {
    fn default() -> Self {
        Self {
            id: 0,
            caids: vec![0x0B00],
            resources: vec![
                tags::RESOURCE_MANAGER,
                tags::APPLICATION_INFO,
                tags::CA_SUPPORT,
                tags::DATE_TIME,
                tags::MMI,
            ],
            app_info: AppInfo::default(),
            datetime_interval_s: 0,
            max_services: 1,
            descramble_latency_ms: 0,
            ts_latency_ms: 0,
            ci_bandwidth_bps: None,
            bandwidth_burst_ms: 10,
            quirks: Quirks::default(),
            pmt_reply: PmtReplyMode::default(),
            max_tpdu_data: None,
            #[cfg(feature = "ci-plus")]
            ciplus: None,
        }
    }
}
