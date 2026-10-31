//! The status page and RTSP DESCRIBE read the adapter's live frontend values
//! (`ad->strength`, `snr`, `db`, `ber`, `pa`/`fn`) instead of placeholders.

use ms_net::rtsp::Protocol;
use ms_net::session::{DescribeOptions, SessionManager};
use ms_net::web::{WebConfig, WebService};
use ms_sched::{AdapterConfig, AdapterTuner, Scheduler, TuneError};
use ms_tuner::{DeliverySystem, SignalStatus, TuneParams};

struct NoTune;

impl AdapterTuner for NoTune {
    fn tune(&mut self, _params: &TuneParams) -> Result<(), TuneError> {
        Ok(())
    }
}

fn manager() -> SessionManager {
    let mut sched = Scheduler::new();
    // /dev/dvb/adapter3/frontend1 (dvb.cpp:2418 sets pa and fn).
    let dvb = AdapterConfig {
        phy_adapter: 3,
        phy_frontend: 1,
        ..AdapterConfig::new(DeliverySystem::DvbS2)
    };
    sched.add_adapter(0, dvb, Box::new(NoTune));
    sched.add_adapter(
        1,
        AdapterConfig::new(DeliverySystem::DvbS2),
        Box::new(NoTune),
    );
    SessionManager::with_scheduler(sched)
}

/// Marks `aid` as tuned, which is what the status page treats as in use.
fn mark_tuned(session: &mut SessionManager, aid: usize) {
    let adapter = session
        .scheduler_mut()
        .adapter_mut(aid)
        .expect("adapter exists");
    adapter.params.freq = 11_362_000;
}

fn reading() -> SignalStatus {
    SignalStatus {
        locked: true,
        strength: 200,
        snr: 120,
        ber: 7,
        db: 1234,
        status_valid: true,
    }
}

// adapter.cpp:2420-2437: ad_strength, ad_snr, ad_db, ad_ber and ad_phyad come
// from the adapter struct, which get_signal and the DVB device set.
#[test]
fn live_reading_and_phy_numbers_reach_the_status_page() {
    let mut session = manager();
    mark_tuned(&mut session, 0);
    session.set_adapter_signal(0, Some(reading()));

    let snapshot = session.web_snapshot();
    let adapter = &snapshot.adapters[0];
    assert_eq!(
        (adapter.strength, adapter.snr, adapter.db, adapter.ber),
        (200, 120, 1234, 7)
    );
    assert_eq!((adapter.phy_adapter, adapter.phy_frontend), (3, 1));

    let json = WebService::new(WebConfig::default()).state_json(&snapshot);
    assert!(json.contains("\"ad_strength\":[200,"), "{json}");
    assert!(json.contains("\"ad_snr\":[120,"), "{json}");
    assert!(json.contains("\"ad_db\":[1234,"), "{json}");
    assert!(json.contains("\"ad_phyad\":[3,"), "{json}");
}

// adapter.cpp:440-443 and 1092-1097: an adapter that is not tuned reports no
// signal, whatever the last reading was; db is MAX_DB until a measurement.
// ad->ber is never reset by C++, so the last value stays.
#[test]
fn untuned_adapter_reports_no_signal_but_keeps_ber() {
    let mut session = manager();
    session.set_adapter_signal(1, Some(reading()));
    let snapshot = session.web_snapshot();
    let adapter = &snapshot.adapters[1];
    assert_eq!((adapter.strength, adapter.snr), (0, 0));
    assert_eq!(adapter.db, SignalStatus::DB_UNKNOWN);
    assert_eq!(adapter.ber, 7, "ad->ber is kept");
}

// A tune clears strength, snr and db (the data plane sends None); ber is kept.
#[test]
fn cleared_reading_reports_no_signal_and_keeps_ber() {
    let mut session = manager();
    mark_tuned(&mut session, 0);
    session.set_adapter_signal(0, Some(reading()));
    session.set_adapter_signal(0, None);
    let snapshot = session.web_snapshot();
    let adapter = &snapshot.adapters[0];
    assert_eq!((adapter.strength, adapter.snr), (0, 0));
    assert_eq!(adapter.db, SignalStatus::DB_UNKNOWN);
    assert_eq!(adapter.ber, 7);
}

fn describe_options() -> DescribeOptions {
    DescribeOptions {
        local_host: "127.0.0.1".to_owned(),
        session_id: Some(1000),
        session_version: Some(1000),
        time: "0 0".to_owned(),
    }
}

const URL: &str = "?src=1&freq=11362&pol=h&sr=22000&msys=dvbs2&pids=0,16";

fn play(session: &mut SessionManager) -> ms_net::StreamId {
    session.set_describe_options(describe_options());
    let id = session
        .setup_stream_request(URL, None, "SETUP")
        .expect("setup_stream returned NULL");
    assert!(
        session.start_play(id, Protocol::Http).is_ok(),
        "start_play failed"
    );
    id
}

fn describe(session: &mut SessionManager, id: ms_net::StreamId) -> String {
    session
        .describe_streams(Some(id), URL)
        .expect("describe_streams returned NULL")
}

// adapter.cpp:1471-1487: the DESCRIBE fmtp uses the live strength, status and
// snr (snr above 15 is shifted right by four); 255,1,15 only without a reading.
#[test]
fn describe_uses_the_live_reading() {
    let mut session = manager();
    let id = play(&mut session);
    let before = describe(&mut session, id);
    assert!(
        before.contains("tuner=1,255,1,15,11362,"),
        "placeholder before a reading: {before}"
    );

    session.set_adapter_signal(0, Some(reading()));
    let live = describe(&mut session, id);
    assert!(
        live.contains("tuner=1,200,1,7,11362,"),
        "live strength, status and snr>>4: {live}"
    );
}

// adapter.cpp:1092 sets ad->status to -1 until a status is read: the placeholder
// stays while the reading has no valid status, even though the web values are live.
#[test]
fn describe_placeholder_until_the_status_is_valid() {
    let mut session = manager();
    let id = play(&mut session);
    let invalid = SignalStatus {
        status_valid: false,
        ..reading()
    };
    session.set_adapter_signal(0, Some(invalid));
    let sdp = describe(&mut session, id);
    assert!(
        sdp.contains("tuner=1,255,1,15,11362,"),
        "status not valid: {sdp}"
    );

    let web = session.web_snapshot();
    assert_eq!(
        web.adapters[0].strength, 200,
        "the status page keeps the reading"
    );
}

// A retune to another transponder clears the reading (adapter.cpp:1092-1097).
#[test]
fn retune_to_another_mux_clears_the_stored_reading() {
    let mut session = manager();
    let id = play(&mut session);
    session.set_adapter_signal(0, Some(reading()));
    assert!(describe(&mut session, id).contains("tuner=1,200,"));
    session.close_stream(id);

    let other = "?src=1&freq=11400&pol=h&sr=22000&msys=dvbs2&pids=0,16";
    let second = session
        .setup_stream_request(other, None, "SETUP")
        .expect("setup_stream returned NULL");
    assert!(session.start_play(second, Protocol::Http).is_ok());
    let sdp = session
        .describe_streams(Some(second), other)
        .expect("describe_streams returned NULL");
    assert!(
        sdp.contains("tuner=1,255,1,15,11400,"),
        "cleared by the retune: {sdp}"
    );
}

// A second PLAY on the mux already tuned, while the first stream still runs, does
// not retune (do_tune is clear), so C++ keeps the reading (adapter.cpp:1086-1097
// sit inside the tune branch). Closing the last stream would retune on the next
// PLAY, which is why this test keeps the first stream open.
#[test]
fn same_mux_play_keeps_the_stored_reading() {
    let mut session = manager();
    let _first = play(&mut session);
    session.set_adapter_signal(0, Some(reading()));

    let second = session
        .setup_stream_request(URL, None, "SETUP")
        .expect("setup_stream returned NULL");
    assert!(session.start_play(second, Protocol::Http).is_ok());
    let sdp = session
        .describe_streams(Some(second), URL)
        .expect("describe_streams returned NULL");
    assert!(
        sdp.contains("tuner=1,200,1,7,11362,"),
        "same mux keeps the reading: {sdp}"
    );
}
