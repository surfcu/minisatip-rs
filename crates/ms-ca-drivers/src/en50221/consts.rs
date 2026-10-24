//! TPDU, SPDU, resource and APDU identifiers and host limits, named as in ca.h/ca.cpp.

/// Builds a resource id like `MKRID(class, type, version)`.
pub const fn mkrid(class: u32, ty: u32, version: u32) -> u32 {
    ((class & 0xffff) << 16) | ((ty & 0x3ff) << 6) | (version & 0x3f)
}

pub const MAX_ADAPTERS: usize = 100;
pub const MAX_SESSIONS: usize = 64;
/// CAIDs kept per CAM (`MAX_CAID`, see tests/test_ca.cpp:850).
pub const MAX_CAID: usize = 20;
/// `sizeof(ci_name) - 1`: longest CAM name kept.
pub const CI_NAME_MAX: usize = 127;
/// `sizeof(pin_str) - 1`.
pub const PIN_MAX: usize = 9;
/// Read buffer of `ca_read_tpdu`.
pub const MAX_TPDU: usize = 4096;

pub const CA_RESET_ATTEMPTS: u32 = 3;
pub const CA_READY_POLLS: u32 = 800;
pub const CA_RECONNECT_INTERVAL_MS: i64 = 5000;
pub const CA_MAX_POLL_FAILS: u32 = 3;
pub const CA_CMD_TIMEOUT_MS: i64 = 15000;

pub const T_SB: u8 = 0x80;
pub const T_RCV: u8 = 0x81;
pub const T_CREATE_TC: u8 = 0x82;
pub const T_CTC_REPLY: u8 = 0x83;
pub const T_DELETE_TC: u8 = 0x84;
pub const T_DTC_REPLY: u8 = 0x85;
pub const T_REQUEST_TC: u8 = 0x86;
pub const T_NEW_TC: u8 = 0x87;
pub const T_TC_ERROR: u8 = 0x88;
pub const T_DATA_LAST: u8 = 0xA0;
pub const T_DATA_MORE: u8 = 0xA1;

pub const ST_SESSION_NUMBER: u8 = 0x90;
pub const ST_OPEN_SESSION_REQUEST: u8 = 0x91;
pub const ST_OPEN_SESSION_RESPONSE: u8 = 0x92;
pub const ST_CREATE_SESSION: u8 = 0x93;
pub const ST_CREATE_SESSION_RESPONSE: u8 = 0x94;
pub const ST_CLOSE_SESSION_REQUEST: u8 = 0x95;
pub const ST_CLOSE_SESSION_RESPONSE: u8 = 0x96;

/// Session status: resource non-existent.
pub const SS_NOT_FOUND: u8 = 0xF0;

pub const EN50221_APP_RM_RESOURCEID: u32 = mkrid(1, 1, 1);
pub const EN50221_APP_AI_RESOURCEID: u32 = mkrid(2, 1, 1);
pub const EN50221_APP_CA_RESOURCEID: u32 = mkrid(3, 1, 1);
pub const EN50221_APP_DATETIME_RESOURCEID: u32 = mkrid(36, 1, 1);
pub const EN50221_APP_MMI_RESOURCEID: u32 = mkrid(64, 1, 1);
pub const EN50221_APP_DVB_RESOURCEID: u32 = mkrid(32, 1, 1);
pub const TS101699_APP_RM_RESOURCEID: u32 = mkrid(1, 1, 2);
pub const TS101699_APP_AI_RESOURCEID: u32 = mkrid(2, 1, 2);
pub const CIPLUS_APP_AI_RESOURCEID: u32 = mkrid(2, 1, 3);
pub const CIPLUS_APP_DVB_RESOURCEID: u32 = mkrid(32, 1, 2);
pub const TS103205_APP_DVB_RESOURCEID: u32 = mkrid(32, 1, 3);
pub const TS103205_APP_MMI_RESOURCEID: u32 = mkrid(64, 2, 1);
pub const CIPLUS_APP_CC_RESOURCEID: u32 = mkrid(140, 64, 1);
pub const CIPLUS_APP_CC_RESOURCEID_TWO: u32 = mkrid(140, 64, 2);
pub const TS103205_APP_CC_RESOURCEID_THREE: u32 = mkrid(140, 64, 4);
pub const CIPLUS_APP_LANG_RESOURCEID: u32 = mkrid(141, 64, 1);
pub const CIPLUS_APP_UPGR_RESOURCEID: u32 = mkrid(142, 64, 1);
pub const CIPLUS_APP_OPRF_RESOURCEID: u32 = mkrid(143, 64, 1);
pub const TS103205_APP_OPRF_TWO_RESOURCEID: u32 = mkrid(143, 64, 2);
pub const TS103205_APP_OPRF_THREE_RESOURCEID: u32 = mkrid(143, 64, 3);
pub const TS101699_APP_AMMI_RESOURCEID: u32 = mkrid(65, 1, 1);
pub const CIPLUS_APP_AMMI_RESOURCEID: u32 = mkrid(65, 1, 2);

pub const TAG_PROFILE_ENQUIRY: u32 = 0x9F8010;
pub const TAG_PROFILE: u32 = 0x9F8011;
pub const TAG_PROFILE_CHANGE: u32 = 0x9F8012;
pub const TAG_APP_INFO_ENQUIRY: u32 = 0x9F8020;
pub const CI_DATA_RATE_INFO: u32 = 0x9F8024;
pub const TAG_CA_INFO_ENQUIRY: u32 = 0x9F8030;
pub const TAG_CA_INFO: u32 = 0x9F8031;
pub const TAG_CA_PMT: u32 = 0x9F8032;
pub const TAG_DATE_TIME_ENQUIRY: u32 = 0x9F8440;
pub const TAG_DATE_TIME: u32 = 0x9F8441;
pub const TAG_CLOSE_MMI: u32 = 0x9F8800;
pub const TAG_DISPLAY_CONTROL: u32 = 0x9F8801;
pub const TAG_DISPLAY_REPLY: u32 = 0x9F8802;
pub const TAG_ENQUIRY: u32 = 0x9F8807;
pub const TAG_ANSWER: u32 = 0x9F8808;
pub const TAG_MENU_LAST: u32 = 0x9F8809;
pub const TAG_MENU_MORE: u32 = 0x9F880A;
pub const TAG_MENU_ANSWER: u32 = 0x9F880B;
pub const TAG_LIST_LAST: u32 = 0x9F880C;
pub const TAG_LIST_MORE: u32 = 0x9F880D;

pub const MMI_CLOSE_MMI_CMD_ID_IMMEDIATE: u8 = 0x00;
pub const MMI_ANSW_ID_CANCEL: u8 = 0x00;
pub const MMI_ANSW_ID_ANSWER: u8 = 0x01;
pub const MMI_DISPLAY_REPLY_ID_MMI_MODE_ACK: u8 = 0x01;
pub const MMI_DISPLAY_CONTROL_CMD_ID_SET_MMI_MODE: u8 = 0x01;

pub const CIPLUS_TAG_CC_OPEN_REQ: u32 = 0x9F9001;
pub const CIPLUS_TAG_CC_OPEN_CNF: u32 = 0x9F9002;
pub const CIPLUS_TAG_CC_DATA_REQ: u32 = 0x9F9003;
pub const CIPLUS_TAG_CC_DATA_CNF: u32 = 0x9F9004;
pub const CIPLUS_TAG_CC_SYNC_REQ: u32 = 0x9F9005;
pub const CIPLUS_TAG_CC_SYNC_CNF: u32 = 0x9F9006;
pub const CIPLUS_TAG_CC_SAC_DATA_REQ: u32 = 0x9F9007;
pub const CIPLUS_TAG_CC_SAC_DATA_CNF: u32 = 0x9F9008;
pub const CIPLUS_TAG_CC_SAC_SYNC_REQ: u32 = 0x9F9009;
pub const CIPLUS_TAG_CC_SAC_SYNC_CNF: u32 = 0x9F9010;
pub const CIPLUS_TAG_APP_INFO: u32 = 0x9F8021;
pub const CIPLUS_TAG_CICAM_RESET: u32 = 0x9F8023;
pub const CIPLUS_TAG_COUNTRY_ENQ: u32 = 0x9F8100;
pub const CIPLUS_TAG_COUNTRY_REPLY: u32 = 0x9F8101;
pub const CIPLUS_TAG_LANG_ENQ: u32 = 0x9F8110;
pub const CIPLUS_TAG_LANG_REPLY: u32 = 0x9F8111;
pub const CIPLUS_TAG_FIRMWARE_UPGR: u32 = 0x9F9D01;
pub const CIPLUS_TAG_FIRMWARE_UPGR_REPLY: u32 = 0x9F9D02;
pub const CIPLUS_TAG_FIRMWARE_UPGR_PRGRS: u32 = 0x9F9D03;
pub const CIPLUS_TAG_FIRMWARE_UPGR_COMPLT: u32 = 0x9F9D04;
pub const CIPLUS_TAG_OPERATOR_STATUS: u32 = 0x9F9C01;
pub const CIPLUS_TAG_OPERATOR_INFO_REQ: u32 = 0x9F9C04;
pub const CIPLUS_TAG_OPERATOR_INFO: u32 = 0x9F9C05;
pub const CIPLUS_TAG_OPERATOR_SEARCH_START: u32 = 0x9F9C06;
pub const CIPLUS_TAG_OPERATOR_SEARCH_STATUS: u32 = 0x9F9C07;
pub const CIPLUS_TAG_OPERATOR_TUNE: u32 = 0x9F9C09;
pub const CIPLUS_TAG_OPERATOR_TUNE_STATUS: u32 = 0x9F9C0A;

/// `CIPLUS_DATA_RATE_96_MBPS` as the one byte `ciplus13_app_ai_data_rate_info` sends.
pub const CIPLUS_DATA_RATE_96_MBPS: u8 = 1;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_match_mkrid() {
        // From tests/test_ca.cpp:1762 (AI session resource 0x20041)
        assert_eq!(EN50221_APP_AI_RESOURCEID, 0x0002_0041);
        // From tests/test_ca.cpp:1381 (MKRID(3, 1, 1) is CA support)
        assert_eq!(EN50221_APP_CA_RESOURCEID, 0x0003_0041);
        // From tests/test_ca.cpp:1844 (TS 103 205 MMI is 0x400081)
        assert_eq!(TS103205_APP_MMI_RESOURCEID, 0x0040_0081);
    }
}
