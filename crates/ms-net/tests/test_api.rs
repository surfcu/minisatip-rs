//! Status/variables API engine and symbol-table providers.
//!
//! Ported C++ tests are marked with their source location. The remaining
//! cases are Rust checks derived from variables.cpp/utils.cpp.

use std::path::PathBuf;
use std::time::{Duration, UNIX_EPOCH};

use ms_net::api::{
    Raw, Symbol, SymbolTable, VAR_AARRAY_STRING, VAR_ARRAY_INT8, VAR_FLOAT, VAR_FUNCTION_INT,
    VAR_FUNCTION_INT64, VAR_FUNCTION_STRING, VAR_HEX, VAR_INT, VAR_INT8, VAR_INT16, VAR_INT64,
    VAR_PSTRING, VAR_STRING, VAR_UINT8, VAR_UINT16, cstr, is_var, parse_int, snprintf_pointer,
};
use ms_net::web::{
    ADAPTER_DVB, AdapterState, ApiFeatures, BandwidthCounters, CaDeviceState, KeyChannel, KeyState,
    PmtState, SatipcState, StreamState, WebConfig, WebService, WebSnapshot, format_asctime,
};
use ms_tuner::DeliverySystem;

fn config() -> WebConfig {
    WebConfig {
        document_root: PathBuf::from("html"),
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

fn service() -> WebService {
    WebService::new(config())
}

fn read(service: &WebService, state: &WebSnapshot, name: &str, index: i32, max: usize) -> Raw {
    service.with_symbols(state, |table| {
        table
            .symbol(name)
            .unwrap_or_else(|| panic!("missing symbol {name}"))
            .read(index, max)
    })
}

fn text(raw: Raw) -> Vec<u8> {
    match raw {
        Raw::Str(bytes) => cstr(&bytes).to_vec(),
        other => panic!("expected a string, got {other:?}"),
    }
}

/// Top-level JSON value for `name`, without the key.
fn json_field<'a>(json: &'a str, name: &str) -> &'a str {
    let key = format!("\"{name}\":");
    let start = json.find(&key).unwrap_or_else(|| panic!("missing {name}")) + key.len();
    let rest = &json[start..];
    let end = rest
        .find(",\n\"")
        .or_else(|| rest.find("\n}"))
        .unwrap_or(rest.len());
    &rest[..end]
}

fn thousands() -> f32 {
    (1.0_f64 / 1000.0) as f32
}

#[test]
fn test_version_var_eval() {
    // Ported from tests/test_minisatip.cpp:42-59.
    let service = service();
    let state = WebSnapshot::new();
    let version = service.config().version.clone();
    assert!(!version.is_empty(), "version should not be empty");

    let var = b"$version$";
    let len = is_var(var);
    assert!(len > 0, "$version$ should be recognized as a variable");

    let dest = service.with_symbols(&state, |table| table.var_eval(&var[1..len], 255));
    assert!(
        !dest.is_empty(),
        "var_eval should produce output for $version$"
    );
    assert_eq!(
        dest,
        version.as_bytes(),
        "$version$ should expand to the version string"
    );
}

#[test]
fn test_get_ca_caids_string() {
    // Ported from tests/test_ca.cpp:770-796.
    let service = service();
    let caids = |list: &[u16]| {
        let mut state = WebSnapshot::new();
        state.ca = vec![Some(CaDeviceState {
            caids: list.to_vec(),
            ..CaDeviceState::default()
        })];
        text(read(&service, &state, "ca_caids", 0, 64))
    };

    assert_eq!(caids(&[]), b"", "invalid empty CAID string");
    assert_eq!(caids(&[0x0B00]), b"0B00", "invalid single CAID string");
    assert_eq!(
        caids(&[0x0B00, 0x0B01, 0x0B02]),
        b"0B00, 0B01, 0B02",
        "invalid single CAID string"
    );
}

#[test]
fn test_channel_falls_back_to_sid() {
    // Ported from tests/test_dvbapi.cpp:174-204.
    let service = service();
    // C++ also asserts keys_add() returns a valid id; ms-net has no key
    // table yet, so the state is constructed directly.
    let channel = |name: &str| {
        let mut state = WebSnapshot::new();
        state.keys = vec![Some(KeyState {
            enabled: 1,
            channel: Some(KeyChannel {
                name: name.to_owned(),
                sid: 1234,
            }),
            ..KeyState::default()
        })];
        text(read(&service, &state, "key_channel", 0, 64))
    };

    assert_eq!(
        channel(""),
        b"SID 1234",
        "channel must fall back to the SID without SDT"
    );
    assert_eq!(
        channel("Test Channel"),
        b"Test Channel",
        "channel must show the SDT name when present"
    );
}

#[test]
fn snprintf_pointer_matches_cpp_types_and_scaling() {
    // variables.cpp:30-81.
    let print = |ty, raw: Raw, multiplier| snprintf_pointer(ty, &raw, multiplier);
    assert_eq!(print(VAR_UINT8, Raw::Int(-1), 1.0), b"255");
    assert_eq!(print(VAR_INT8, Raw::Int(-1), 1.0), b"-1");
    assert_eq!(print(VAR_UINT16, Raw::Int(65535), 1.0), b"65535");
    assert_eq!(print(VAR_INT16, Raw::Int(-2), 1.0), b"-2");
    assert_eq!(print(VAR_INT, Raw::Int(42), 2.5), b"105");
    // C++ multiplies the int64 by a float before the cast (variables.cpp:47).
    assert_eq!(
        print(VAR_INT64, Raw::Int(9_000_000_000), 1.0),
        b"8999999488"
    );
    assert_eq!(print(VAR_STRING, Raw::text("abc"), 1.0), b"abc");
    assert_eq!(print(VAR_PSTRING, Raw::text("x"), 1.0), b"x");
    assert_eq!(print(VAR_FLOAT, Raw::Float(1.5), 2.0), b"3.000000");
    assert_eq!(print(VAR_HEX, Raw::Int(255), 1.0), b"0xff");
    assert_eq!(print(VAR_HEX, Raw::Int(-1), 1.0), b"0xffffffff");
    assert_eq!(print(VAR_INT, Raw::Int(11_361_999), thousands()), b"11362");
    assert_eq!(print(VAR_INT, Raw::Int(22_000_999), thousands()), b"22001");
}

#[test]
fn is_var_matches_cpp_boundaries() {
    // variables.cpp:98-109.
    assert_eq!(is_var(b"$version$"), 8);
    assert_eq!(is_var(b"$$"), 1);
    assert_eq!(is_var(b"$"), 0);
    assert_eq!(is_var(b"abc$d$"), 0);
    assert_eq!(is_var(b"$abcdefghijklmnopqrs$"), 20);
    assert_eq!(is_var(b"$abcdefghijklmnopqrst$"), 0);
    assert_eq!(is_var(b"$abc"), 0);
    assert_eq!(is_var(b"$a\0b$"), 0);
}

#[test]
fn parse_int_matches_from_chars() {
    // utils.cpp:102-124; the default value distinguishes failures.
    assert_eq!(parse_int(b"12", 7), 12);
    assert_eq!(parse_int(b" \t-12abc", 7), -12);
    assert_eq!(parse_int(b"+7", 7), 7);
    assert_eq!(parse_int(b"", 7), 7);
    assert_eq!(parse_int(b"x", 7), 7);
    assert_eq!(parse_int(b"99999999999999999999", 7), 7);
    assert_eq!(parse_int(b"-2147483648", 7), i32::MIN);
    assert_eq!(parse_int(b"2147483648", 7), 7);
    assert_eq!(parse_int(b"1x", 7), 1);
}

#[test]
fn get_var_address_uses_prefixes_and_indexes() {
    // variables.cpp:113-169.
    let mut table = SymbolTable::new();
    table.push(vec![
        Symbol::scalar("pfx", VAR_INT, 1.0, Raw::Int(1)),
        Symbol::scalar("pfx_two", VAR_INT, 1.0, Raw::Int(2)),
        Symbol::new("arr", VAR_ARRAY_INT8, 1.0, 3, |i, _| {
            Raw::Int([10, 20, 30][i as usize])
        }),
        Symbol::new("strs", VAR_AARRAY_STRING, 1.0, 2, |i, _| {
            Raw::text(if i == 0 { "a" } else { "" })
        }),
        Symbol::new("fi", VAR_FUNCTION_INT, 0.0, 4, |i, _| {
            Raw::Int(i64::from(i))
        }),
        Symbol::new("fm", VAR_FUNCTION_INT, 2.0, 1, |_, _| Raw::Int(5)),
        Symbol::new("f64", VAR_FUNCTION_INT64, 0.0, 1, |_, _| {
            Raw::Int(5_000_000_000)
        }),
        Symbol::new("fs", VAR_FUNCTION_STRING, 0.0, 1, |_, _| Raw::text("str")),
    ]);

    // The first matching name is a prefix, so "pfx_two" resolves through "pfx".
    assert_eq!(
        table.get_var_address(b"pfx_two"),
        Some((VAR_INT, Raw::Int(1), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"arr0"),
        Some((VAR_ARRAY_INT8, Raw::Int(10), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"arr2"),
        Some((VAR_ARRAY_INT8, Raw::Int(30), 1.0))
    );
    assert_eq!(table.get_var_address(b"arr3"), None);
    assert_eq!(
        table.get_var_address(b"arrX"),
        Some((VAR_ARRAY_INT8, Raw::Int(10), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"strs0"),
        Some((VAR_AARRAY_STRING, Raw::text("a"), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"strs1"),
        Some((VAR_AARRAY_STRING, Raw::text(""), 1.0))
    );
    assert_eq!(table.get_var_address(b"strs2"), None);
    assert_eq!(
        table.get_var_address(b"fi2"),
        Some((VAR_FUNCTION_INT, Raw::Int(2), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"fm0"),
        Some((VAR_FUNCTION_INT, Raw::Int(5), 2.0))
    );
    assert_eq!(
        table.get_var_address(b"f64"),
        Some((VAR_FUNCTION_INT64, Raw::Int(5_000_000_000), 1.0))
    );
    assert_eq!(
        table.get_var_address(b"fs"),
        Some((VAR_FUNCTION_STRING, Raw::text("str"), 0.0))
    );
    assert_eq!(table.get_var_address(b"unknown"), None);
}

#[test]
fn var_eval_truncates_like_snprintf_pointer() {
    // variables.cpp:30-96: the returned count includes the NUL on truncation.
    let mut table = SymbolTable::new();
    table.push(vec![Symbol::scalar(
        "s",
        VAR_STRING,
        1.0,
        Raw::text("abcdef"),
    )]);
    assert_eq!(table.var_eval(b"s", 0), b"");
    assert_eq!(table.var_eval(b"s", 1), b"\0");
    assert_eq!(table.var_eval(b"s", 4), b"abc\0");
    assert_eq!(table.var_eval(b"s", 6), b"abcde\0");
    assert_eq!(table.var_eval(b"s", 7), b"abcdef");
    assert_eq!(table.var_eval(b"unknown", 7), b"");
}

#[test]
fn json_state_engine_emits_cpp_bytes() {
    // variables.cpp:189-296, one symbol of each kind.
    let mut table = SymbolTable::new();
    table.push(vec![
        Symbol::scalar("i", VAR_INT, 2.0, Raw::Int(21)),
        Symbol::scalar("s", VAR_STRING, 0.0, Raw::text("hi")),
        Symbol::scalar("p", VAR_PSTRING, 0.0, Raw::text("")),
        Symbol::scalar("h", VAR_HEX, 1.0, Raw::Int(255)),
        Symbol::scalar("f", VAR_FLOAT, 1.0, Raw::Float(1.5)),
        Symbol::new("ai", VAR_ARRAY_INT8, 1.0, 3, |i, _| {
            Raw::Int([1, -2, 3][i as usize])
        }),
        Symbol::new("as", VAR_AARRAY_STRING, 1.0, 2, |i, _| {
            Raw::text(if i == 0 { "x" } else { "" })
        }),
        Symbol::new("fi", VAR_FUNCTION_INT, 0.0, 2, |i, _| {
            Raw::Int(7 + i64::from(i))
        }),
        Symbol::new("f64", VAR_FUNCTION_INT64, 0.0, 2, |i, _| {
            Raw::Int(9 + i64::from(i))
        }),
        Symbol::new("fs", VAR_FUNCTION_STRING, 0.0, 2, |i, _| {
            Raw::text(if i == 0 { "a" } else { "b" })
        }),
    ]);
    assert_eq!(
        table.json_state(),
        b"{\n\"i\":42,\n\"s\":\"hi\",\n\"p\":\"\",\n\"h\":\"0xff\",\n\"f\":1.500000,\n\
          \"ai\":[1,-2,3],\n\"as\":[\"x\",\"\"],\n\"fi\":[7,8],\n\"f64\":[9,10],\n\
          \"fs\":[\"a\",\"b\"]\n}\n"
    );
}

#[test]
fn escape_json_reproduces_cpp_control_character_quirk() {
    // variables.cpp:171-187: strlcatf sizes the escape with the source length.
    let mut table = SymbolTable::new();
    table.push(vec![Symbol::scalar(
        "c",
        VAR_STRING,
        0.0,
        Raw::Str(b"a\nb\tc".to_vec()),
    )]);
    assert_eq!(table.json_state(), b"{\n\"c\":\"a%xc\"\n}\n");

    let mut lone = SymbolTable::new();
    lone.push(vec![Symbol::scalar(
        "c",
        VAR_STRING,
        0.0,
        Raw::Str(b"\n".to_vec()),
    )]);
    assert_eq!(lone.json_state(), b"{\n\"c\":\"\n}\n");
}

#[test]
fn json_state_truncates_like_strlcatf() {
    // strlcatf (utils.h:176-185) and snprintf_pointer keep the NUL byte.
    let mut table = SymbolTable::new();
    table.push(vec![Symbol::scalar("i", VAR_INT, 1.0, Raw::Int(42))]);
    assert_eq!(table.json_state_sized(8, 1024), b"{\n\"i\":4");
    assert_eq!(table.json_state_sized(1, 1024), b"");
}

#[test]
fn process_file_handles_expansion_flush_and_nul() {
    // utils.cpp:379-421.
    let mut table = SymbolTable::new();
    table.push(vec![
        Symbol::scalar("v", VAR_STRING, 0.0, Raw::text("X")),
        Symbol::scalar("big", VAR_STRING, 0.0, Raw::Str(vec![b'y'; 20_000])),
    ]);
    assert_eq!(table.process_file(b"a$v$b").0, b"aXb");
    assert_eq!(table.process_file(b"ab\0cd").0, b"ab");

    let (short, flushed) = table.process_file(&vec![b'x'; 8_200]);
    assert_eq!(short.len(), 8_200);
    assert!(!flushed);

    let (long, flushed) = table.process_file(&vec![b'x'; 8_201]);
    assert!(flushed);
    assert_eq!(long.len(), 8_205);
    assert_eq!(&long[..8_201], &vec![b'x'; 8_201][..]);
    assert_eq!(&long[8_201..], b"\r\n\r\n");

    let (huge, flushed) = table.process_file(b"$big$");
    assert!(flushed);
    assert_eq!(huge.len(), 8_294);
    assert!(huge[..8_289].iter().all(|byte| *byte == b'y'));
    assert_eq!(huge[8_289], 0);
    assert_eq!(&huge[8_290..], b"\r\n\r\n");
}

#[test]
fn bandwidth_json_matches_cpp_format() {
    // variables.cpp:298-324.
    let mut state = WebSnapshot::new();
    state.bandwidth = BandwidthCounters {
        bw: -1,
        dmx: 2,
        tbw: 3,
        reads: 4,
        writes: 5,
        fwrites: 6,
        ns_read: 7,
        tt: 8,
        buffered: 9,
        dropped: 10,
    };
    assert_eq!(
        service().bandwidth_json(&state),
        "{\n\"bw\":-1,\n\"dmx\":2,\n\"tbw\":3,\n\"reads\":4,\n\"writes\":5,\n\
         \"fwrites\":6,\n\"ns_read\":7,\n\"tt\":8,\n\"buffered\":9,\n\"dropped\":10\n}"
    );
}

#[test]
fn state_json_lists_every_cpp_symbol_in_order() {
    // symbols.cpp:27-41 and the seven tables in variables.h.
    let expected = [
        // adapters_sym
        "ad_enabled",
        "ad_type",
        "ad_pos",
        "ad_freq",
        "ad_strength",
        "ad_snr",
        "ad_db",
        "ad_ber",
        "ad_pol",
        "ad_sr",
        "ad_bw",
        "ad_stream",
        "ad_fe",
        "ad_master",
        "ad_sidcount",
        "ad_phyad",
        "ad_phyfd",
        "ad_sys",
        "ad_mtype",
        "ad_t2id",
        "ad_c2tft",
        "ad_plp",
        "ad_plsc",
        "ad_sm",
        "ad_gi",
        "ad_allsys",
        "ad_pids",
        "ad_ccerr",
        "ad_decerr",
        "tuner_s2",
        "tuner_t2",
        "tuner_c2",
        "tuner_t",
        "tuner_c",
        "tuner_ac",
        "tuner_at",
        "ad_disabled",
        "ad_adapter_names",
        // stream_sym
        "st_enabled",
        "st_play",
        "st_adapter",
        "st_useragent",
        "st_rhost",
        "st_rport",
        "st_proto",
        "st_pids",
        "st_overflow",
        "st_buffered",
        // minisatip_sym
        "has_pmt",
        "name_app",
        "http_host",
        "uuid",
        "bootid",
        "deviceid",
        "http_port",
        "rtsp_host",
        "datetime_compile",
        "datetime_start",
        "datetime_current",
        "time_running",
        "run_user",
        "run_pid",
        "version",
        "command_line",
        // ca_sym, dvbapi_sym, satipc_sym, pmt_sym
        "ca_enabled",
        "ca_state",
        "ca_caids",
        "ca_ci_name",
        "key_enabled",
        "key_hops",
        "key_ecmtime",
        "key_pmt",
        "key_adapter",
        "key_cardsystem",
        "key_reader",
        "key_from",
        "key_protocol",
        "key_channel",
        "ad_satip",
        "ad_satip_transport_type",
        "ad_channel",
        "ad_pmt",
    ];
    let json = service().state_json(&WebSnapshot::new());
    let mut names = Vec::new();
    for line in json.split('\n').skip(1) {
        if line.is_empty() || line == "}" {
            continue;
        }
        let line = line.strip_prefix(',').unwrap_or(line);
        let rest = line.strip_prefix('"').expect("quoted key");
        let end = rest.find("\":").expect("key terminator");
        names.push(&rest[..end]);
    }
    assert_eq!(names, expected);
}

#[test]
fn state_json_feature_flags_gate_optional_tables() {
    // symbols.cpp:27-41: each table is compiled in by a feature.
    let mut disabled = config();
    disabled.features = ApiFeatures {
        dvbca: false,
        dvbapi: false,
        satipc: false,
        pmt: false,
    };
    let json = WebService::new(disabled).state_json(&WebSnapshot::new());
    assert!(json.contains("\"has_pmt\":0,"));
    assert!(!json.contains("\"ca_enabled\""));
    assert!(!json.contains("\"key_enabled\""));
    assert!(!json.contains("\"ad_satip\""));
    assert!(!json.contains("\"ad_channel\""));

    let mut only_ca = config();
    only_ca.features = ApiFeatures {
        dvbca: true,
        dvbapi: false,
        satipc: false,
        pmt: false,
    };
    let json = WebService::new(only_ca).state_json(&WebSnapshot::new());
    assert!(json.contains("\"ca_enabled\":["));
    assert!(!json.contains("\"key_enabled\""));
}

#[test]
fn adapter_getter_defaults_match_cpp() {
    // adapter.cpp:2288-2410: get_adapter_nw returns NULL for every empty slot.
    let json = service().state_json(&WebSnapshot::new());
    for (name, first) in [
        ("ad_freq", "0"),
        ("ad_bw", "8000"),
        ("ad_stream", "-1"),
        ("ad_plp", "-1"),
        ("ad_plsc", "-1"),
        ("ad_mtype", "6"),
        ("ad_gi", "4"),
        ("ad_sys", "0"),
        ("ad_pos", "0"),
        ("ad_fe", "0"),
        ("ad_t2id", "0"),
        ("ad_c2tft", "0"),
        ("ad_sm", "0"),
    ] {
        let field = json_field(&json, name);
        assert!(
            field.starts_with(&format!("[{first},")),
            "{name} first value: {field}"
        );
    }
    assert!(json_field(&json, "ad_allsys").starts_with("[\"\","));
    assert!(json_field(&json, "ad_pids").starts_with("[\"\","));
    assert!(json_field(&json, "ad_adapter_names").starts_with("[\"\","));
}

#[test]
fn adapter_symbols_scale_and_truncate() {
    let mut state = WebSnapshot::new();
    state.adapters = vec![AdapterState {
        present: true,
        enabled: true,
        kind: ADAPTER_DVB,
        systems: vec![
            DeliverySystem::DvbS2,
            DeliverySystem::DvbT,
            DeliverySystem::DvbC,
        ],
        pos: 3,
        freq: 11_361_999,
        pol: 2,
        sr: 22_000_999,
        bw: 27_000_000,
        strength: 100,
        snr: 200,
        db: 30,
        ber: 40,
        stream: 4,
        fe: 2,
        master: 0x1FF,
        sid_count: 300,
        phy_adapter: 10,
        phy_frontend: 11,
        sys: 6,
        mtype: 9,
        t2id: 7,
        c2tft: 8,
        plp: 5,
        plsc: 6,
        sm: 1,
        gi: 2,
        pids: vec![0, 16],
        ccerr: 2,
        decerr: 3,
        name: "dev0".to_owned(),
        ..AdapterState::default()
    }];
    let json = service().state_json(&state);
    for (name, first) in [
        ("ad_enabled", "1"),
        ("ad_type", "1"),
        ("ad_pos", "3"),
        ("ad_freq", "11362"),
        ("ad_strength", "100"),
        ("ad_snr", "200"),
        ("ad_db", "30"),
        ("ad_ber", "40"),
        ("ad_pol", "2"),
        ("ad_sr", "22001"),
        ("ad_bw", "27000"),
        ("ad_stream", "4"),
        ("ad_fe", "2"),
        ("ad_master", "255"),
        ("ad_sidcount", "44"),
        ("ad_phyad", "10"),
        ("ad_phyfd", "11"),
        ("ad_sys", "6"),
        ("ad_mtype", "9"),
        ("ad_t2id", "7"),
        ("ad_c2tft", "8"),
        ("ad_plp", "5"),
        ("ad_plsc", "6"),
        ("ad_sm", "1"),
        ("ad_gi", "2"),
        ("ad_ccerr", "2"),
        ("ad_decerr", "3"),
        ("ad_disabled", "0"),
    ] {
        let field = json_field(&json, name);
        assert!(
            field.starts_with(&format!("[{first},")),
            "{name} first value: {field}"
        );
    }
    assert_eq!(
        json_field(&json, "ad_allsys"),
        format!("[\"dvbs2,dvbt,dvbc\",{}\"\"]", "\"\",".repeat(98))
    );
    assert!(json_field(&json, "ad_pids").starts_with("[\"0,16\","));
    assert!(json_field(&json, "ad_adapter_names").starts_with("[\"dev0\","));
    assert!(json.contains("\"tuner_s2\":1,"));
    assert!(json.contains("\"tuner_t\":1,"));
    assert!(json.contains("\"tuner_c\":1,"));
    assert!(json.contains("\"tuner_t2\":0,"));
    assert!(json.contains("\"tuner_c2\":0,"));
}

#[test]
fn adapter_pid_and_delsys_strings_match_cpp() {
    // adapter.cpp:2294-2338.
    let render = |systems: &[DeliverySystem], pids: &[i32]| {
        let mut state = WebSnapshot::new();
        state.adapters = vec![AdapterState {
            present: true,
            enabled: true,
            systems: systems.to_vec(),
            pids: pids.to_vec(),
            ..AdapterState::default()
        }];
        let json = service().state_json(&state);
        (
            json_field(&json, "ad_allsys").to_owned(),
            json_field(&json, "ad_pids").to_owned(),
        )
    };
    let (_, pids) = render(&[], &[]);
    assert!(pids.starts_with("[\"none\","));
    let (_, pids) = render(&[], &[8192, 16]);
    assert!(pids.starts_with("[\"all\","));
    let (_, pids) = render(&[], &[0, 16]);
    assert!(pids.starts_with("[\"0,16\","));
    let (systems, _) = render(&[DeliverySystem::DvbS], &[]);
    assert!(systems.starts_with("[\"dvbs\","));
    let (systems, _) = render(&[], &[]);
    assert!(systems.starts_with("[\"\","));
}

#[test]
fn stream_symbols_match_cpp_getters() {
    // stream.cpp:1472-1610.
    let mut state = WebSnapshot::new();
    state.adapters = vec![AdapterState {
        present: true,
        enabled: true,
        ..AdapterState::default()
    }];
    state.streams = vec![
        StreamState {
            enabled: true,
            play: true,
            adapter: 0,
            user_agent: "Lavf".to_owned(),
            rhost: "1.2.3.4".to_owned(),
            rport: 5000,
            protocol: "HTTP".to_owned(),
            pids: vec![0, 16],
            overflow: -1,
            buffered: 7,
        },
        StreamState {
            adapter: -1,
            user_agent: "closed".to_owned(),
            ..StreamState::default()
        },
    ];
    let json = service().state_json(&state);
    for (name, first) in [
        ("st_enabled", "1"),
        ("st_play", "1"),
        ("st_adapter", "0"),
        ("st_rport", "5000"),
        ("st_overflow", "-1"),
        ("st_buffered", "7"),
    ] {
        let field = json_field(&json, name);
        assert!(
            field.starts_with(&format!("[{first},")),
            "{name} first value: {field}"
        );
    }
    assert!(json_field(&json, "st_useragent").starts_with("[\"Lavf\","));
    assert!(json_field(&json, "st_rhost").starts_with("[\"1.2.3.4\","));
    assert!(json_field(&json, "st_proto").starts_with("[\"HTTP\","));
    assert!(json_field(&json, "st_pids").starts_with("[\"0,16\","));
    assert!(json_field(&json, "st_useragent").contains(",\"closed\","));
    assert!(json_field(&json, "st_rhost").contains(",\"\","));
    assert!(json_field(&json, "st_proto").contains(",\"\","));
    assert!(json_field(&json, "st_pids").contains(",\"\","));
    assert!(json_field(&json, "st_adapter").contains(",-1,"));
}

#[test]
fn ca_dvbapi_satipc_pmt_symbols_match_cpp() {
    let mut state = WebSnapshot::new();
    state.adapters = vec![AdapterState {
        present: true,
        enabled: true,
        pmts: vec![
            PmtState {
                running: true,
                name: "Ch".to_owned(),
                pid: 0x100,
            },
            PmtState {
                running: false,
                name: "Off".to_owned(),
                pid: 0x200,
            },
            PmtState {
                running: true,
                name: String::new(),
                pid: 0x300,
            },
        ],
        ..AdapterState::default()
    }];
    state.ca = vec![Some(CaDeviceState {
        enabled: 1,
        state: 2,
        caids: vec![0x0B00, 0x0B01],
        ci_name: "CAM".to_owned(),
    })];
    state.keys = vec![Some(KeyState {
        enabled: 1,
        hops: 2,
        ecmtime: 3,
        pmt_pid: 0x1000,
        adapter: 4,
        cardsystem: "Conax".to_owned(),
        reader: "reader".to_owned(),
        from: "from".to_owned(),
        protocol: "protocol".to_owned(),
        channel: Some(KeyChannel {
            name: String::new(),
            sid: 42,
        }),
    })];
    state.satipc = vec![Some(SatipcState {
        sip: "10.0.0.1".to_owned(),
        transport_type: 2,
    })];
    let json = service().state_json(&state);

    assert!(json_field(&json, "ca_enabled").starts_with("[1,"));
    assert!(json_field(&json, "ca_state").starts_with("[2,"));
    assert!(json_field(&json, "ca_caids").starts_with("[\"0B00, 0B01\","));
    assert!(json_field(&json, "ca_ci_name").starts_with("[\"CAM\","));
    for (name, first) in [
        ("key_enabled", "1"),
        ("key_hops", "2"),
        ("key_ecmtime", "3"),
        ("key_pmt", "4096"),
        ("key_adapter", "4"),
    ] {
        let field = json_field(&json, name);
        assert!(
            field.starts_with(&format!("[{first},")),
            "{name} first value: {field}"
        );
    }
    assert!(json_field(&json, "key_cardsystem").starts_with("[\"Conax\","));
    assert!(json_field(&json, "key_reader").starts_with("[\"reader\","));
    assert!(json_field(&json, "key_from").starts_with("[\"from\","));
    assert!(json_field(&json, "key_protocol").starts_with("[\"protocol\","));
    assert!(json_field(&json, "key_channel").starts_with("[\"SID 42\","));
    assert!(json_field(&json, "ad_satip").starts_with("[\"10.0.0.1\","));
    assert!(json_field(&json, "ad_satip_transport_type").starts_with("[2,"));
    assert!(json_field(&json, "ad_channel").starts_with("[\"Ch\","));
    assert!(json_field(&json, "ad_pmt").starts_with("[\"256,768\","));

    let empty = service().state_json(&WebSnapshot::new());
    assert!(json_field(&empty, "ad_channel").starts_with("[\"\","));
    assert!(json_field(&empty, "ad_pmt").starts_with("[\"\","));
    assert!(json_field(&empty, "key_channel").starts_with("[\"\","));
}

#[test]
fn templates_resolve_indexed_symbols() {
    // variables.cpp:83-169 through process_file (utils.cpp:379).
    let mut state = WebSnapshot::new();
    state.adapters = vec![AdapterState {
        present: true,
        enabled: true,
        kind: ADAPTER_DVB,
        systems: vec![DeliverySystem::DvbS2],
        freq: 11_362_000,
        sr: 22_000,
        pids: vec![0],
        ..AdapterState::default()
    }];
    state.ca = vec![Some(CaDeviceState {
        enabled: 1,
        caids: vec![0x0B00],
        ..CaDeviceState::default()
    })];
    state.keys = vec![Some(KeyState {
        enabled: 1,
        ..KeyState::default()
    })];
    state.satipc = vec![Some(SatipcState {
        sip: "10.0.0.1".to_owned(),
        ..SatipcState::default()
    })];
    state.streams = vec![StreamState {
        enabled: true,
        adapter: 0,
        protocol: "HTTP".to_owned(),
        pids: vec![0],
        ..StreamState::default()
    }];
    let template = b"$ad_enabled0$|$ad_freq0$|$ad_sr0$|$tuner_s2$|$ca_enabled0$|\
                     $ca_caids0$|$key_enabled0$|$ad_satip0$|$st_enabled0$|$st_proto0$|$ad_pids0$";
    let out = service().with_symbols(&state, |table| table.process_file(template).0);
    assert_eq!(out, b"1|11362|22|1|1|0B00|1|10.0.0.1|1|HTTP|0");
    // A longer sibling name is rejected by is_var; a short suffix still hits
    // the "ad_satip" AARRAY prefix and parses as index 0 (variables.cpp:109).
    let long = service().with_symbols(&state, |table| {
        table.process_file(b"$ad_satip_transport_type0$").0
    });
    assert_eq!(long, b"$ad_satip_transport_type0$");
    let quirk = service().with_symbols(&state, |table| table.process_file(b"$ad_satipx$").0);
    assert_eq!(quirk, b"10.0.0.1");
}

#[test]
fn asctime_has_cpp_layout() {
    // minisatip.cpp:1155-1158 replaces the newline with a space.
    let time = UNIX_EPOCH + Duration::from_secs(1_700_000_000);
    let text = format_asctime(time);
    assert_eq!(text.len(), 25, "{text:?}");
    for space in [3, 10, 19, 24] {
        assert_eq!(&text[space..space + 1], " ", "{text:?}");
    }
    assert!(text[20..24].chars().all(|c| c.is_ascii_digit()), "{text:?}");
}
