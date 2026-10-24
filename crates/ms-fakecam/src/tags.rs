//! TPDU, SPDU, resource and APDU identifiers, named as in ca.h.

/// Builds a resource id like `MKRID(class, type, version)`.
pub const fn mkrid(class: u32, ty: u32, version: u32) -> u32 {
    (class << 16) | (ty << 6) | version
}

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

pub const RESOURCE_MANAGER: u32 = mkrid(1, 1, 1);
pub const APPLICATION_INFO: u32 = mkrid(2, 1, 1);
pub const CA_SUPPORT: u32 = mkrid(3, 1, 1);
pub const HOST_CONTROL: u32 = mkrid(32, 1, 1);
pub const DATE_TIME: u32 = mkrid(36, 1, 1);
pub const MMI: u32 = mkrid(64, 1, 1);
/// CI+ content control resources, versions 1, 2 and the TS 103 205 one.
pub const CIPLUS_CC: u32 = mkrid(140, 64, 1);
pub const CIPLUS_CC_TWO: u32 = mkrid(140, 64, 2);
pub const TS103205_CC_THREE: u32 = mkrid(140, 64, 4);

pub const TAG_PROFILE_ENQUIRY: u32 = 0x9F8010;
pub const TAG_PROFILE: u32 = 0x9F8011;
pub const TAG_PROFILE_CHANGE: u32 = 0x9F8012;
pub const TAG_APP_INFO_ENQUIRY: u32 = 0x9F8020;
pub const TAG_APP_INFO: u32 = 0x9F8021;
pub const TAG_CICAM_RESET: u32 = 0x9F8023;
pub const TAG_DATA_RATE_INFO: u32 = 0x9F8024;
pub const TAG_CA_INFO_ENQUIRY: u32 = 0x9F8030;
pub const TAG_CA_INFO: u32 = 0x9F8031;
pub const TAG_CA_PMT: u32 = 0x9F8032;
pub const TAG_CA_PMT_REPLY: u32 = 0x9F8033;
pub const TAG_DATE_TIME_ENQUIRY: u32 = 0x9F8440;
pub const TAG_DATE_TIME: u32 = 0x9F8441;
pub const TAG_CLOSE_MMI: u32 = 0x9F8800;
pub const TAG_DISPLAY_CONTROL: u32 = 0x9F8801;
pub const TAG_DISPLAY_REPLY: u32 = 0x9F8802;
pub const TAG_ENQUIRY: u32 = 0x9F8807;
pub const TAG_ANSWER: u32 = 0x9F8808;
pub const TAG_MENU_LAST: u32 = 0x9F8809;
pub const TAG_MENU_ANSWER: u32 = 0x9F880B;
pub const TAG_LIST_LAST: u32 = 0x9F880C;
pub const TAG_CC_OPEN_REQ: u32 = 0x9F9001;
pub const TAG_CC_OPEN_CNF: u32 = 0x9F9002;
pub const TAG_CC_DATA_REQ: u32 = 0x9F9003;
pub const TAG_CC_DATA_CNF: u32 = 0x9F9004;
pub const TAG_CC_SYNC_REQ: u32 = 0x9F9005;
pub const TAG_CC_SYNC_CNF: u32 = 0x9F9006;
pub const TAG_CC_SAC_DATA_REQ: u32 = 0x9F9007;
pub const TAG_CC_SAC_DATA_CNF: u32 = 0x9F9008;
pub const TAG_CC_SAC_SYNC_REQ: u32 = 0x9F9009;
pub const TAG_CC_SAC_SYNC_CNF: u32 = 0x9F9010;

/// True for the three content control resources.
pub fn is_cc_resource(resource: u32) -> bool {
    matches!(resource, CIPLUS_CC | CIPLUS_CC_TWO | TS103205_CC_THREE)
}

/// Short name of an APDU tag for the recorded log; `None` when unknown.
pub fn apdu_name(tag: u32) -> Option<&'static str> {
    Some(match tag {
        TAG_PROFILE_ENQUIRY => "profile_enq",
        TAG_PROFILE => "profile",
        TAG_PROFILE_CHANGE => "profile_change",
        TAG_APP_INFO_ENQUIRY => "app_info_enq",
        TAG_APP_INFO => "app_info",
        TAG_CICAM_RESET => "cicam_reset",
        TAG_DATA_RATE_INFO => "data_rate_info",
        TAG_CA_INFO_ENQUIRY => "ca_info_enq",
        TAG_CA_INFO => "ca_info",
        TAG_CA_PMT => "ca_pmt",
        TAG_CA_PMT_REPLY => "ca_pmt_reply",
        TAG_DATE_TIME_ENQUIRY => "date_time_enq",
        TAG_DATE_TIME => "date_time",
        TAG_CLOSE_MMI => "close_mmi",
        TAG_DISPLAY_CONTROL => "display_control",
        TAG_DISPLAY_REPLY => "display_reply",
        TAG_ENQUIRY => "enquiry",
        TAG_ANSWER => "answer",
        TAG_MENU_LAST => "menu_last",
        TAG_MENU_ANSWER => "menu_answer",
        TAG_LIST_LAST => "list_last",
        TAG_CC_OPEN_REQ => "cc_open_req",
        TAG_CC_OPEN_CNF => "cc_open_cnf",
        TAG_CC_DATA_REQ => "cc_data_req",
        TAG_CC_DATA_CNF => "cc_data_cnf",
        TAG_CC_SYNC_REQ => "cc_sync_req",
        TAG_CC_SYNC_CNF => "cc_sync_cnf",
        TAG_CC_SAC_DATA_REQ => "cc_sac_data_req",
        TAG_CC_SAC_DATA_CNF => "cc_sac_data_cnf",
        TAG_CC_SAC_SYNC_REQ => "cc_sac_sync_req",
        TAG_CC_SAC_SYNC_CNF => "cc_sac_sync_cnf",
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_ids_match_ca_cpp() {
        assert_eq!(RESOURCE_MANAGER, 0x0001_0041);
        // From tests/test_ca.cpp:1762 (AI session resource 0x20041)
        assert_eq!(APPLICATION_INFO, 0x0002_0041);
        // From tests/test_ca.cpp:2039 (MKRID(3, 1, 1) is CA support)
        assert_eq!(CA_SUPPORT, 0x0003_0041);
        assert_eq!(DATE_TIME, 0x0024_0041);
        assert_eq!(MMI, 0x0040_0041);
    }
}
