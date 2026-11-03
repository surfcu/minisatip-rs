//! Typed constructors for diagnostic events.
//!
//! Every constructor is a no-op without the `diag-log` feature. Arguments that
//! allocate are built inside a closure, so nothing is computed when it is off.

use super::{Fields, event_json};
use crate::diag;

/// Emits one event through the local outputs and the remote sink.
fn emit(name: &str, fields: &Fields) {
    let json = event_json(name, &crate::rfc3339_now(), &crate::thread_name(), fields);
    crate::write_local(&json);
    diag::remote::send_event(&json);
}

fn tune_fields(adapter: i64, freq: u64, pol: i64, sr: u64, msys: i64, result: &str) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("freq", freq);
    fields.integer("pol", pol);
    fields.int("sr", sr);
    fields.integer("msys", msys);
    fields.string("result", result);
    fields
}

/// `tune`: a scheduler tune attempt.
pub fn tune(adapter: i64, result: &str, params: impl FnOnce() -> (u64, i64, u64, i64)) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    let (freq, pol, sr, msys) = params();
    emit("tune", &tune_fields(adapter, freq, pol, sr, msys, result));
}

fn lock_fields(adapter: i64, locked: bool, status: i64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.boolean("locked", locked);
    fields.integer("status", status);
    fields
}

/// `lock`: frontend lock state changed.
pub fn lock(adapter: i64, locked: bool, status: i64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("lock", &lock_fields(adapter, locked, status));
}

fn signal_fields(adapter: i64, strength: u16, snr: u16, ber: u32) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("strength", u64::from(strength));
    fields.int("snr", u64::from(snr));
    fields.int("ber", u64::from(ber));
    fields
}

/// `signal`: signal values moved beyond the reporting threshold.
pub fn signal(adapter: i64, strength: u16, snr: u16, ber: u32) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("signal", &signal_fields(adapter, strength, snr, ber));
}

fn cc_errors_fields(adapter: i64, pid: u16, count: u64, period_s: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("pid", u64::from(pid));
    fields.int("count", count);
    fields.int("period_s", period_s);
    fields
}

/// `cc_errors`: aggregated continuity errors for one PID.
pub fn cc_errors(adapter: i64, pid: u16, count: u64, period_s: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "cc_errors",
        &cc_errors_fields(adapter, pid, count, period_s),
    );
}

fn send_drops_fields(client: &str, session: i64, kind: &str, bytes: u64, period_s: u64) -> Fields {
    let mut fields = Fields::new();
    fields.string("client", client);
    fields.integer("session", session);
    fields.string("kind", kind);
    fields.int("count", bytes);
    fields.int("period_s", period_s);
    fields
}

/// `send_drops`: aggregated output drops for one client.
pub fn send_drops(
    client: impl FnOnce() -> String,
    session: i64,
    kind: &str,
    bytes: u64,
    period_s: u64,
) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "send_drops",
        &send_drops_fields(&client(), session, kind, bytes, period_s),
    );
}

fn session_fields(session: i64, proto: &str, adapter: i64, client: &str) -> Fields {
    let mut fields = Fields::new();
    fields.integer("session", session);
    fields.string("proto", proto);
    fields.integer("adapter", adapter);
    fields.string("client", client);
    fields
}

/// `session_open`: playback started for a session.
pub fn session_open(session: i64, proto: &str, adapter: i64, client: impl FnOnce() -> String) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "session_open",
        &session_fields(session, proto, adapter, &client()),
    );
}

/// `session_close`: playback ended for a session.
pub fn session_close(session: i64, proto: &str, adapter: i64, client: impl FnOnce() -> String) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "session_close",
        &session_fields(session, proto, adapter, &client()),
    );
}

fn capmt_fields(
    ca: i64,
    hash: u32,
    version: i64,
    reason: &str,
    skipped: bool,
    pmt_ids: Vec<u64>,
) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", ca);
    fields.int_list("pmt_ids", pmt_ids);
    fields.int("hash", u64::from(hash));
    fields.integer("version", version);
    fields.string("reason", reason);
    fields.boolean("skipped", skipped);
    fields
}

/// `capmt`: a CAPMT send attempt, including one skipped by the content hash.
pub fn capmt(
    ca: i64,
    hash: impl FnOnce() -> u32,
    version: i64,
    reason: &str,
    skipped: bool,
    pmt_ids: impl FnOnce() -> Vec<u64>,
) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "capmt",
        &capmt_fields(ca, hash(), version, reason, skipped, pmt_ids()),
    );
}

fn cam_state_fields(ca: i64, state: &str, mode: &str, suppressed: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", ca);
    fields.string("state", state);
    fields.string("mode", mode);
    if suppressed > 0 {
        fields.int("suppressed", suppressed);
    }
    fields
}

/// `cam_state`: an EN 50221 slot changed state.
pub fn cam_state(ca: i64, state: &str, mode: &str) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("cam_state", &cam_state_fields(ca, state, mode, 0));
}

/// One CA slot's `cam_state` changes: a repeat is dropped (but not `cam_recovery`); a reset right after `init`
/// mutes that slot's init/reset loop, counted in `suppressed` on its next event.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CamStateFilter {
    state: String,
    mode: String,
    muted: bool,
    suppressed: u64,
}

impl CamStateFilter {
    /// Sends `cam_state` when the filter lets it through.
    pub fn send(&mut self, ca: i64, state: &str, mode: &str) {
        if !cfg!(feature = "diag-log") {
            return;
        }
        if let Some(suppressed) = self.wanted(state, mode) {
            emit("cam_state", &cam_state_fields(ca, state, mode, suppressed));
        }
    }

    /// `Some(events muted since the last one)` when this event is sent.
    fn wanted(&mut self, state: &str, mode: &str) -> Option<u64> {
        // A mode change (CI+ falling back to CI) always ends the mute.
        if matches!(state, "init" | "reset") && self.muted && self.mode == mode {
            self.suppressed += 1;
            return None;
        }
        // Every `--cam-recovery` re-send is reported; it changes no other state.
        if self.state == state && self.mode == mode && state != "cam_recovery" {
            return None;
        }
        self.muted = state == "reset" && self.state == "init";
        self.state = state.to_owned();
        self.mode = mode.to_owned();
        Some(std::mem::take(&mut self.suppressed))
    }
}

fn cw_fields(pmt: i64, parity: i64, ecm_ms: Option<i64>, source: &str) -> Fields {
    let mut fields = Fields::new();
    fields.integer("pmt", pmt);
    fields.integer("parity", parity);
    fields.opt_int("ecm_ms", ecm_ms);
    fields.string("source", source);
    fields
}

/// `cw`: a control word was installed; `ecm_ms` is null when unknown.
pub fn cw(pmt: i64, parity: i64, ecm_ms: Option<i64>, source: &str) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("cw", &cw_fields(pmt, parity, ecm_ms, source));
}

fn ddci_batch_fields(adapter: i64, grown_to: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("grown_to", grown_to);
    fields
}

/// `ddci_batch`: CAM output grew the adapter batch.
pub fn ddci_batch(adapter: i64, grown_to: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("ddci_batch", &ddci_batch_fields(adapter, grown_to));
}

fn ddci_fifo_overflow_fields(adapter: i64, dropped: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("dropped", dropped);
    fields
}

/// `ddci_fifo_overflow`: bytes were lost in a DDCI FIFO.
pub fn ddci_fifo_overflow(adapter: i64, dropped: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "ddci_fifo_overflow",
        &ddci_fifo_overflow_fields(adapter, dropped),
    );
}

fn ddci_capture_fields(slot: i64, files: &str, bytes: u64, drops: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("slot", slot);
    fields.string("files", files);
    fields.int("bytes", bytes);
    fields.int("drops", drops);
    fields
}

/// `ddci_capture`: a `--ddci-capture` pair was written; `drops` counts bytes the writer missed.
pub fn ddci_capture(slot: i64, files: &str, bytes: u64, drops: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "ddci_capture",
        &ddci_capture_fields(slot, files, bytes, drops),
    );
}

/// One MMI page as `cam_menu` reports it; the page text itself stays in the `[UI n]` lines.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CamMenu<'a> {
    pub ca: i64,
    pub kind: &'a str,
    pub items: u64,
    pub lines: u64,
    pub text_hash: u64,
    pub answer: &'a str,
    pub solicited: bool,
}

fn cam_menu_fields(menu: &CamMenu<'_>) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", menu.ca);
    fields.string("kind", menu.kind);
    fields.int("items", menu.items);
    fields.int("lines", menu.lines);
    fields.string("text_hash", &format!("{:016x}", menu.text_hash));
    fields.string("answer", menu.answer);
    fields.boolean("solicited", menu.solicited);
    fields
}

/// `cam_menu`: an MMI menu, list or enquiry page and what the host answered.
pub fn cam_menu(menu: &CamMenu<'_>) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("cam_menu", &cam_menu_fields(menu));
}

/// `ca_tables` fields: ECM/EMM traffic to one DDCI CAM in one window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CaTables {
    pub ddci: i64,
    pub ecm_pkts: u64,
    pub emm_pkts: u64,
    pub ecm_sections: u64,
    pub emm_sections: u64,
    pub parity_dropped: u64,
    pub period_s: u64,
}

fn ca_tables_fields(t: &CaTables) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ddci", t.ddci);
    fields.int("ecm_pkts", t.ecm_pkts);
    fields.int("emm_pkts", t.emm_pkts);
    fields.int("ecm_sections", t.ecm_sections);
    fields.int("emm_sections", t.emm_sections);
    fields.int("parity_dropped", t.parity_dropped);
    fields.int("period_s", t.period_s);
    fields
}

/// `ca_tables`: ECM/EMM packets sent to a DDCI CAM (`--cam-ecm-trace`).
pub fn ca_tables(tables: &CaTables) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("ca_tables", &ca_tables_fields(tables));
}

fn ecm_parity_fields(ddci: i64, adapter: i64, pid: u64, table_id: u64, at_ms: i64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ddci", ddci);
    fields.integer("adapter", adapter);
    fields.int("pid", pid);
    fields.int("table_id", table_id);
    fields.integer("at_ms", at_ms);
    fields
}

/// `ecm_parity`: an ECM PID switched table_id (0x80/0x81) at UTC `at_ms` (`--cam-ecm-trace`).
pub fn ecm_parity(ddci: i64, adapter: i64, pid: u64, table_id: u64, at_ms: i64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "ecm_parity",
        &ecm_parity_fields(ddci, adapter, pid, table_id, at_ms),
    );
}

fn ci_decrypt_fields(
    adapter: i64,
    keys_installed: u64,
    errors: u64,
    packets: u64,
    period_s: u64,
) -> Fields {
    let mut fields = Fields::new();
    fields.integer("adapter", adapter);
    fields.int("keys_installed", keys_installed);
    fields.int("errors", errors);
    fields.int("packets", packets);
    fields.int("period_s", period_s);
    fields
}

/// `ci_decrypt`: CI-adapter descrambler state for one period.
pub fn ci_decrypt(adapter: i64, keys_installed: u64, errors: u64, packets: u64, period_s: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "ci_decrypt",
        &ci_decrypt_fields(adapter, keys_installed, errors, packets, period_s),
    );
}

fn ciplus_sac_fields(ca: i64, state: &str) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", ca);
    fields.string("state", state);
    fields
}

/// `ciplus_sac`: CI+ SAC/DH handshake state.
pub fn ciplus_sac(ca: i64, state: &str) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("ciplus_sac", &ciplus_sac_fields(ca, state));
}

fn ciplus_key_fields(ca: i64, parity: i64, target_adapters: Vec<u64>) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", ca);
    fields.integer("parity", parity);
    fields.int_list("target_adapters", target_adapters);
    fields
}

/// `ciplus_key`: a CI+ key was installed for the listed adapters.
pub fn ciplus_key(ca: i64, parity: i64, target_adapters: impl FnOnce() -> Vec<u64>) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit(
        "ciplus_key",
        &ciplus_key_fields(ca, parity, target_adapters()),
    );
}

fn ciplus_uri_fields(ca: i64, length: u64) -> Fields {
    let mut fields = Fields::new();
    fields.integer("ca", ca);
    fields.int("length", length);
    fields
}

/// `ciplus_uri`: length of the URI message; the bytes are never logged.
pub fn ciplus_uri(ca: i64, length: u64) {
    if !cfg!(feature = "diag-log") {
        return;
    }
    emit("ciplus_uri", &ciplus_uri_fields(ca, length));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sent(states: &[&str]) -> Vec<String> {
        let mut filter = CamStateFilter::default();
        states
            .iter()
            .filter(|state| filter.wanted(state, "ci").is_some())
            .map(|state| (*state).to_owned())
            .collect()
    }

    #[test]
    fn cam_state_carries_the_muted_count() {
        assert_eq!(
            json("cam_state", &cam_state_fields(2, "ready", "ci", 5)),
            envelope(
                "cam_state",
                ",\"ca\":2,\"state\":\"ready\",\"mode\":\"ci\",\"suppressed\":5"
            )
        );
    }

    #[test]
    fn table_events_carry_counts_and_parity() {
        let tables = CaTables {
            ddci: 8,
            ecm_pkts: 66,
            emm_pkts: 12,
            ecm_sections: 33,
            emm_sections: 6,
            parity_dropped: 0,
            period_s: 10,
        };
        assert_eq!(
            json("ca_tables", &ca_tables_fields(&tables)),
            envelope(
                "ca_tables",
                ",\"ddci\":8,\"ecm_pkts\":66,\"emm_pkts\":12,\"ecm_sections\":33,\"emm_sections\":6,\"parity_dropped\":0,\"period_s\":10"
            )
        );
        assert_eq!(
            json(
                "ecm_parity",
                &ecm_parity_fields(8, 2, 0x5f5, 0x81, 1_793_613_371_000)
            ),
            envelope(
                "ecm_parity",
                ",\"ddci\":8,\"adapter\":2,\"pid\":1525,\"table_id\":129,\"at_ms\":1793613371000"
            )
        );
    }

    #[test]
    fn cam_menu_carries_counts_hash_and_answer() {
        let menu = CamMenu {
            ca: 8,
            kind: "menu",
            items: 2,
            lines: 5,
            text_hash: 0xab,
            answer: "1",
            solicited: false,
        };
        assert_eq!(
            json("cam_menu", &cam_menu_fields(&menu)),
            envelope(
                "cam_menu",
                ",\"ca\":8,\"kind\":\"menu\",\"items\":2,\"lines\":5,\"text_hash\":\"00000000000000ab\",\"answer\":\"1\",\"solicited\":false"
            )
        );
    }

    #[test]
    fn every_cam_recovery_is_reported() {
        let states = ["init", "ready", "cam_recovery", "cam_recovery", "ready"];
        assert_eq!(
            sent(&states),
            ["init", "ready", "cam_recovery", "cam_recovery", "ready"]
        );
    }

    #[test]
    fn an_empty_slot_loop_is_reported_once() {
        // The command watchdog resets a slot that never answers every 15 s.
        let states = ["init", "reset", "init", "reset", "init", "reset"];
        assert_eq!(sent(&states), ["init", "reset"]);
    }

    #[test]
    fn a_working_cam_reports_every_change() {
        let states = [
            "init", "ready", "ready", "reset", "init", "ready", "removed",
        ];
        assert_eq!(
            sent(&states),
            ["init", "ready", "reset", "init", "ready", "removed"]
        );
    }

    #[test]
    fn a_mute_ends_with_any_other_state() {
        let states = [
            "init", "reset", "init", "reset", "init", "ready", "reset", "init",
        ];
        assert_eq!(sent(&states), ["init", "reset", "ready", "reset", "init"]);
        let mut filter = CamStateFilter::default();
        assert_eq!(filter.wanted("init", "ci+"), Some(0));
        assert_eq!(filter.wanted("reset", "ci+"), Some(0));
        assert_eq!(filter.wanted("init", "ci+"), None, "muted");
        assert_eq!(filter.wanted("reset", "ci+"), None, "muted");
        assert_eq!(
            filter.wanted("init", "ci"),
            Some(2),
            "a mode change ends the mute"
        );
        assert_eq!(filter.wanted("ready", "ci"), Some(0));
    }

    const TS: &str = "2026-10-24T10:20:30.400+00:00";
    const THREAD: &str = "t";

    fn json(name: &str, fields: &Fields) -> String {
        event_json(name, TS, THREAD, fields)
    }

    fn envelope(name: &str, rest: &str) -> String {
        format!(
            "{{\"kind\":\"event\",\"event\":\"{name}\",\"ts\":\"{TS}\",\"thread\":\"{THREAD}\"{rest}}}"
        )
    }

    #[test]
    fn tune_event_json() {
        assert_eq!(
            json("tune", &tune_fields(2, 11_361_750, 2, 27_500_000, 5, "ok")),
            envelope(
                "tune",
                ",\"adapter\":2,\"freq\":11361750,\"pol\":2,\"sr\":27500000,\"msys\":5,\"result\":\"ok\""
            )
        );
    }

    #[test]
    fn lock_event_json() {
        assert_eq!(
            json("lock", &lock_fields(0, true, 1)),
            envelope("lock", ",\"adapter\":0,\"locked\":true,\"status\":1")
        );
    }

    #[test]
    fn signal_event_json() {
        assert_eq!(
            json("signal", &signal_fields(1, 200, 15, 7)),
            envelope(
                "signal",
                ",\"adapter\":1,\"strength\":200,\"snr\":15,\"ber\":7"
            )
        );
    }

    #[test]
    fn cc_errors_event_json() {
        assert_eq!(
            json("cc_errors", &cc_errors_fields(3, 0x100, 12, 10)),
            envelope(
                "cc_errors",
                ",\"adapter\":3,\"pid\":256,\"count\":12,\"period_s\":10"
            )
        );
    }

    #[test]
    fn send_drops_event_json() {
        assert_eq!(
            json(
                "send_drops",
                &send_drops_fields("192.0.2.1:5004", 1000, "rtp", 4, 10)
            ),
            envelope(
                "send_drops",
                ",\"client\":\"192.0.2.1:5004\",\"session\":1000,\"kind\":\"rtp\",\"count\":4,\"period_s\":10"
            )
        );
    }

    #[test]
    fn session_open_event_json() {
        assert_eq!(
            json(
                "session_open",
                &session_fields(1000, "rtsp", 1, "192.0.2.1:40000")
            ),
            envelope(
                "session_open",
                ",\"session\":1000,\"proto\":\"rtsp\",\"adapter\":1,\"client\":\"192.0.2.1:40000\""
            )
        );
    }

    #[test]
    fn session_close_event_json() {
        assert_eq!(
            json(
                "session_close",
                &session_fields(1000, "http", 1, "192.0.2.1:40000")
            ),
            envelope(
                "session_close",
                ",\"session\":1000,\"proto\":\"http\",\"adapter\":1,\"client\":\"192.0.2.1:40000\""
            )
        );
    }

    #[test]
    fn capmt_event_json_and_pmt_cap() {
        assert_eq!(
            json(
                "capmt",
                &capmt_fields(0, 0xDEAD_BEEF, 3, "rejoin", true, (1u64..=20).collect())
            ),
            envelope(
                "capmt",
                ",\"ca\":0,\"pmt_ids\":[1,2,3,4,5,6,7,8,9,10,11,12,13,14,15,16],\
                 \"hash\":3735928559,\"version\":3,\"reason\":\"rejoin\",\"skipped\":true"
            )
        );
    }

    #[test]
    fn cam_state_event_json() {
        assert_eq!(
            json("cam_state", &cam_state_fields(2, "ready", "ci+", 0)),
            envelope(
                "cam_state",
                ",\"ca\":2,\"state\":\"ready\",\"mode\":\"ci+\""
            )
        );
    }

    #[test]
    fn cw_event_json_with_and_without_ecm_time() {
        assert_eq!(
            json("cw", &cw_fields(7, 1, Some(480), "dvbapi")),
            envelope(
                "cw",
                ",\"pmt\":7,\"parity\":1,\"ecm_ms\":480,\"source\":\"dvbapi\""
            )
        );
        assert_eq!(
            json("cw", &cw_fields(7, 0, None, "ci+")),
            envelope(
                "cw",
                ",\"pmt\":7,\"parity\":0,\"ecm_ms\":null,\"source\":\"ci+\""
            )
        );
    }

    #[test]
    fn ddci_batch_event_json() {
        assert_eq!(
            json("ddci_batch", &ddci_batch_fields(1, 376_832)),
            envelope("ddci_batch", ",\"adapter\":1,\"grown_to\":376832")
        );
    }

    #[test]
    fn ddci_fifo_overflow_event_json() {
        assert_eq!(
            json("ddci_fifo_overflow", &ddci_fifo_overflow_fields(1, 376)),
            envelope("ddci_fifo_overflow", ",\"adapter\":1,\"dropped\":376")
        );
    }

    #[test]
    fn ci_decrypt_event_json() {
        assert_eq!(
            json("ci_decrypt", &ci_decrypt_fields(2, 3, 9, 12, 10)),
            envelope(
                "ci_decrypt",
                ",\"adapter\":2,\"keys_installed\":3,\"errors\":9,\"packets\":12,\"period_s\":10"
            )
        );
    }

    #[test]
    fn ciplus_sac_event_json() {
        assert_eq!(
            json("ciplus_sac", &ciplus_sac_fields(0, "auth_failed")),
            envelope("ciplus_sac", ",\"ca\":0,\"state\":\"auth_failed\"")
        );
    }

    #[test]
    fn ciplus_key_event_json() {
        assert_eq!(
            json("ciplus_key", &ciplus_key_fields(0, 1, vec![1, 3, 5])),
            envelope(
                "ciplus_key",
                ",\"ca\":0,\"parity\":1,\"target_adapters\":[1,3,5]"
            )
        );
    }

    #[test]
    fn ciplus_uri_event_json() {
        assert_eq!(
            json("ciplus_uri", &ciplus_uri_fields(0, 8)),
            envelope("ciplus_uri", ",\"ca\":0,\"length\":8")
        );
    }

    #[test]
    fn string_fields_are_capped_at_256_bytes() {
        let client = "x".repeat(300);
        let text = json("session_open", &session_fields(1, "rtsp", 0, &client));
        assert!(text.contains(&format!("\"client\":\"{}\"", "x".repeat(256))));
        assert!(!text.contains(&"x".repeat(257)));
    }
}
