//! CLI compatibility parser, config file and variables/status API.

mod getopt;
mod options;
mod parse;
mod usage;

pub use getopt::OptError;
pub use ms_log::{ALL_LOG_MODULES, LOG_LEVELS};
pub use options::{
    ADAPTER_BUFFER, AbsoluteSource, AdapterTimeout, CaChannels, CaPin, DVB_CHARSET_AUTO,
    DVB_CHARSET_LATIN1, DVR_BUFFER, DeliverySystem, DelsysConfig, DiseqcConfig, DiseqcMulti,
    DiseqcTiming, DropEncrypted, LnbConfig, MAX_ADAPTERS, MAX_CA_PMT, MAX_CAID, MAX_PMT_PER_CAPMT,
    MAX_SOURCES, Options, SATIP_STR_LEN, SignalForce, SignalMultiplier, SlaveConfig,
    UnicableConfig, UnicableKind,
};
pub use parse::{ParseOutcome, parse_args, parse_dvbapi_opt, parse_sdt_charset_opt};
pub use usage::{BUILT_INFO, USAGE, VERSION_BANNER, built_info, usage_text};
