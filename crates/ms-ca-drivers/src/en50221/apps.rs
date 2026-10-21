//! Application resources (`APP_*_handler`/`_create`/`_close` in ca.cpp).

use super::asn1::decode_len;
use super::consts::*;
use super::device::{CaDevice, CaState, HostEvent};
use super::resources::App;

/// `ca_send_datetime` body: MJD then BCD hours, minutes, seconds (UTC).
pub fn date_time_body(unix: u64) -> [u8; 5] {
    let bcd = |v: u64| (((v / 10) << 4) | (v % 10)) as u8;
    let [hi, lo] = ((unix / 86400 + 40587) as u16).to_be_bytes();
    let t = unix % 86400;
    [hi, lo, bcd(t / 3600), bcd(t % 3600 / 60), bcd(t % 60)]
}

/// Texts of an MMI menu/list body (title, subtitle, bottom, items), as `APP_MMI_handler` walks it.
pub fn mmi_menu_lines(body: &[u8]) -> Vec<String> {
    let mut lines = Vec::new();
    let Some((&first, mut data)) = body.split_first() else {
        return lines;
    };
    let n = if first == 0xFF {
        0
    } else {
        usize::from(first) + 1
    };
    for _ in 0..n + 3 {
        if data.len() < 3 {
            break;
        }
        data = &data[3..];
        let Some((len, llen)) = decode_len(data) else {
            break;
        };
        let Some(text) = data.get(llen..llen + len) else {
            break;
        };
        lines.push(String::from_utf8_lossy(text).into_owned());
        data = &data[llen + len..];
    }
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
                self.write_apdu(idx, TAG_PROFILE_ENQUIRY, &[]);
            }
            Some(App::AppInfo) => {
                if let Some(s) = self.sessions[idx].as_mut() {
                    s.ai_version = (resource & 0x3F) as u8;
                }
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
            }
            Some(App::HostLanguage) => {
                self.write_apdu(idx, CIPLUS_TAG_COUNTRY_REPLY, b"USA");
                self.write_apdu(idx, CIPLUS_TAG_LANG_REPLY, b"eng");
            }
            _ => {}
        }
    }

    /// The handler's `close`; only CA support has one.
    pub(super) fn app_close(&mut self, idx: usize) {
        if self.app_of(idx) == Some(App::CaSupport) {
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
                if tag == TAG_DATE_TIME_ENQUIRY
                    && let Some(&secs) = data.first()
                {
                    self.datetime_response_interval = u64::from(secs) * 1000;
                }
                self.send_datetime();
            }
            Some(App::Mmi) => self.mmi_apdu(idx, tag, data),
            Some(App::HostLanguage) => match tag {
                CIPLUS_TAG_COUNTRY_ENQ => {
                    self.write_apdu(idx, CIPLUS_TAG_COUNTRY_REPLY, b"USA");
                }
                CIPLUS_TAG_LANG_ENQ => {
                    self.write_apdu(idx, CIPLUS_TAG_LANG_REPLY, b"eng");
                }
                _ => {}
            },
            Some(App::CamUpgrade) => match tag {
                CIPLUS_TAG_FIRMWARE_UPGR => {
                    self.write_apdu(idx, CIPLUS_TAG_FIRMWARE_UPGR_REPLY, &[0]);
                }
                CIPLUS_TAG_FIRMWARE_UPGR_COMPLT if data.get(4).is_some_and(|&s| s < 2) => {
                    self.request_close();
                }
                _ => {}
            },
            Some(App::OperatorProfile) => self.oprf_apdu(idx, tag, data),
            Some(App::Empty | App::ContentControl) | None => {}
        }
    }

    fn rm_apdu(&mut self, idx: usize, tag: u32) {
        match tag {
            TAG_PROFILE => {
                self.write_apdu(idx, TAG_PROFILE_CHANGE, &[]);
            }
            TAG_PROFILE_ENQUIRY => {
                let ids = super::resources::populate_resources(self.force_ci);
                self.write_apdu(idx, TAG_PROFILE, &ids);
            }
            _ => {}
        }
    }

    fn ai_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        match tag {
            CIPLUS_TAG_APP_INFO if data.len() >= 6 => {
                let n = usize::from(data[5]).min(data.len() - 6).min(CI_NAME_MAX);
                if n > 0 {
                    self.ci_name = data[6..6 + n].to_vec();
                }
                if self.sessions[idx].is_some_and(|s| s.ai_version >= 3) {
                    self.write_apdu(idx, CI_DATA_RATE_INFO, &[CIPLUS_DATA_RATE_96_MBPS]);
                }
            }
            CIPLUS_TAG_CICAM_RESET => self.request_close(),
            _ => {}
        }
    }

    fn ca_apdu(&mut self, tag: u32, data: &[u8]) {
        if tag != TAG_CA_INFO {
            return;
        }
        self.state = CaState::Initialized;
        if !self.has_forced_caids {
            self.caids = data
                .as_chunks::<2>()
                .0
                .iter()
                .take(MAX_CAID)
                .map(|c| u16::from_be_bytes(*c))
                .collect();
        }
        let caids = self.caids.clone();
        self.push_event(HostEvent::CaInfo { caids });
    }

    /// `ca_send_datetime`: sent when a DATE_TIME session exists, re-armed either way.
    pub(super) fn send_datetime(&mut self) {
        let body = date_time_body(self.clock.unix_time());
        if let Some(idx) = self.find_session_for_resource(EN50221_APP_DATETIME_RESOURCEID) {
            self.write_apdu(idx, TAG_DATE_TIME, &body);
        }
        self.datetime_next_send = self.now() + self.datetime_response_interval as i64;
    }

    fn mmi_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        match tag {
            TAG_DISPLAY_CONTROL if data.len() >= 2 => {
                if data[0] == MMI_DISPLAY_CONTROL_CMD_ID_SET_MMI_MODE {
                    let reply = [MMI_DISPLAY_REPLY_ID_MMI_MODE_ACK, data[1]];
                    self.write_apdu(idx, TAG_DISPLAY_REPLY, &reply);
                }
            }
            TAG_ENQUIRY if data.len() >= 2 => {
                let blind = data[0] & 0x01 != 0;
                let answer_len = data[1];
                let text = String::from_utf8_lossy(&data[2..data.len().min(257)]).into_owned();
                self.push_event(HostEvent::MmiEnquiry {
                    text,
                    blind,
                    answer_len,
                });
                if self.pin.len() == usize::from(answer_len) {
                    let mut answer = vec![MMI_ANSW_ID_ANSWER];
                    answer.extend_from_slice(&self.pin);
                    self.write_apdu(idx, TAG_ANSWER, &answer);
                }
                self.write_apdu(idx, TAG_CLOSE_MMI, &[MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
            }
            TAG_LIST_LAST | TAG_MENU_LAST => {
                let lines = mmi_menu_lines(data);
                self.push_event(HostEvent::MmiMenu { lines });
                self.write_apdu(idx, TAG_MENU_ANSWER, &[0x01]);
                self.write_apdu(idx, TAG_CLOSE_MMI, &[MMI_CLOSE_MMI_CMD_ID_IMMEDIATE]);
            }
            _ => {}
        }
    }

    fn oprf_apdu(&mut self, idx: usize, tag: u32, data: &[u8]) {
        const SEARCH: [u8; 8] = [0x03, 0x01, 0x16, 0x19, 0x02, 0x43, 0x79, 0x00];
        const TUNE_STATUS: [u8; 5] = [0xFF, 0x00, 0x00, 0x30, 0x00];
        match tag {
            CIPLUS_TAG_OPERATOR_STATUS if data.len() >= 3 && data[1] & 0x20 == 0 => {
                self.write_apdu(idx, CIPLUS_TAG_OPERATOR_INFO_REQ, &[]);
            }
            CIPLUS_TAG_OPERATOR_INFO => {
                self.write_apdu(idx, CIPLUS_TAG_OPERATOR_SEARCH_START, &SEARCH);
            }
            CIPLUS_TAG_OPERATOR_TUNE => {
                self.write_apdu(idx, CIPLUS_TAG_OPERATOR_TUNE_STATUS, &TUNE_STATUS);
            }
            _ => {}
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
        assert_eq!(mmi_menu_lines(&b), ["Title", "Sub", "Bottom", "One", "Two"]);
        b[0] = 0xFF;
        assert_eq!(mmi_menu_lines(&b), ["Title", "Sub", "Bottom"]);
        assert!(mmi_menu_lines(&b[..6]).is_empty());
        assert!(mmi_menu_lines(&[]).is_empty());
    }
}
