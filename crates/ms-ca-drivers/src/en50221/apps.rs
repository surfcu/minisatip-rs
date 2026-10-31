//! Application resources (`APP_*_handler`/`_create`/`_close` in ca.cpp).

use super::asn1::decode_len;
#[cfg(feature = "ci-plus")]
use super::ciplus::cc::CcAction;
use super::consts::*;
#[cfg(feature = "ci-plus")]
use super::device::CcKeyBytes;
use super::device::{CaDevice, CaState, HostEvent};
use super::resources::App;

/// `#define DEFAULT_LOG LOG_DVBCA` (ca.cpp:52).
const DEFAULT_LOG: u32 = ms_log::LOG_DVBCA;

/// A C `%s` of `bytes`: everything up to the first NUL.
fn c_str(bytes: &[u8]) -> std::borrow::Cow<'_, str> {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end])
}

/// `ca_send_datetime` body: MJD then BCD hours, minutes, seconds (UTC).
pub fn date_time_body(unix: u64) -> [u8; 5] {
    let bcd = |v: u64| (((v / 10) << 4) | (v % 10)) as u8;
    let [hi, lo] = ((unix / 86400 + 40587) as u16).to_be_bytes();
    let t = unix % 86400;
    [hi, lo, bcd(t / 3600), bcd(t % 3600 / 60), bcd(t % 60)]
}

/// Texts of an MMI menu/list body (title, subtitle, bottom, items), as `APP_MMI_handler` walks it.
pub fn mmi_menu_lines(id: usize, body: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let Some((&first, mut data)) = body.split_first() else {
        return lines;
    };
    let n = if first == 0xFF {
        0
    } else {
        usize::from(first) + 1
    };
    ms_log::log!("CA {} ======= MMI menu =======", id);
    for i in 0..n + 3 {
        if data.len() < 3 {
            break;
        }
        ms_log::debugm!(
            "[{}] text tag: {:02x} {:02x} {:02x}",
            i,
            data[0],
            data[1],
            data[2]
        );
        data = &data[3..];
        let Some((len, llen)) = decode_len(data) else {
            if data.len() >= 3 {
                ms_log::log!(
                    "CA {} asn_1_decode failed: {:02X} {:02X} {:02X}",
                    id,
                    data[0],
                    data[1],
                    data[2]
                );
            }
            break;
        };
        data = &data[llen..];
        ms_log::debugm!("[{}] {} bytes text", i, len);
        let Some(text) = data.get(..len) else {
            break;
        };
        ms_log::log!("[UI {}] {}", i, c_str(text));
        lines.push(String::from_utf8_lossy(text).into_owned());
        data = &data[len..];
    }
    ms_log::log!("======================");
    lines
}

impl CaDevice {
    fn app_of(&self, idx: usize) -> Option<App> {
        self.sessions[idx].map(|s| s.handler.app)
    }

    /// The handler's `create`, run right after the open session response.
    pub(super) fn app_create(&mut self, idx: usize, resource: u32) {
        match self.app_of(idx) {
            Some(App::ResourceManager) => {
                ms_log::log!("RM Created");
                self.write_apdu(idx, TAG_PROFILE_ENQUIRY, &[]);
            }
            Some(App::AppInfo) => {
                if let Some(s) = self.sessions[idx].as_mut() {
                    s.ai_version = (resource & 0x3F) as u8;
                }
                let version = self.sessions[idx].map_or(0, |s| s.ai_version);
                ms_log::log!(
                    "APP_AI_create: CAM requested version {} of the Application Information resource",
                    version
                );
                self.write_apdu(idx, TAG_APP_INFO_ENQUIRY, &[]);
            }
            Some(App::CaSupport) => {
                self.write_apdu(idx, TAG_CA_INFO_ENQUIRY, &[]);
            }
            Some(App::ContentControl) => {
                self.uri_mask = if resource == CIPLUS_APP_CC_RESOURCEID {
                    1
                } else {
                    3
                };
                #[cfg(feature = "ci-plus")]
                {
                    let mask = self.uri_mask;
                    self.cc_state().set_uri_mask(mask);
                }
            }
            Some(App::HostLanguage) => {
                ms_log::log!("APP_LANG_create: Sending language to the CAM");
                self.write_apdu(idx, CIPLUS_TAG_COUNTRY_REPLY, b"USA");
                self.write_apdu(idx, CIPLUS_TAG_LANG_REPLY, b"eng");
            }
            _ => {}
        }
    }

    /// The handler's `close`; only CA support has one.
    pub(super) fn app_close(&mut self, idx: usize) {
        if self.app_of(idx) == Some(App::CaSupport) {
            ms_log::log!("Closed the CA session for CA {}", self.id);
            self.state = CaState::Active;
            self.push_event(HostEvent::CaSessionClosed);
        }
    }

    /// The handler's callback for one APDU.
    pub(super) fn app_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        match self.app_of(idx) {
            Some(App::ResourceManager) => self.rm_apdu(idx, tag),
            Some(App::AppInfo) => self.ai_apdu(idx, tag, data),
            Some(App::CaSupport) => self.ca_apdu(tag, data),
            Some(App::DateTime) => {
                match tag {
                    TAG_DATE_TIME_ENQUIRY => {
                        if let Some(&secs) = data.first() {
                            self.datetime_response_interval = u64::from(secs) * 1000;
                        }
                    }
                    _ => ms_log::log!("unexpected tag in APP_DateTime_handler (0x{:x})", tag),
                }
                self.send_datetime();
            }
            Some(App::Mmi) => self.mmi_apdu(idx, tag, data),
            Some(App::HostLanguage) => match tag {
                CIPLUS_TAG_COUNTRY_ENQ => {
                    ms_log::log!("country answered with '{}'", "USA");
                    self.write_apdu(idx, CIPLUS_TAG_COUNTRY_REPLY, b"USA");
                }
                CIPLUS_TAG_LANG_ENQ => {
                    ms_log::log!("language answered with '{}'", "eng");
                    self.write_apdu(idx, CIPLUS_TAG_LANG_REPLY, b"eng");
                }
                _ => ms_log::log!("unknown host lac apdu tag {:02x}", tag),
            },
            Some(App::CamUpgrade) => {
                ms_log::log!("CAM_fw_upgrade_receive");
                match tag {
                    CIPLUS_TAG_FIRMWARE_UPGR => {
                        ms_log::log!("CI+ CA{} Firmware Upgrade Command detected... ", self.id);
                        self.write_apdu(idx, CIPLUS_TAG_FIRMWARE_UPGR_REPLY, &[0]);
                    }
                    CIPLUS_TAG_FIRMWARE_UPGR_PRGRS => {
                        if let Some(&percent) = data.get(4) {
                            ms_log::log!(
                                "CI+ CA{} Firmware Upgrade Progress {} percents",
                                self.id,
                                percent
                            );
                        }
                    }
                    CIPLUS_TAG_FIRMWARE_UPGR_COMPLT => {
                        if let Some(&status) = data.get(4) {
                            ms_log::log!(
                                "CI+ CA{} Firmware Upgrade Complete (reset status {})",
                                self.id,
                                status
                            );
                            if status < 2 {
                                self.request_close();
                            }
                        }
                    }
                    _ => ms_log::log!("unknown fw upgrade apdu tag {:06X}", tag),
                }
            }
            Some(App::OperatorProfile) => self.oprf_apdu(idx, tag, data),
            Some(App::Empty) => ms_log::log!("Got unhandled resource {:06X}", tag),
            Some(App::ContentControl) => self.cc_apdu(idx, tag, data),
            None => {}
        }
    }

    /// `CIPLUS_APP_CC_handler`: the state machine answers, the device applies.
    #[cfg(feature = "ci-plus")]
    fn cc_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        let actions = self.cc_state().handle(tag, data);
        for action in actions {
            match action {
                CcAction::Apdu { tag, body } => {
                    self.write_apdu(idx, tag, &body);
                }
                CcAction::NewKey { parity, key, iv } => {
                    let slot = usize::from(parity);
                    if slot < 2 {
                        self.key[slot] = key;
                        self.iv[slot] = iv;
                        self.parity = parity;
                        // A key arrived: the slot is alive and any MMI recovery is off.
                        self.last_key_ms = self.now();
                        self.mmi_error_since = -1;
                        self.push_event(HostEvent::CcKey {
                            parity,
                            key: CcKeyBytes(key),
                            iv: CcKeyBytes(iv),
                        });
                    }
                }
                CcAction::DisableCws => self.push_event(HostEvent::CcDisableCws),
                CcAction::ForceCi => self.force_ci = true,
            }
        }
    }

    /// Plain CI: content control is not handled.
    #[cfg(not(feature = "ci-plus"))]
    fn cc_apdu(&mut self, _idx: usize, _tag: u32, _data: &[u8]) {}

    fn rm_apdu(&mut self, idx: usize, tag: u32) {
        match tag {
            TAG_PROFILE => {
                self.write_apdu(idx, TAG_PROFILE_CHANGE, &[]);
            }
            TAG_PROFILE_ENQUIRY => {
                ms_log::log!(
                    "Populating {} resources",
                    if self.force_ci { "CI" } else { "CI+" }
                );
                let ids = super::resources::populate_resources(self.force_ci);
                self.write_apdu(idx, TAG_PROFILE, &ids);
            }
            _ => ms_log::log!("unexpected tag in ResourceManagerHandle (0x{:x})", tag),
        }
    }

    fn ai_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        let number = self.sessions[idx].map_or(0, |s| s.number);
        ms_log::log!(
            "RECV ciplus AI msg CA {}, session_num {}, resource_id {:x}",
            self.id,
            number,
            tag
        );
        match tag {
            CIPLUS_TAG_APP_INFO => {
                ms_log::hexdump!("CIPLUS_TAG_APP_INFO", data);
                if data.len() < 6 {
                    ms_log::log!(
                        "CA {}: application info too short ({})",
                        self.id,
                        data.len()
                    );
                } else {
                    ms_log::log!("  Application type: {:02x}", data[0]);
                    ms_log::log!(
                        "  Application manufacturer: {:04X}",
                        (u16::from(data[2]) << 8) | u16::from(data[1])
                    );
                    ms_log::log!(
                        "  Manufacturer code: {:04X}",
                        (u16::from(data[4]) << 8) | u16::from(data[3])
                    );
                    let n = usize::from(data[5]).min(data.len() - 6).min(CI_NAME_MAX);
                    if n > 0 {
                        self.set_ci_name(&data[6..6 + n]);
                    }
                    ms_log::log!("  CAM Name: {} ({})", self.ci_name(), n);
                    if self.sessions[idx].is_some_and(|s| s.ai_version >= 3) {
                        ms_log::log!("setting CI+ CAM data rate to {} Mbps", "96");
                        self.write_apdu(idx, CI_DATA_RATE_INFO, &[CIPLUS_DATA_RATE_96_MBPS]);
                    }
                }
            }
            CIPLUS_TAG_CICAM_RESET => {
                ms_log::hexdump!("CIPLUS_TAG_CICAM_RESET", data);
                ms_log::log!("CA {} Reset requested", self.id);
                self.request_close();
            }
            _ => ms_log::log!("unexpected tag in CIPLUS_APP_AI_handler (0x{:x})", tag),
        }
    }

    fn ca_apdu(&mut self, tag: u32, data: &[u8]) {
        if tag != TAG_CA_INFO {
            ms_log::log!("APP_CA_handler: unexpected tag (0x{:x})", tag);
            return;
        }
        self.state = CaState::Initialized;
        self.report_state("ready");
        if !self.has_forced_caids {
            let chunks = data.as_chunks::<2>().0;
            if chunks.len() > MAX_CAID {
                ms_log::log!(
                    "CA{}: CAM reported more CAIDs than MAX_CAID limit ({}), truncating",
                    self.id,
                    MAX_CAID
                );
            }
            self.caids = chunks
                .iter()
                .take(MAX_CAID)
                .map(|c| u16::from_be_bytes(*c))
                .collect();
        }
        for &caid in &self.caids {
            ms_log::log!("   Supported CA ID: {:04X} for CA{}", caid, self.id);
        }
        let caids = self.caids.clone();
        self.push_event(HostEvent::CaInfo { caids });
    }

    /// `ca_send_datetime`: sent when a DATE_TIME session exists, re-armed either way.
    pub(super) fn send_datetime(&mut self) {
        let body = date_time_body(self.clock.unix_time());
        if let Some(idx) = self.find_session_for_resource(EN50221_APP_DATETIME_RESOURCEID) {
            let number = self.sessions[idx].map_or(0, |s| s.number);
            ms_log::log!(
                "Sending time to CA {} to session {}, response interval: {}",
                self.id,
                number,
                self.datetime_response_interval
            );
            self.write_apdu(idx, TAG_DATE_TIME, &body);
        }
        self.datetime_next_send = self.now() + self.datetime_response_interval as i64;
    }

    fn mmi_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        ms_log::hexdump!("Got MMI: ", data);
        match tag {
            TAG_DISPLAY_CONTROL if data.len() >= 2 => {
                if data[0] == MMI_DISPLAY_CONTROL_CMD_ID_SET_MMI_MODE {
                    let number = self.sessions[idx].map_or(0, |s| s.number);
                    ms_log::log!(
                        "mmi display ctl cb received for CA {} session_num {} cmd_id 0x{:02x} mmi_mode {}",
                        self.id,
                        number,
                        data[0],
                        data[1]
                    );
                    let reply = [MMI_DISPLAY_REPLY_ID_MMI_MODE_ACK, data[1]];
                    self.write_apdu(idx, TAG_DISPLAY_REPLY, &reply);
                }
            }
            TAG_ENQUIRY if data.len() >= 2 => {
                #[cfg(feature = "ci-plus")]
                self.note_mmi_error();
                let blind = data[0] & 0x01 != 0;
                let answer_len = data[1];
                let text_bytes = &data[2..data.len().min(2 + 255)];
                let text = String::from_utf8_lossy(text_bytes).into_owned();
                ms_log::log!(
                    "MMI enquiry from CAM in CA {}:  {} ({}{} digits)",
                    self.id,
                    c_str(text_bytes),
                    if blind { "blind " } else { "" },
                    answer_len
                );
                self.push_event(HostEvent::MmiEnquiry {
                    text,
                    blind,
                    answer_len,
                });
                if self.pin.len() == usize::from(answer_len) {
                    ms_log::log!("answering to PIN enquiry");
                    let mut answer = vec![MMI_ANSW_ID_ANSWER];
                    answer.extend_from_slice(&self.pin);
                    self.write_apdu(idx, TAG_ANSWER, &answer);
                }
                self.write_apdu(idx, TAG_CLOSE_MMI, &[MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
            }
            TAG_LIST_LAST | TAG_MENU_LAST => {
                #[cfg(feature = "ci-plus")]
                self.note_mmi_error();
                let lines = mmi_menu_lines(self.id, data);
                self.push_event(HostEvent::MmiMenu { lines });
                self.write_apdu(idx, TAG_MENU_ANSWER, &[0x01]);
                self.write_apdu(idx, TAG_CLOSE_MMI, &[MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
            }
            // C++ reads these unchecked; the port leaves short APDUs alone.
            TAG_DISPLAY_CONTROL | TAG_ENQUIRY => {}
            _ => ms_log::log!(
                "APP_MMI_handler: CA {} unknown tag resource {:06X}",
                self.id,
                tag
            ),
        }
    }

    /// Arms CI+ key recovery when a CI+ CAM reports an MMI error, not on plain CI
    /// and not before the first key. No C++ equivalent.
    #[cfg(feature = "ci-plus")]
    fn note_mmi_error(&mut self) {
        if self.force_ci
            || self.recovery_timeout_ms <= 0
            || self.mmi_error_since >= 0
            || self.last_key_ms < 0
        {
            return;
        }
        self.mmi_error_since = self.now();
    }

    fn oprf_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        const SEARCH: [u8; 8] = [0x03, 0x01, 0x16, 0x19, 0x02, 0x43, 0x79, 0x00];
        const TUNE_STATUS: [u8; 5] = [0xFF, 0x00, 0x00, 0x30, 0x00];
        ms_log::hexdump!("CIPLUS_APP_OPRF_handler", data);
        match tag {
            CIPLUS_TAG_OPERATOR_STATUS | CIPLUS_TAG_OPERATOR_SEARCH_STATUS if data.len() >= 3 => {
                let initialized = data[1] & (1 << 5) != 0;
                ms_log::log!(
                    "Received {} APDU: \n  info version:    {}\n  profile type:    {}\n  initialized:     {}\n  refresh request: {}\n  error:           {}\n",
                    if tag == CIPLUS_TAG_OPERATOR_STATUS {
                        "operator_status"
                    } else {
                        "operator_search_status"
                    },
                    data[0] >> 5,
                    data[1] >> 6,
                    u8::from(initialized),
                    data[1] & 0x3,
                    data[2] >> 4
                );
                if tag == CIPLUS_TAG_OPERATOR_STATUS && !initialized {
                    ms_log::log!(
                        "Initializing operator_info_req since profile is not initialized yet"
                    );
                    self.write_apdu(idx, CIPLUS_TAG_OPERATOR_INFO_REQ, &[]);
                }
            }
            CIPLUS_TAG_OPERATOR_INFO => {
                if data.len() >= 3 {
                    let lang = data.get(10..13).unwrap_or_default();
                    let name_len = usize::from(data.get(13).copied().unwrap_or(0));
                    let name = data.get(14..14 + name_len).unwrap_or_default();
                    ms_log::log!(
                        "Received operator_info APDU: \n  info valid:                  {}\n  info version:                {}\n  cicam original network id:   {}\n  language code:               {}\n  profile_name:                {}\n",
                        u8::from(data[0] & (1 << 3) != 0),
                        data[0] & 0x07,
                        (u16::from(data[1]) << 8) + u16::from(data[2]),
                        c_str(lang),
                        c_str(name)
                    );
                }
                self.write_apdu(idx, CIPLUS_TAG_OPERATOR_SEARCH_START, &SEARCH);
            }
            CIPLUS_TAG_OPERATOR_TUNE => {
                if let Some(&descr_len) = data.get(1) {
                    ms_log::log!(
                        "Received operator_tune APDU with {} descriptor bytes",
                        descr_len
                    );
                }
                self.write_apdu(idx, CIPLUS_TAG_OPERATOR_TUNE_STATUS, &TUNE_STATUS);
            }
            _ => ms_log::log!("unknown OPRF apdu tag {:03x}", tag),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn date_time_is_mjd_and_bcd() {
        // 2023-11-14 22:13:20 UTC, MJD 60262
        assert_eq!(
            date_time_body(1_700_000_000),
            [0xEB, 0x66, 0x22, 0x13, 0x20]
        );
        assert_eq!(date_time_body(0), [0x9E, 0x8B, 0, 0, 0]);
        assert_eq!(
            date_time_body(1_700_000_000),
            ms_fakecam::host::date_time_body(1_700_000_000)
        );
    }

    #[test]
    fn menu_lines_follow_choice_count() {
        // two choices: title, subtitle, bottom, then the items
        let mut b = vec![0x01];
        for t in ["Title", "Sub", "Bottom", "One", "Two"] {
            b.extend_from_slice(&[0x9F, 0x88, 0x03, t.len() as u8]);
            b.extend_from_slice(t.as_bytes());
        }
        assert_eq!(
            mmi_menu_lines(0, &b),
            ["Title", "Sub", "Bottom", "One", "Two"]
        );
        b[0] = 0xFF;
        assert_eq!(mmi_menu_lines(0, &b), ["Title", "Sub", "Bottom"]);
        assert!(mmi_menu_lines(0, &b[..6]).is_empty());
        assert!(mmi_menu_lines(0, &[]).is_empty());
    }
}
