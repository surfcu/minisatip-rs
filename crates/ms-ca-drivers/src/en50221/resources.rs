//! `application_handler[]`: resources the host accepts, and the watchdog's command table.

use super::consts::*;

/// Which application handler serves a resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum App {
    ResourceManager,
    AppInfo,
    CaSupport,
    /// `APP_empty`: host control and application MMI.
    Empty,
    DateTime,
    Mmi,
    /// CI+ content control; crypto is out of scope (PLAN §3).
    ContentControl,
    HostLanguage,
    CamUpgrade,
    OperatorProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Handler {
    pub resource: u32,
    pub name: &'static str,
    pub app: App,
}

const fn h(resource: u32, name: &'static str, app: App) -> Handler {
    Handler {
        resource,
        name,
        app,
    }
}

/// Same order as ca.cpp; `populate_resources` sends it as the host profile.
pub const APPLICATION_HANDLERS: [Handler; 21] = [
    h(
        EN50221_APP_RM_RESOURCEID,
        "EN50221_APP_RM_RESOURCEID",
        App::ResourceManager,
    ),
    h(
        EN50221_APP_AI_RESOURCEID,
        "EN50221_APP_AI_RESOURCEID",
        App::AppInfo,
    ),
    h(
        EN50221_APP_CA_RESOURCEID,
        "EN50221_APP_CA_RESOURCEID",
        App::CaSupport,
    ),
    h(
        EN50221_APP_DVB_RESOURCEID,
        "EN50221_APP_DVB_RESOURCEID",
        App::Empty,
    ),
    h(
        EN50221_APP_DATETIME_RESOURCEID,
        "EN50221_APP_DATETIME_RESOURCEID",
        App::DateTime,
    ),
    h(
        EN50221_APP_MMI_RESOURCEID,
        "EN50221_APP_MMI_RESOURCEID",
        App::Mmi,
    ),
    h(
        TS101699_APP_RM_RESOURCEID,
        "TS101699_APP_RM_RESOURCEID",
        App::ResourceManager,
    ),
    h(
        TS101699_APP_AI_RESOURCEID,
        "TS101699_APP_AI_RESOURCEID",
        App::AppInfo,
    ),
    h(
        CIPLUS_APP_AI_RESOURCEID,
        "CIPLUS_APP_AI_RESOURCEID",
        App::AppInfo,
    ),
    h(
        CIPLUS_APP_DVB_RESOURCEID,
        "CIPLUS_APP_DVB_RESOURCEID",
        App::Empty,
    ),
    h(
        TS103205_APP_DVB_RESOURCEID,
        "TS103205_APP_DVB_RESOURCEID",
        App::Empty,
    ),
    h(
        CIPLUS_APP_CC_RESOURCEID,
        "CIPLUS_APP_CC_RESOURCEID",
        App::ContentControl,
    ),
    h(
        CIPLUS_APP_CC_RESOURCEID_TWO,
        "CIPLUS_APP_CC_RESOURCEID_TWO",
        App::ContentControl,
    ),
    h(
        TS103205_APP_CC_RESOURCEID_THREE,
        "TS103205_APP_CC_RESOURCEID_THREE",
        App::ContentControl,
    ),
    h(
        CIPLUS_APP_LANG_RESOURCEID,
        "CIPLUS_APP_LANG_RESOURCEID",
        App::HostLanguage,
    ),
    h(
        CIPLUS_APP_UPGR_RESOURCEID,
        "CIPLUS_APP_UPGR_RESOURCEID",
        App::CamUpgrade,
    ),
    h(
        CIPLUS_APP_OPRF_RESOURCEID,
        "CIPLUS_APP_OPRF_RESOURCEID",
        App::OperatorProfile,
    ),
    h(
        TS103205_APP_OPRF_TWO_RESOURCEID,
        "TS103205_APP_OPRF_TWO_RESOURCEID",
        App::OperatorProfile,
    ),
    h(
        TS103205_APP_OPRF_THREE_RESOURCEID,
        "TS103205_APP_OPRF_THREE_RESOURCEID",
        App::OperatorProfile,
    ),
    h(
        TS101699_APP_AMMI_RESOURCEID,
        "TS101699_APP_AMMI_RESOURCEID",
        App::Empty,
    ),
    h(
        CIPLUS_APP_AMMI_RESOURCEID,
        "CIPLUS_APP_AMMI_RESOURCEID",
        App::Empty,
    ),
];

pub fn find_handler(resource: u32) -> Option<&'static Handler> {
    APPLICATION_HANDLERS.iter().find(|h| h.resource == resource)
}

/// `populate_resources`: the profile, cut before the CI+ resources in CI mode.
pub fn populate_resources(force_ci: bool) -> Vec<u8> {
    APPLICATION_HANDLERS
        .iter()
        .take_while(|h| !(force_ci && h.resource == TS101699_APP_RM_RESOURCEID))
        .flat_map(|h| h.resource.to_be_bytes())
        .collect()
}

/// `ca_tracked_cmds`: commands the CAM must answer, with each accepted reply.
const TRACKED: [(u32, u32); 8] = [
    (TAG_PROFILE_ENQUIRY, TAG_PROFILE),
    (TAG_APP_INFO_ENQUIRY, CIPLUS_TAG_APP_INFO),
    (TAG_CA_INFO_ENQUIRY, TAG_CA_INFO),
    (CIPLUS_TAG_OPERATOR_INFO_REQ, CIPLUS_TAG_OPERATOR_INFO),
    (
        CIPLUS_TAG_OPERATOR_SEARCH_START,
        CIPLUS_TAG_OPERATOR_SEARCH_STATUS,
    ),
    (CIPLUS_TAG_OPERATOR_SEARCH_START, CIPLUS_TAG_OPERATOR_STATUS),
    (CIPLUS_TAG_OPERATOR_SEARCH_START, CIPLUS_TAG_OPERATOR_INFO),
    (CIPLUS_TAG_OPERATOR_SEARCH_START, CIPLUS_TAG_OPERATOR_TUNE),
];

/// `ca_cmd_tracked`.
pub fn cmd_tracked(cmd: u32) -> bool {
    TRACKED.iter().any(|(c, _)| *c == cmd)
}

/// `ca_cmd_reply`.
pub fn cmd_reply(cmd: u32, reply: u32) -> bool {
    TRACKED.iter().any(|&(c, r)| c == cmd && r == reply)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn profile_lists_every_handler_or_stops_before_ci_plus() {
        assert_eq!(populate_resources(false).len(), 21 * 4);
        let ci = populate_resources(true);
        assert_eq!(ci.len(), 6 * 4);
        assert_eq!(&ci[..4], &EN50221_APP_RM_RESOURCEID.to_be_bytes());
        assert_eq!(&ci[20..], &EN50221_APP_MMI_RESOURCEID.to_be_bytes());
    }

    #[test]
    fn host_profile_matches_fakecam_fixture() {
        let ids: Vec<u32> = APPLICATION_HANDLERS.iter().map(|h| h.resource).collect();
        assert_eq!(ids, ms_fakecam::host::HOST_RESOURCES);
    }

    // From tests/test_ca.cpp:1844 (TS 103 205 MMI has no handler)
    #[test]
    fn multistream_mmi_is_unknown() {
        assert!(find_handler(TS103205_APP_MMI_RESOURCEID).is_none());
        assert!(find_handler(EN50221_APP_MMI_RESOURCEID).is_some());
    }

    #[test]
    fn tracked_commands() {
        assert!(cmd_tracked(TAG_CA_INFO_ENQUIRY));
        assert!(!cmd_tracked(TAG_CA_PMT));
        assert!(!cmd_tracked(TAG_CA_INFO));
        assert!(cmd_reply(
            CIPLUS_TAG_OPERATOR_SEARCH_START,
            CIPLUS_TAG_OPERATOR_TUNE
        ));
        assert!(!cmd_reply(TAG_CA_INFO_ENQUIRY, CIPLUS_TAG_APP_INFO));
    }
}
