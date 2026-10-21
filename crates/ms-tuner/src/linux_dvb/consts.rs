//! Linux DVB API v5 constants (`linux/dvb/frontend.h`, `dmx.h`) and C++ `dvb.h` values.

// fe_delivery_system_t
pub const SYS_UNDEFINED: i32 = 0;
pub const SYS_DVBC_ANNEX_A: i32 = 1;
pub const SYS_DVBC_ANNEX_B: i32 = 2;
pub const SYS_DVBT: i32 = 3;
pub const SYS_DVBS: i32 = 5;
pub const SYS_DVBS2: i32 = 6;
pub const SYS_ISDBT: i32 = 8;
pub const SYS_ATSC: i32 = 11;
pub const SYS_DVBT2: i32 = 16;
pub const SYS_DVBC2: i32 = 19;

// fe_status_t
pub const FE_HAS_SIGNAL: i32 = 0x01;
pub const FE_HAS_CARRIER: i32 = 0x02;
pub const FE_HAS_LOCK: i32 = 0x10;
pub const FE_TIMEDOUT: i32 = 0x20;

// fe_type_t and fe_caps_t
pub const FE_QPSK: u32 = 0;
pub const FE_QAM: u32 = 1;
pub const FE_OFDM: u32 = 2;
pub const FE_ATSC: u32 = 3;
pub const FE_CAN_QAM_64: u32 = 0x2000;
pub const FE_CAN_QAM_256: u32 = 0x8000;
pub const FE_CAN_QAM_AUTO: u32 = 0x10000;
pub const FE_CAN_8VSB: u32 = 0x20_0000;
pub const FE_CAN_16VSB: u32 = 0x40_0000;
pub const FE_CAN_2G_MODULATION: u32 = 0x1000_0000;

// dtv_property commands
pub const DTV_TUNE: u32 = 1;
pub const DTV_CLEAR: u32 = 2;
pub const DTV_FREQUENCY: u32 = 3;
pub const DTV_MODULATION: u32 = 4;
pub const DTV_BANDWIDTH_HZ: u32 = 5;
pub const DTV_INVERSION: u32 = 6;
pub const DTV_SYMBOL_RATE: u32 = 8;
pub const DTV_INNER_FEC: u32 = 9;
pub const DTV_PILOT: u32 = 12;
pub const DTV_ROLLOFF: u32 = 13;
pub const DTV_DELIVERY_SYSTEM: u32 = 17;
pub const DTV_ISDBT_PARTIAL_RECEPTION: u32 = 18;
pub const DTV_CODE_RATE_HP: u32 = 36;
pub const DTV_CODE_RATE_LP: u32 = 37;
pub const DTV_GUARD_INTERVAL: u32 = 38;
pub const DTV_TRANSMISSION_MODE: u32 = 39;
pub const DTV_HIERARCHY: u32 = 40;
pub const DTV_STREAM_ID: u32 = 42;
pub const DTV_ENUM_DELSYS: u32 = 44;
pub const DTV_STAT_SIGNAL_STRENGTH: u32 = 62;
pub const DTV_STAT_CNR: u32 = 63;
pub const DTV_STAT_ERROR_BLOCK_COUNT: u32 = 68;
pub const DTV_SCRAMBLING_SEQUENCE_INDEX: u32 = 70;

// fecap_scale_params
pub const FE_SCALE_NOT_AVAILABLE: u8 = 0;
pub const FE_SCALE_DECIBEL: u8 = 1;
pub const FE_SCALE_RELATIVE: u8 = 2;
pub const FE_SCALE_COUNTER: u8 = 3;

// Enum defaults used by `ADD_PROP` in dvb.cpp.
pub const FEC_NONE: i32 = 0;
pub const FEC_1_2: i32 = 1;
pub const FEC_2_3: i32 = 2;
pub const FEC_3_4: i32 = 3;
pub const FEC_4_5: i32 = 4;
pub const FEC_5_6: i32 = 5;
pub const FEC_7_8: i32 = 7;
pub const FEC_8_9: i32 = 8;
pub const FEC_AUTO: i32 = 9;
pub const FEC_3_5: i32 = 10;
pub const FEC_9_10: i32 = 11;
pub const QPSK: i32 = 0;
pub const QAM_16: i32 = 1;
pub const QAM_64: i32 = 3;
pub const QAM_256: i32 = 5;
pub const QAM_AUTO: i32 = 6;
pub const PSK_8: i32 = 9;
pub const PILOT_ON: i32 = 0;
pub const PILOT_OFF: i32 = 1;
pub const PILOT_AUTO: i32 = 2;
pub const ROLLOFF_AUTO: i32 = 3;
pub const INVERSION_AUTO: i32 = 2;
pub const GUARD_INTERVAL_AUTO: i32 = 4;
pub const TRANSMISSION_MODE_AUTO: i32 = 2;
pub const HIERARCHY_AUTO: i32 = 4;
pub const PLS_MODE_ROOT: i32 = 0;

// dvb.h frequency limits in kHz.
pub const MIN_FRQ_DVBT: i32 = 174_000;
pub const MAX_FRQ_DVBT: i32 = 858_000;
pub const MIN_FRQ_DVBC: i32 = 51_000;
pub const MAX_FRQ_DVBC: i32 = 860_000;
pub const MIN_FRQ_DVBS: i32 = 950_000;
pub const MAX_FRQ_DVBS: i32 = 2_150_000;

// dvb.h switch types.
pub const SWITCH_UNICABLE: i32 = 1;
pub const SWITCH_JESS: i32 = 2;
pub const SWITCH_SLAVE: i32 = 3;

// adapter.h `force_tuner_signal` bits (`-M`).
pub const TUNER_FORCE_NO: u8 = 0;
pub const TUNER_FORCE_STRENGTH_PERCENT: u8 = 1;
pub const TUNER_FORCE_STRENGTH_DECIBEL: u8 = 2;
pub const TUNER_FORCE_SNR_PERCENT: u8 = 4;
pub const TUNER_FORCE_SNR_DECIBEL: u8 = 8;

/// `MAX_SOURCES` from adapter.h.
pub const MAX_SOURCES: usize = 64;
/// `DVR_BUFFER` default for `-b`.
pub const DVR_BUFFER: i32 = 30 * 1024 * 188;
