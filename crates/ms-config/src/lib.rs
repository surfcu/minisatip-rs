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
pub use parse::{
    Host, ParseOutcome, parse_args, parse_args_on, parse_dvbapi_opt, parse_sdt_charset_opt,
};
pub use usage::{
    BUILD_DATE, BUILD_DATETIME, BUILD_TIME, BUILT_INFO, S2API_VERSION, USAGE, VERSION_BANNER,
    built_info, usage_text,
};

/// `DVB_API_VERSION` (`linux/dvb/version.h`) of the uapi ms-tuner was ported from.
pub const DVB_API_VERSION: u8 = 5;

/// `DVB_API_VERSION_MINOR` (`linux/dvb/version.h`) of that uapi.
pub const DVB_API_VERSION_MINOR: u8 = 12;
