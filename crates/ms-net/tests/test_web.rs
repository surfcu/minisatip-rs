//! Web UI and generated endpoint tests.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ms_net::bandwidth::BandwidthSnapshot;
use ms_net::handler::{HandlerContext, handle_http};
use ms_net::rtsp::{Protocol, Request};
use ms_net::session::SessionManager;
use ms_net::web::{
    ADAPTER_DVB, AdapterState, ApiFeatures, StreamState, TunerCounts, WebConfig, WebService,
    WebSnapshot, content_type,
};
use ms_tuner::DeliverySystem;

const FILES: [&str; 24] = [
    "jquery.dataTables.min.js",
    "lr.jpg",
    "lr.png",
    "satfinder.html",
    "satip.xml",
    "sm.jpg",
    "sm.png",
    "sort_asc.png",
    "sort_both.png",
    "dvbc.png",
    "dvbc2.png",
    "dvbci.png",
    "dvbs.png",
    "dvbs2.png",
    "dvbs2x.png",
    "dvbt.png",
    "dvbt2.png",
    "favicon.ico",
    "favicon.png",
    "jquery.dataTables.min.css",
    "status.html",
    "dLAN.xml",
    "jquery-3.2.1.min.js",
    "sort_desc.png",
];

fn get(uri: &str) -> Request {
    Request {
        method: "GET".to_owned(),
        uri: uri.to_owned(),
        version: "HTTP/1.0".to_owned(),
        headers: Vec::new(),
        body: Vec::new(),
    }
}

fn head(uri: &str) -> Request {
    Request {
        method: "HEAD".to_owned(),
        ..get(uri)
    }
}

fn config(document_root: PathBuf) -> WebConfig {
    WebConfig {
        document_root,
        name_app: "minisatip".to_owned(),
        version: "1.2.3".to_owned(),
        uuid: "1234-5678".to_owned(),
        http_host: "10.0.0.5:8080".to_owned(),
        rtsp_host: "10.0.0.5".to_owned(),
        http_port: 8080,
        rtsp_port: 554,
        xml_path: "desc.xml".to_owned(),
        playlist: None,
        bootid: 4,
        device_id: 2,
        force_sadapter: 0,
        force_tadapter: 0,
        force_cadapter: 0,
        features: ApiFeatures::default(),
    }
}

fn adapter(systems: &[DeliverySystem]) -> AdapterState {
    AdapterState {
        present: true,
        enabled: true,
        kind: ADAPTER_DVB,
        systems: systems.to_vec(),
        ..AdapterState::default()
    }
}

fn temp_dir() -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0);
    let path = std::env::temp_dir().join(format!("msnet-web-{}-{nanos}", std::process::id()));
    fs::create_dir_all(&path).expect("temp dir");
    path
}

#[test]
fn content_types_match_cpp_readfile() {
    // Ported from utils.cpp:460-488.
    assert_eq!(
        content_type("sm.png"),
        "Cache-Control: max-age=3600\r\nContent-type: image/png\r\nConnection: close"
    );
    assert_eq!(
        content_type("favicon.ico"),
        "Cache-Control: max-age=3600\r\nContent-type: image/x-icon\r\nConnection: close"
    );
    assert_eq!(
        content_type("lr.jpg"),
        "Cache-Control: max-age=3600\r\nContent-type: image/jpeg\r\nConnection: close"
    );
    assert_eq!(
        content_type("jquery.dataTables.min.css"),
        "Cache-Control: max-age=3600\r\nContent-type: text/css\r\nConnection: close"
    );
    assert_eq!(
        content_type("jquery-3.2.1.min.js"),
        "Cache-Control: max-age=3600\r\nContent-type: text/javascript\r\nConnection: close"
    );
    assert_eq!(
        content_type("status.html"),
        "Cache-Control: max-age=3600\r\nContent-type: text/html; charset=utf-8\r\nConnection: close"
    );
    assert_eq!(
        content_type("satip.xml"),
        "Cache-Control: no-cache\r\nContent-type: text/xml"
    );
    assert_eq!(
        content_type("state.json"),
        "Cache-Control: no-cache\r\nContent-type: application/json; charset=utf-8"
    );
    assert_eq!(
        content_type("playlist.m3u"),
        "Cache-Control: no-cache\r\nContent-type: video/x-mpegurl"
    );
    assert_eq!(
        content_type("binary.bin"),
        "Cache-Control: no-cache\r\nContent-type: application/octet-stream"
    );
}

#[test]
fn desc_xml_matches_cpp_format_string() {
    // Expected literal derived from minisatip.cpp:1468-1494.
    let service = WebService::new(config(PathBuf::from("html")));
    let mut snapshot = WebSnapshot::new();
    snapshot.adapters = vec![
        adapter(&[DeliverySystem::DvbS2]),
        adapter(&[DeliverySystem::DvbT]),
        adapter(&[DeliverySystem::DvbC]),
        adapter(&[DeliverySystem::DvbT2]),
        adapter(&[DeliverySystem::DvbC2]),
    ];
    let expected = concat!(
        "<?xml version=\"1.0\"?>",
        "<root xmlns=\"urn:schemas-upnp-org:device-1-0\" configId=\"1\">",
        "<specVersion><major>1</major><minor>1</minor></specVersion>",
        "<device><deviceType>urn:ses-com:device:SatIPServer:1</deviceType>",
        "<friendlyName>minisatip</friendlyName><manufacturer>cata</manufacturer>",
        "<manufacturerURL>http://github.com/catalinii/minisatip</manufacturerURL>",
        "<modelDescription>minisatip for Linux</modelDescription><modelName>minisatip</modelName>",
        "<modelNumber>1.2.3</modelNumber><modelURL></modelURL><serialNumber>1</serialNumber>",
        "<UDN>uuid:1234-5678</UDN>",
        "<iconList>",
        "<icon><mimetype>image/png</mimetype><width>48</width><height>48</height><depth>24</depth><url>/sm.png</url></icon>",
        "<icon><mimetype>image/png</mimetype><width>120</width><height>120</height><depth>24</depth><url>/lr.png</url></icon>",
        "<icon><mimetype>image/jpeg</mimetype><width>48</width><height>48</height><depth>24</depth><url>/sm.jpg</url></icon>",
        "<icon><mimetype>image/jpeg</mimetype><width>120</width><height>120</height><depth>24</depth><url>/lr.jpg</url></icon>",
        "</iconList>",
        "<presentationURL>http://10.0.0.5:8080/</presentationURL>\r\n",
        "<satip:X_SATIPCAP xmlns:satip=\"urn:ses-com:satip\">",
        "DVBS2-1,DVBT-1,DVBC-1,DVBT2-1,DVBC2-1",
        "</satip:X_SATIPCAP>",
        "</device></root>"
    );
    assert_eq!(service.desc_xml(&snapshot), expected);

    let route = service.route(&get("/desc.xml"), &snapshot).expect("route");
    assert_eq!(route.status, 200);
    assert_eq!(
        route.headers,
        vec![
            "Cache-Control: no-cache".to_owned(),
            "Content-type: text/xml".to_owned(),
            "X-SATIP-RTSP-Port: 554".to_owned(),
            "Connection: close".to_owned()
        ]
    );
    assert_eq!(route.body, expected.as_bytes());
}

#[test]
fn configured_port_zero_is_advertised_verbatim() {
    // minisatip.cpp:1130-1136 prints opts.http_port and opts.rtsp_port as
    // configured; it never substitutes the ephemeral port bind() chose.
    let mut cfg = config(PathBuf::from("html"));
    cfg.http_port = 0;
    cfg.rtsp_port = 0;
    let service = WebService::new(cfg);
    let snapshot = WebSnapshot::new();
    let route = service.route(&get("/desc.xml"), &snapshot).expect("route");
    assert!(route.headers.contains(&"X-SATIP-RTSP-Port: 0".to_owned()));
    assert!(service.state_json(&snapshot).contains("\"http_port\":0"));
}

#[test]
fn desc_xml_defaults_to_dvbs2_zero() {
    let service = WebService::new(config(PathBuf::from("html")));
    let snapshot = WebSnapshot::new();
    assert!(service.desc_xml(&snapshot).contains(
        "<satip:X_SATIPCAP xmlns:satip=\"urn:ses-com:satip\">DVBS2-0</satip:X_SATIPCAP>"
    ));
}

#[test]
fn tuner_counts_follow_cpp_adapter_iteration() {
    // adapter.cpp:484-520: DVB-T is only counted when T2 is absent, likewise C.
    let adapters = vec![
        adapter(&[DeliverySystem::DvbS]),
        adapter(&[DeliverySystem::DvbS2]),
        adapter(&[DeliverySystem::DvbT, DeliverySystem::DvbT2]),
        adapter(&[DeliverySystem::DvbT]),
        adapter(&[DeliverySystem::DvbC, DeliverySystem::DvbC2]),
        adapter(&[DeliverySystem::DvbC2]),
    ];
    assert_eq!(
        TunerCounts::from_adapters(&adapters),
        TunerCounts {
            s2: 2,
            t: 1,
            c: 0,
            t2: 1,
            c2: 2,
            at: 0,
            ac: 0,
        }
    );
}

#[test]
fn template_substitution_matches_process_file() {
    let root = temp_dir();
    fs::write(
        root.join("satip.xml"),
        "<?xml version=\"1.0\"?><cap>$tuner_s2$,$tuner_t$,$tuner_c$</cap>$unknown$",
    )
    .expect("write template");
    let service = WebService::new(config(root.clone()));
    let mut snapshot = WebSnapshot::new();
    snapshot.adapters = vec![
        adapter(&[DeliverySystem::DvbS2]),
        adapter(&[DeliverySystem::DvbT]),
    ];
    let route = service.route(&get("/satip.xml"), &snapshot).expect("route");
    assert_eq!(
        route.body,
        b"<?xml version=\"1.0\"?><cap>1,1,0</cap>".to_vec()
    );
    assert!(route.headers.contains(&"Content-type: text/xml".to_owned()));
    fs::remove_dir_all(root).ok();
}

#[test]
fn template_skips_unknown_and_unterminated_variables() {
    // is_var/var_eval (variables.cpp:83-109): unknown names expand to nothing,
    // and a '$' with no closing '$' within 20 bytes is copied.
    let service = WebService::new(config(PathBuf::from("html")));
    let snapshot = WebSnapshot::new();
    let run = |data: &[u8]| service.with_symbols(&snapshot, |table| table.process_file(data).0);
    assert_eq!(run(b"a$version$b"), b"a1.2.3b".to_vec());
    assert_eq!(run(b"a$unknown$b"), b"ab".to_vec());
    assert_eq!(run(b"a$unterminated"), b"a$unterminated".to_vec());
    assert_eq!(
        run(b"$abcdefghijklmnopqrstu$"),
        b"$abcdefghijklmnopqrstu$".to_vec()
    );
}

#[test]
fn root_serves_status_html_and_missing_files_are_404() {
    let root = temp_dir();
    fs::write(root.join("status.html"), b"<html>ok</html>").expect("write");
    fs::write(root.join("sm.png"), b"\x89PNG\r\n\x1a\n").expect("write");
    let service = WebService::new(config(root.clone()));
    let snapshot = WebSnapshot::new();

    let route = service.route(&get("/"), &snapshot).expect("route");
    assert_eq!(route.status, 200);
    assert_eq!(route.body, b"<html>ok</html>");
    assert!(
        route
            .headers
            .contains(&"Content-type: text/html; charset=utf-8".to_owned())
    );

    let image = service.route(&get("/sm.png"), &snapshot).expect("route");
    assert_eq!(image.body, b"\x89PNG\r\n\x1a\n");

    let missing = service.route(&get("/nope.html"), &snapshot).expect("route");
    assert_eq!(missing.status, 404);
    assert!(missing.headers.is_empty());
    assert!(missing.body.is_empty());

    // The HEAD branch returns headers without a body (read_http:1658-1661).
    let head_route = service.route(&head("/sm.png"), &snapshot).expect("route");
    assert_eq!(head_route.status, 200);
    assert!(head_route.body.is_empty());
    assert!(!head_route.body_on_head);

    fs::remove_dir_all(root).ok();
}

#[test]
fn static_name_without_leading_slash_is_served() {
    // minisatip.cpp:1647-1657 guards on `url.substr(1)` but readfile uses
    // the original name, so `GET status.html` opens html/status.html.
    let root = temp_dir();
    fs::write(root.join("status.html"), b"ok").expect("write");
    let service = WebService::new(config(root.clone()));
    let snapshot = WebSnapshot::new();
    let route = service
        .route(&get("status.html"), &snapshot)
        .expect("route");
    assert_eq!(route.status, 200);
    assert_eq!(route.body, b"ok");
    fs::remove_dir_all(root).ok();
}

#[test]
fn traversal_and_nested_paths_are_rejected() {
    // read_http:1647-1648 rejects any '/' after the first and any "..".
    let root = temp_dir();
    fs::write(root.join("status.html"), b"ok").expect("write");
    let service = WebService::new(config(root.clone()));
    let snapshot = WebSnapshot::new();
    for uri in ["/../etc/passwd", "/sub/dir", "/..", "/sub/../status.html"] {
        let route = service.route(&get(uri), &snapshot).expect("route");
        assert_eq!(route.status, 404, "uri {uri}");
    }
    fs::remove_dir_all(root).ok();
}

#[test]
fn state_json_uses_cpp_symbol_order_and_values() {
    let service = WebService::new(config(PathBuf::from("html")));
    let mut snapshot = WebSnapshot::new();
    let mut tuned = adapter(&[DeliverySystem::DvbS2]);
    tuned.freq = 11_362_000;
    tuned.sr = 22_000;
    tuned.pol = 2;
    tuned.master = 3;
    tuned.sid_count = 1;
    tuned.sys = 6;
    tuned.pids = vec![0, 16];
    snapshot.adapters = vec![tuned];
    snapshot.streams = vec![StreamState {
        enabled: true,
        play: true,
        adapter: 0,
        user_agent: "Lavf/60".to_owned(),
        rhost: "10.0.0.9".to_owned(),
        rport: 5000,
        protocol: "HTTP".to_owned(),
        pids: vec![0, 16],
        ..StreamState::default()
    }];
    let json = service.state_json(&snapshot);
    assert!(json.starts_with("{\n\"ad_enabled\":[1,0,0,"));
    assert!(json.contains("\"ad_type\":[1,0,0,"));
    assert!(json.contains("\"ad_freq\":[11362,0,0,"));
    assert!(json.contains("\"ad_sr\":[22,0,0,"));
    // adapter.cpp:2399-2410: QAM_AUTO, GUARD_INTERVAL_AUTO and `ds` defaults.
    assert!(json.contains("\"ad_mtype\":[6,6,6,"));
    assert!(json.contains("\"ad_gi\":[4,4,4,"));
    assert!(json.contains("\"ad_plp\":[-1,-1,-1,"));
    assert!(json.contains("\"ad_pids\":[\"0,16\",\"\",\"\","));
    assert!(json.contains("\"tuner_s2\":1,"));
    assert!(json.contains("\"st_enabled\":[1,0,0,"));
    assert!(json.contains("\"st_proto\":[\"HTTP\",\"\",\"\","));
    assert!(json.contains("\"st_pids\":[\"0,16\",\"\",\"\","));
    // minisatip.cpp:2181: has_pmt is 1 unless built with DISABLE_PMT.
    assert!(json.contains("\"has_pmt\":1,"));
    assert!(json.contains("\"name_app\":\"minisatip\","));
    assert!(json.contains("\"deviceid\":2,"));
    // minisatip_sym is followed by ca_sym; pmt_sym ends the list (symbols.cpp:27-41).
    assert!(json.contains("\"command_line\":\"\",\n\"ca_enabled\":["));
    assert!(json.ends_with("\"\"]\n}\n"));
    assert_eq!(json.matches("\"ad_enabled\":[").count(), 1);
}

#[test]
fn state_json_scales_symbols_through_cpp_float_multiplier() {
    let service = WebService::new(config(PathBuf::from("html")));
    let mut snapshot = WebSnapshot::new();
    let mut tuned = adapter(&[DeliverySystem::DvbS2]);
    // snprintf_pointer casts `(int)(value * 1.0f / 1000.0f)`, which rounds
    // these up; plain integer division would report one less.
    tuned.freq = 11_361_999;
    tuned.sr = 22_000_999;
    tuned.bw = 22_000_999;
    snapshot.adapters = vec![tuned];

    let json = service.state_json(&snapshot);
    assert!(json.contains("\"ad_freq\":[11362,0,0,"));
    assert!(json.contains("\"ad_sr\":[22001,0,0,"));
    // Absent adapter slots keep the DVB-T default bandwidth (8 MHz), so only
    // the first element is checked here.
    assert!(json.contains("\"ad_bw\":[22001,"));
}

#[test]
fn handler_serves_generated_and_static_resources() {
    let root = temp_dir();
    fs::write(root.join("status.html"), b"status").expect("write");
    let mut manager = SessionManager::new();
    manager.set_web(WebService::new(config(root.clone())));
    let request = get("/desc.xml");
    let mut ctx = HandlerContext::new(&mut manager, Protocol::Http);
    let response = handle_http(&mut ctx, &request).expect("response");
    assert_eq!(response.status, 200);
    let expected = WebService::new(config(PathBuf::from("unused")))
        .desc_xml(&WebSnapshot::new())
        .into_bytes();
    assert_eq!(response.body, expected);

    let request = get("/");
    let mut ctx = HandlerContext::new(&mut manager, Protocol::Http);
    let response = handle_http(&mut ctx, &request).expect("response");
    assert_eq!(response.body, b"status");

    fs::remove_dir_all(root).ok();
}

#[test]
fn html_directory_is_served_byte_for_byte() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../html");
    if !FILES.iter().all(|name| root.join(name).is_file()) {
        eprintln!("skipping asset comparison: html/ has not been copied into the repository");
        return;
    }
    let service = WebService::new(config(root.clone()));
    let snapshot = WebSnapshot::new();
    for name in FILES {
        let source = fs::read(root.join(name)).expect("read asset");
        let route = service
            .route(&get(&format!("/{name}")), &snapshot)
            .expect("route");
        assert_eq!(route.status, 200, "{name}");
        if name.ends_with(".xml") || name.ends_with(".html") {
            let text = String::from_utf8(source).expect("text asset");
            let expected = text
                .replace("$version$", "1.2.3")
                .replace("$uuid$", "1234-5678")
                .replace("$http_host$", "10.0.0.5:8080")
                .replace("$tuner_s2$", "0")
                .replace("$tuner_t$", "0")
                .replace("$tuner_c$", "0")
                .replace("$tuner_c2$", "0")
                .replace("$tuner_t2$", "0");
            // process_file flushes past 8200 bytes (utils.cpp:397-420); the
            // streamed response, not the route body, carries the "\r\n\r\n".
            assert_eq!(route.streamed, expected.len() > 8200, "{name}");
            assert_eq!(route.body, expected.into_bytes(), "{name}");
        } else {
            assert_eq!(route.body, source, "{name}");
        }
    }
}

#[test]
fn forced_adapter_counts_override_desc_and_state() {
    // adapter.cpp:478-487: `if (opts.force_sadapter) tuner_s2 = ...` pins the
    // server/slave/ca counts even when no adapter supports them.
    let mut cfg = config(PathBuf::from("html"));
    cfg.force_sadapter = 3;
    cfg.force_tadapter = 2;
    cfg.force_cadapter = 1;
    let service = WebService::new(cfg);
    let snapshot = WebSnapshot::new();
    let xml = service.desc_xml(&snapshot);
    assert!(xml.contains("DVBS2-3,"), "{xml}");
    assert!(xml.contains("DVBT-2,"), "{xml}");
    assert!(xml.contains("DVBC-1"), "{xml}");
    let json = service.state_json(&snapshot);
    assert!(json.contains("\"tuner_s2\":3,"), "{json}");
    assert!(json.contains("\"tuner_t\":2,"), "{json}");
    assert!(json.contains("\"tuner_c\":1,"), "{json}");
}

#[test]
fn no_enabled_adapters_advertise_zero_tuners() {
    // enable_adapters disables every slot first (adapter.cpp:1626-1636); an
    // empty `-e` therefore leaves no tuners.
    let service = WebService::new(config(PathBuf::from("html")));
    let snapshot = WebSnapshot::new();
    assert!(
        service
            .desc_xml(&snapshot)
            .contains(">DVBS2-0</satip:X_SATIPCAP>")
    );
    assert!(service.state_json(&snapshot).contains("\"tuner_s2\":0,"));
}

#[test]
fn desc_xml_is_truncated_to_the_cpp_two_kilobyte_buffer() {
    // minisatip.cpp:1462,1601: `char buf[2000]` and snprintf, so at most 1999
    // bytes survive; a long playlist is what reaches the limit.
    let mut cfg = config(PathBuf::from("html"));
    cfg.playlist = Some("x".repeat(4000));
    let service = WebService::new(cfg);
    let full = service.desc_xml(&WebSnapshot::new()).into_bytes();
    assert!(full.len() > 1999);
    let route = service
        .route(&get("/desc.xml"), &WebSnapshot::new())
        .expect("route");
    assert_eq!(route.body.len(), 1999);
    assert_eq!(route.body, full[..1999]);
}

#[test]
fn large_html_files_stream_without_content_length() {
    // utils.cpp:399-419: after `io > sizeof(outp) - 100` (8200) the response
    // is sent without Content-Length and the body ends with CRLFCRLF.
    let root = temp_dir();
    let mut content = String::from("$version$");
    content.push_str(&"x".repeat(8200));
    fs::write(root.join("big.html"), content.as_bytes()).expect("write");
    let mut manager = SessionManager::new();
    manager.set_web(WebService::new(config(root.clone())));
    let request = get("/big.html");
    let mut ctx = HandlerContext::new(&mut manager, Protocol::Http);
    let response = handle_http(&mut ctx, &request).expect("response");
    assert_eq!(response.status, 200);
    assert!(!response.body.is_empty());
    let bytes = response.to_bytes();
    let text = String::from_utf8(bytes).expect("utf-8");
    assert!(!text.contains("Content-Length:"), "{text}");
    let (_, body) = text.split_once("\r\n\r\n").expect("header end");
    // Exactly one terminator after the content (utils.cpp:415-417).
    assert!(body.ends_with("x\r\n\r\n"), "streamed body terminator");
    assert_eq!(body.len(), "1.2.3".len() + 8200 + 4);
    fs::remove_dir_all(root).ok();
}

#[test]
fn bandwidth_json_prints_the_folded_counters() {
    // api/variables.cpp:305-324: get_json_bandwidth formats c_bw, c_bw_dmx,
    // c_tbw, c_reads, c_writes, c_failed_writes, c_ns_read, c_tt, c_buffered
    // and c_dropped in this order.
    let service = WebService::new(config(PathBuf::from("html")));
    let mut snapshot = WebSnapshot::new();
    snapshot.bandwidth = BandwidthSnapshot {
        bw: 12,
        dmx: 340,
        tbw: 5,
        reads: 7,
        writes: 9,
        fwrites: 2,
        ns_read: 1100,
        tt: 8,
        buffered: 4096,
        dropped: 512,
    };
    let route = service
        .route(&get("/bandwidth.json"), &snapshot)
        .expect("route");
    assert_eq!(route.status, 200);
    assert_eq!(
        String::from_utf8(route.body).expect("utf-8"),
        concat!(
            "{\n",
            "\"bw\":12,\n",
            "\"dmx\":340,\n",
            "\"tbw\":5,\n",
            "\"reads\":7,\n",
            "\"writes\":9,\n",
            "\"fwrites\":2,\n",
            "\"ns_read\":1100,\n",
            "\"tt\":8,\n",
            "\"buffered\":4096,\n",
            "\"dropped\":512\n",
            "}"
        )
    );
}
