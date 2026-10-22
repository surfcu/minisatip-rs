//! Table-driven coverage of every option, plus getopt_long edge cases.

use ms_config::{
    ADAPTER_BUFFER, ALL_LOG_MODULES, DeliverySystem, DropEncrypted, OptError, Options,
    ParseOutcome, UnicableKind, parse_args,
};

type OptionCase = (&'static [&'static str], fn(&Options));

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

fn parse_outcome(values: &[&str]) -> ParseOutcome {
    parse_args(&args(values))
}

fn parse(values: &[&str]) -> Options {
    parse_outcome(values)
        .into_options()
        .expect("arguments should parse")
}

fn approx(left: f32, right: f32) -> bool {
    (left - right).abs() < 1e-6
}

#[test]
fn short_options_table() {
    let cases: &[OptionCase] = &[
        (&["-r", "239.1.1.1"], |o| {
            assert_eq!(o.rrtp.as_deref(), Some("239.1.1.1"))
        }),
        (&["-N"], |o| assert!(o.disable_dvb)),
        (&["-G"], |o| assert!(o.disable_ssdp)),
        (&["-V", "10.0.0.1"], |o| {
            assert_eq!(o.bind.as_deref(), Some("10.0.0.1"))
        }),
        (&["-U", "10.0.0.2"], |o| {
            assert_eq!(o.bind_http.as_deref(), Some("10.0.0.2"))
        }),
        (&["-J", "eth0"], |o| {
            assert_eq!(o.bind_dev.as_deref(), Some("eth0"))
        }),
        (&["-w", "192.168.1.1:8080"], |o| {
            assert_eq!(o.http_host.as_deref(), Some("192.168.1.1:8080"))
        }),
        (&["-x", "9090"], |o| assert_eq!(o.http_port, 9090)),
        (&["-l", "http,pmt"], |o| {
            assert_eq!(o.log, 1 | (1 << 1) | (1 << 6));
            assert_eq!(o.debug, 0);
        }),
        (&["-v", "http"], |o| {
            assert_eq!(o.log, 1 | (1 << 1));
            assert_eq!(o.debug, 1 << 1);
        }),
        (&["-F", "/tmp/x.log"], |o| {
            assert_eq!(o.log_file, "/tmp/x.log")
        }),
        (&["-h"], |o| assert!(o.help)),
        (&["-p", "http://example/list"], |o| {
            assert_eq!(
                o.playlist.as_deref(),
                Some(
                    "<satip:X_SATIPM3U xmlns:satip=\"urn:ses-com:satip\">http://example/list</satip:X_SATIPM3U>\r\n"
                )
            )
        }),
        (&["-a", "1:2:3"], |o| {
            assert_eq!(
                (o.force_sadapter, o.force_tadapter, o.force_cadapter),
                (1, 2, 3)
            )
        }),
        (&["-f"], |o| assert_eq!(o.daemon, 0)),
        (&["-b", "18800:18988"], |o| {
            assert_eq!(o.adapter_buffer, 18800);
            assert_eq!(o.dvr_buffer, 18988);
        }),
        (&["-B", "20"], |o| assert_eq!(o.max_sbuf, 20)),
        (&["-H", "5:50"], |o| {
            assert_eq!(o.udp_threshold, 5);
            assert_eq!(o.tcp_threshold, 50);
        }),
        (&["-e", "0-2,5"], |o| {
            assert_eq!(o.enabled_adapters.as_deref(), Some(&[0, 1, 2, 5][..]))
        }),
        (&["-u", "2:0-1284-1111"], |o| {
            let entry = o.unicable[0];
            assert_eq!(entry.adapter, 2);
            assert_eq!(entry.kind, UnicableKind::Unicable);
            assert_eq!((entry.slot, entry.freq, entry.pin), (0, 1284, 1111));
            assert!(!entry.only_13v);
        }),
        (&["-j", "2:1-1400"], |o| {
            let entry = o.unicable[0];
            assert_eq!(entry.kind, UnicableKind::Jess);
            assert_eq!((entry.slot, entry.freq, entry.pin), (1, 1400, -1));
        }),
        (&["-d", "0:1-0"], |o| {
            let entry = o.diseqc[0];
            assert_eq!(entry.adapter, 0);
            assert!(!entry.fast);
            assert_eq!(entry.addr, 0x10);
            assert_eq!((entry.committed_no, entry.uncommitted_no), (1, 0));
        }),
        (&["-q", "0:15-54-15-15-15-0"], |o| {
            let entry = o.diseqc_timings[0];
            assert_eq!(entry.adapter, 0);
            assert_eq!(
                (
                    entry.before_cmd,
                    entry.after_cmd,
                    entry.after_repeated_cmd,
                    entry.after_switch,
                    entry.after_burst,
                    entry.after_tone
                ),
                (15, 54, 15, 15, 15, 0)
            );
        }),
        (&["-S", "1-2:0"], |o| {
            assert_eq!(
                o.slaves,
                vec![
                    ms_config::SlaveConfig {
                        adapter: 1,
                        master: 0
                    },
                    ms_config::SlaveConfig {
                        adapter: 2,
                        master: 0
                    }
                ]
            );
        }),
        (&["-Y", "0:dvbt", "-Y", "1:dvbs2"], |o| {
            assert_eq!(o.delsys[0].system, DeliverySystem::Dvbt);
            assert_eq!(o.delsys[1].system, DeliverySystem::Dvbs2);
        }),
        (&["-o", "192.168.1.100:9001,1"], |o| {
            assert_eq!(o.dvbapi_host, "192.168.1.100");
            assert_eq!(o.dvbapi_port, 9001);
            assert_eq!(o.dvbapi_offset, 1);
        }),
        (&["-g"], |o| assert_eq!(o.slog, 1)),
        (&["-y", "5544"], |o| assert_eq!(o.rtsp_port, 5544)),
        (&["-s", "192.168.1.2"], |o| {
            assert_eq!(o.satip_servers.as_deref(), Some("192.168.1.2"))
        }),
        (&["-I", "living-room"], |o| {
            assert_eq!(o.name_app, "living-room")
        }),
        (&["-i", "10"], |o| assert_eq!(o.th_priority, 10)),
        (&["-O"], |o| assert_eq!(o.satip_rtsp_over_tcp, 1)),
        (&["-R", "/srv/www"], |o| {
            assert_eq!(o.document_root, "/srv/www")
        }),
        (&["-X", "//custom.xml"], |o| {
            assert_eq!(o.xml_path, "custom.xml")
        }),
        (&["-L", "0:9750-10600-11700"], |o| {
            let entry = o.lnb[0];
            assert_eq!(entry.adapter, 0);
            assert_eq!(
                (entry.low, entry.high, entry.switch),
                (9_750_000, 10_600_000, 11_700_000)
            );
        }),
        (&["-E"], |o| {
            assert_eq!(o.drop_encrypted, DropEncrypted::Send)
        }),
        (&["-P", "5000"], |o| assert_eq!(o.start_rtp, 5000)),
        (&["-Z", "1-2:30"], |o| {
            assert_eq!(o.adapter_timeouts.len(), 2);
            assert_eq!(o.adapter_timeouts[0].adapter, 1);
            assert_eq!(o.adapter_timeouts[1].adapter, 2);
            assert_eq!(o.adapter_timeouts[0].timeout_ms, 30_000);
        }),
        (&["-A", "0-1:0:0"], |o| {
            let entry = o.absolute_sources[0];
            assert_eq!((entry.src, entry.src_end, entry.adapter), (0, 1, 0));
            assert_eq!(entry.position, 1);
        }),
        (&["-6", "http://localhost/desc.xml"], |o| {
            assert_eq!(o.satip_xml.as_deref(), Some("http://localhost/desc.xml"))
        }),
        (&["-0", "0:1"], |o| {
            assert_eq!(o.diseqc_multis[0].adapter, 0);
            assert_eq!(o.diseqc_multis[0].position, 1);
        }),
        (&["-M", "0:1.5-1.6"], |o| {
            let entry = o.signal_multipliers[0];
            assert_eq!(entry.adapter, 0);
            assert!(approx(entry.strength, 1.5));
            assert!(approx(entry.snr, 1.6));
        }),
        (&["-D", "4"], |o| assert_eq!(o.device_id, 4)),
        (&["-1", "2"], |o| assert_eq!(o.use_demux_device, 2)),
        (&["-C", "0,2-3"], |o| assert_eq!(o.force_ci, vec![0, 2, 3])),
        (&["-3", "0:1234"], |o| {
            assert_eq!(o.ca_pins[0].adapter, 0);
            assert_eq!(o.ca_pins[0].pin, "1234");
        }),
        (&["-4"], |o| assert_eq!(o.use_ipv4_only, 0)),
        (&["-k"], |o| assert!(o.emulate_pids_all)),
        (&["-c", "0:*1-100"], |o| {
            let entry = &o.ca_channels[0];
            assert_eq!(entry.adapter, 0);
            assert_eq!(entry.multiple_pmt, 1);
            assert_eq!(entry.max_ca_pmt, 1);
            assert_eq!(entry.caids, vec![0x100]);
        }),
        (&["-z", "/tmp/cache"], |o| {
            assert_eq!(o.cache_dir, "/tmp/cache")
        }),
        (&["-5", "1-3,4"], |o| {
            assert_eq!(o.disable_cat, vec![1, 2, 3, 4])
        }),
        (&["-9"], |o| assert_eq!(o.pmt_scan, 0)),
    ];

    for (values, check) in cases {
        check(&parse(values));
    }
}

#[test]
fn long_only_options_table() {
    assert!(parse(&["--version"]).version);
    assert_eq!(parse(&["--send-all-ecm"]).send_all_ecm, 1);
    assert_eq!(
        parse(&["--satip-receive-buffer", "350"]).satipc_buffer,
        350 * 1024
    );
    assert_eq!(
        parse(&["--client-send-buffer", "10"]).output_buffer,
        10 * 1024
    );
    assert!(parse(&["--hw-descrambler"]).hw_descrambler);
    assert_eq!(
        parse(&["--sdt-charset", "latin1"]).sdt_charset,
        ms_config::DVB_CHARSET_LATIN1
    );
}

#[test]
fn every_long_option_is_recognized() {
    let cases: &[&[&str]] = &[
        &["--adapters", "1:2:3"],
        &["--adapter-timeout", "0:30"],
        &["--app-buffer", "10"],
        &["--buffer", "18800:18988"],
        &["--bind", "10.0.0.1"],
        &["--bind-http", "10.0.0.2"],
        &["--bind-dev", "eth0"],
        &["--cache-dir", "/tmp/cache"],
        &["--send-all-ecm"],
        &["--client-send-buffer", "10"],
        &["--delsys", "0:dvbs"],
        &["--debug", "http"],
        &["--device-id", "4"],
        &["--demux-dev", "2"],
        &["--disable-dvb"],
        &["--disable-ssdp"],
        &["--diseqc", "0:1-0"],
        &["--diseqc-timing", "0:15-54-15-15-15-0"],
        &["--diseqc-multi", "0:1"],
        &["--document-root", "html"],
        &["--drop-encrypted"],
        &["--enable-adapters", "0-1"],
        &["--foreground"],
        &["--name-app", "test"],
        &["--http-port", "9090"],
        &["--http-host", "1.2.3.4:8080"],
        &["--ipv6"],
        &["--jess", "0:1-1400"],
        &["--lnb", "0:9750-10600-11700"],
        &["--log", "http"],
        &["--logfile", "/tmp/x.log"],
        &["--multiplier", "0:1.0-1.0"],
        &["--no-all-pids"],
        &["--priority", "5"],
        &["--playlist", "http://example/list"],
        &["--remote-rtp", "1.2.3.4"],
        &["--rtsp-port", "5544"],
        &["--sdt-charset", "auto"],
        &["--syslog"],
        &["--slave", "1:0"],
        &["--threshold", "5:50"],
        &["--udp", "5000"],
        &["--unicable", "0:0-1284"],
        &["--xml", "desc.xml"],
        &["--help"],
        &["--virtual-diseqc", "0:0:0"],
        &["--version"],
        &["--disable-pmt-scan"],
        &["--disable-cat", "1"],
        &["--dvbapi", "127.0.0.1:9000"],
        &["--satip-servers", "192.168.1.2"],
        &["--satip-tcp"],
        &["--satip-xml", "http://localhost/desc.xml"],
        &["--satip-receive-buffer", "350"],
        &["--ca-pin", "0:1234"],
        &["--ci", "0"],
        &["--ca-channels", "0:1"],
        &["--hw-descrambler"],
    ];
    for values in cases {
        let outcome = parse_args(&args(values));
        assert!(outcome.error.is_none(), "{values:?}: {:?}", outcome.error);
        assert!(
            outcome.warnings.is_empty(),
            "{values:?}: {:?}",
            outcome.warnings
        );
    }
}

#[test]
fn combined_short_flags() {
    let opts = parse(&["-fGN9"]);
    assert_eq!(opts.daemon, 0);
    assert!(opts.disable_dvb);
    assert!(opts.disable_ssdp);
    assert_eq!(opts.pmt_scan, 0);

    let opts = parse(&["-fNlhttp,pmt"]);
    assert_eq!(opts.daemon, 0);
    assert!(opts.disable_dvb);
    assert_eq!(opts.log, 1 | (1 << 1) | (1 << 6));

    let opts = parse(&["-fNl", "http"]);
    assert_eq!(opts.log, 1 | (1 << 1));
}

#[test]
fn getopt_edge_cases() {
    // --long=value
    assert_eq!(parse(&["--http-port=9090"]).http_port, 9090);
    // attached short value
    assert_eq!(parse(&["-x9090"]).http_port, 9090);
    // long option abbreviation
    assert_eq!(parse(&["--http-po", "9090"]).http_port, 9090);
    // "--" terminates option parsing
    let opts = parse(&["-f", "--", "-N"]);
    assert_eq!(opts.daemon, 0);
    assert!(!opts.disable_dvb);
    // non-options are ignored (GNU permutation)
    let opts = parse(&["foo", "-f", "bar"]);
    assert_eq!(opts.daemon, 0);
    assert_eq!(opts.command_line, "foo -f bar");
    // a lone "-" is not an option
    assert_eq!(parse(&["-", "-f"]).daemon, 0);
    // single-dash long-only form
    assert!(parse(&["-version"]).version);
    assert_eq!(parse(&["-client-send-buffer", "4"]).output_buffer, 4096);
    // required argument may look like an option
    assert_eq!(parse(&["-i", "-2"]).th_priority, -2);
    // no -l/-v leaves logging disabled
    assert_eq!(parse(&[]).log, 0);
    // -l all enables every module
    assert_eq!(parse(&["-l", "all"]).log, ALL_LOG_MODULES);
    assert_eq!(parse(&["-v", "all"]).debug, ALL_LOG_MODULES);
    // unknown module still enables the general log bit
    assert_eq!(parse(&["-l", "not-a-module"]).log, 1);
    // -E cycles through the three C++ values
    let opts = parse(&["-E"]);
    assert_eq!(opts.drop_encrypted, DropEncrypted::Send);
    let opts = parse(&["-E", "-E"]);
    assert_eq!(opts.drop_encrypted, DropEncrypted::Stuff);
    let opts = parse(&["-E", "-E", "-E"]);
    assert_eq!(opts.drop_encrypted, DropEncrypted::Drop);
    // -4 toggles each time
    assert_eq!(parse(&["-4", "-4"]).use_ipv4_only, 1);
    // bind defaults to bind-http
    let opts = parse(&["-V", "10.0.0.1"]);
    assert_eq!(opts.bind_http.as_deref(), Some("10.0.0.1"));
}

#[test]
fn getopt_diagnostics_are_warnings_and_parsing_continues() {
    // Missing required arguments: glibc prints the diagnostic, the C++ switch
    // has no matching case, and the loop continues with the next option.
    let outcome = parse_outcome(&["-l"]);
    assert_eq!(
        outcome.warnings,
        ["option requires an argument -- 'l'"],
        "short option missing its argument"
    );
    assert!(outcome.error.is_none());
    assert_eq!(outcome.options.log, 0, "no -l was applied");

    let outcome = parse_outcome(&["--log"]);
    assert_eq!(
        outcome.warnings,
        ["option '--log' requires an argument"],
        "long option missing its argument"
    );
    assert!(outcome.error.is_none());

    // Unknown options: a leading unknown short character makes getopt_long_only
    // treat the whole single-dash word as an unrecognized long option.
    let outcome = parse_outcome(&["-Q", "-f"]);
    assert_eq!(outcome.warnings, ["unrecognized option '-Q'"]);
    assert_eq!(outcome.options.daemon, 0, "later -f still applied");

    let outcome = parse_outcome(&["--nope", "-f"]);
    assert_eq!(outcome.warnings, ["unrecognized option '--nope'"]);
    assert_eq!(outcome.options.daemon, 0, "later -f still applied");

    // Unknown characters inside a short cluster use the short diagnostic, and
    // the rest of the cluster is still parsed.
    let outcome = parse_outcome(&["-fQN"]);
    assert_eq!(outcome.warnings, ["invalid option -- 'Q'"]);
    assert_eq!(outcome.options.daemon, 0);
    assert!(outcome.options.disable_dvb);

    // An argument given to a no-argument long option.
    let outcome = parse_outcome(&["--foreground=1", "-N"]);
    assert_eq!(
        outcome.warnings,
        ["option '--foreground' doesn't allow an argument"]
    );
    assert_eq!(outcome.options.daemon, 1);
    assert!(outcome.options.disable_dvb, "later -N still applied");

    // Ambiguous abbreviations list every candidate in option-table order.
    let outcome = parse_outcome(&["--disable", "-f"]);
    assert_eq!(
        outcome.warnings,
        ["option '--disable' is ambiguous; possibilities: \
         '--disable-dvb' '--disable-ssdp' '--disable-pmt-scan' '--disable-cat'"]
    );
    assert!(outcome.error.is_none());
    assert_eq!(outcome.options.daemon, 0, "later -f still applied");

    // Single-dash long-only form uses the same diagnostics with a '-' prefix.
    let outcome = parse_outcome(&["--for=1"]);
    assert_eq!(
        outcome.warnings,
        ["option '--foreground' doesn't allow an argument"],
        "prefix is reported as the full option name"
    );

    let outcome = parse_outcome(&["-dis"]);
    assert_eq!(
        outcome.warnings,
        ["option '-dis' is ambiguous; possibilities: \
         '-disable-dvb' '-disable-ssdp' '-diseqc' '-diseqc-timing' \
         '-diseqc-multi' '-disable-pmt-scan' '-disable-cat'"]
    );

    // A required argument consumes the next argv element even when it looks
    // like an option, exactly as getopt_long_only does.
    let outcome = parse_outcome(&["-l", "-f"]);
    assert!(outcome.warnings.is_empty());
    assert_eq!(outcome.options.daemon, 1, "-f was the argument to -l");
    assert_eq!(outcome.options.log, 1);

    // -Z/--adapter-timeout takes a required argument, so "-f" is its argument.
    // No warning is printed and foreground is not applied.
    let outcome = parse_outcome(&["-Z", "-f"]);
    assert!(outcome.warnings.is_empty());
    assert_eq!(outcome.options.daemon, 1);

    // The same intent with an option that genuinely has no argument: the
    // warning is emitted and the following option still applies.
    let outcome = parse_outcome(&["-Q", "-f"]);
    assert_eq!(outcome.warnings, ["unrecognized option '-Q'"]);
    assert_eq!(outcome.options.daemon, 0);

    // Warnings collected before a fatal error are preserved.
    let outcome = parse_outcome(&["--nope", "--sdt-charset=bogus"]);
    assert_eq!(outcome.warnings, ["unrecognized option '--nope'"]);
    assert!(matches!(
        outcome.error,
        Some(OptError::InvalidValue {
            show_usage: true,
            ..
        })
    ));
}

#[test]
fn fatal_errors_match_cpp_exit_paths() {
    // Invalid --sdt-charset: C++ logs, calls usage(), then exit(1).
    let outcome = parse_outcome(&["--sdt-charset=bogus"]);
    assert!(matches!(
        outcome.error,
        Some(OptError::InvalidValue {
            show_usage: true,
            ..
        })
    ));

    // Invalid --bind-dev: C++ FAIL() exits 1 without usage.
    let outcome = parse_outcome(&["--bind-dev", "a/b"]);
    assert!(matches!(
        outcome.error,
        Some(OptError::InvalidValue {
            show_usage: false,
            ..
        })
    ));

    // resolve_bind_opts() failure: FAIL("Invalid bind configuration").
    let outcome = parse_outcome(&["--bind-dev", "eth0", "--bind", "10.0.0.1"]);
    assert!(matches!(
        outcome.error,
        Some(OptError::InvalidValue {
            show_usage: false,
            ..
        })
    ));
}

#[test]
fn repeated_and_wildcard_options_merge() {
    // Later explicit values replace earlier ones for the same adapter.
    let opts = parse(&["-d", "0:1-0", "-d", "0:3-0"]);
    assert_eq!(opts.diseqc.len(), 1);
    assert_eq!(opts.diseqc[0].committed_no, 3);

    // A wildcard updates existing per-adapter entries and the globals.
    let opts = parse(&["-d", "0:1-0", "-d", "*:@2-1"]);
    assert_eq!(opts.diseqc.len(), 1);
    assert_eq!(opts.diseqc[0].addr, 0);
    assert_eq!(
        (opts.diseqc[0].committed_no, opts.diseqc[0].uncommitted_no),
        (2, 1)
    );
    assert_eq!((opts.diseqc_addr, opts.diseqc_committed_no), (0, 2));

    let opts = parse(&["-q", "0:1-2-3-4-5-6", "-q", "*:7-8-9-10-11-12"]);
    assert_eq!(opts.diseqc_timings.len(), 1);
    assert_eq!(
        (
            opts.diseqc_timings[0].before_cmd,
            opts.diseqc_timings[0].after_repeated_cmd,
            opts.diseqc_timings[0].after_tone
        ),
        (7, 9, 12)
    );

    let opts = parse(&["-0", "0:1", "-0", "*:2"]);
    assert_eq!(opts.diseqc_multis.len(), 1);
    assert_eq!(opts.diseqc_multis[0].position, 2);
    assert_eq!(opts.diseqc_multi, 2);

    let opts = parse(&["-L", "0:9750-10600-11700", "-L", "*:10000-11000-12000"]);
    assert_eq!(opts.lnb.len(), 1);
    assert_eq!(
        (opts.lnb[0].low, opts.lnb[0].high, opts.lnb[0].switch),
        (10_000_000, 11_000_000, 12_000_000)
    );
    assert_eq!(opts.lnb[0].circular, 0);
    assert_eq!(opts.lnb_circular, 0);

    let opts = parse(&["-M", "0:%1.5-#1.6", "-M", "*:2.0-2.5"]);
    assert_eq!(opts.signal_multipliers.len(), 1);
    assert!(approx(opts.signal_multipliers[0].strength, 2.0));
    assert!(approx(opts.signal_multipliers[0].snr, 2.5));
    assert!(opts.signal_multipliers[0].force.strength_percent);
    assert!(opts.signal_multipliers[0].force.snr_decibel);

    let opts = parse(&["-Y", "0:dvbt", "-Y", "0:dvbs2"]);
    assert_eq!(opts.delsys.len(), 1);
    assert_eq!(opts.delsys[0].system, DeliverySystem::Dvbs2);
    assert_eq!(opts.delsys[0].secondary, DeliverySystem::Dvbs);

    let opts = parse(&["-Z", "0:30", "-Z", "0:40"]);
    assert_eq!(opts.adapter_timeouts.len(), 1);
    assert_eq!(opts.adapter_timeouts[0].timeout_ms, 40_000);

    let opts = parse(&["-3", "0:1234", "-3", "0:5678"]);
    assert_eq!(opts.ca_pins.len(), 1);
    assert_eq!(opts.ca_pins[0].pin, "5678");

    let opts = parse(&["-c", "0:1-100", "-c", "0:2-200"]);
    assert_eq!(opts.ca_channels.len(), 1);
    assert_eq!(opts.ca_channels[0].max_ca_pmt, 2);
    assert_eq!(opts.ca_channels[0].caids, vec![0x200]);
}

#[test]
fn slave_requires_enabled_or_allocated_master() {
    // -e 1 disables adapter 0 and nothing else has allocated it.
    let opts = parse(&["-e", "1", "-S", "1:0"]);
    assert!(opts.slaves.is_empty());

    // An earlier per-adapter option allocates the (disabled) master.
    let opts = parse(&["-L", "0:9750-10600-11700", "-e", "1", "-S", "1:0"]);
    assert_eq!(
        opts.slaves,
        vec![ms_config::SlaveConfig {
            adapter: 1,
            master: 0
        }]
    );

    // A master enabled by -e is allocated on demand.
    let opts = parse(&["-e", "0", "-S", "1:0"]);
    assert_eq!(opts.slaves.len(), 1);
}

#[test]
fn defaults_match_set_options() {
    let opts = Options::default();
    assert_eq!(opts.name_app, "minisatip");
    assert_eq!(opts.log, 0);
    assert_eq!(opts.debug, 0);
    assert_eq!(opts.start_rtp, 5500);
    assert_eq!(opts.http_port, 8080);
    assert_eq!(opts.rtsp_port, 554);
    assert_eq!(opts.timeout_sec, 30000);
    assert_eq!(opts.adapter_timeout, 30000);
    assert_eq!(opts.daemon, 1);
    assert_eq!(opts.dvr_buffer, ms_config::DVR_BUFFER);
    assert_eq!(opts.adapter_buffer, ADAPTER_BUFFER);
    assert_eq!(opts.udp_threshold, 25);
    assert_eq!(opts.tcp_threshold, 50);
    assert_eq!(opts.drop_encrypted, DropEncrypted::Drop);
    assert_eq!(opts.pmt_scan, 1);
    assert_eq!(opts.use_ipv4_only, 1);
    assert_eq!(opts.use_demux_device, 0);
    assert_eq!(opts.max_sbuf, 10);
    assert_eq!(opts.th_priority, -1);
    assert_eq!(opts.diseqc_addr, 0x10);
    assert_eq!(opts.diseqc_committed_no, 1);
    assert_eq!(opts.diseqc_before_cmd, 15);
    assert_eq!(opts.diseqc_after_cmd, 54);
    assert_eq!(opts.diseqc_after_repeated_cmd, 15);
    assert_eq!(opts.diseqc_after_switch, 15);
    assert_eq!(opts.diseqc_after_burst, 15);
    assert_eq!(opts.diseqc_after_tone, 0);
    assert_eq!(opts.diseqc_multi, -1);
    assert_eq!(opts.lnb_low, 9_750_000);
    assert_eq!(opts.lnb_high, 10_600_000);
    assert_eq!(opts.lnb_switch, 11_700_000);
    assert_eq!(opts.lnb_circular, 10_750_000);
    assert_eq!(opts.document_root, "html");
    assert_eq!(opts.xml_path, "desc.xml");
    assert_eq!(opts.cache_dir, "/var/cache/minisatip");
    assert_eq!(opts.log_file, "/tmp/minisatip.log");
    assert_eq!(opts.disc_host, "239.255.255.250");
    assert_eq!(opts.dvbapi_port, 0);
    assert_eq!(opts.dvbapi_offset, 0);
    assert_eq!(opts.sdt_charset, ms_config::DVB_CHARSET_AUTO);
    assert!(!opts.enigma);
    assert_eq!(opts.satip_servers, None);
    assert_eq!(opts.enabled_adapters, None);
}

#[cfg(feature = "netceiver")]
#[test]
fn netceiver_option_parses_interface_and_count() {
    let o = parse(&["-n", "vlan4:2"]);
    assert_eq!((o.netcv_if.as_deref(), o.netcv_count), (Some("vlan4"), 2));
    let o = parse(&["--netceiver", "3"]);
    assert_eq!((o.netcv_if.as_deref(), o.netcv_count), (Some("vlan4"), 3));
}

// Without NETCVCLIENT the C++ option table has no netceiver entry, so getopt
// reports -n/--netceiver as unknown and the server keeps running.
#[cfg(not(feature = "netceiver"))]
#[test]
fn netceiver_option_is_unknown_without_the_feature() {
    for values in [&["-n", "vlan4:2"][..], &["--netceiver", "vlan4:2"]] {
        let outcome = parse_outcome(values);
        assert!(outcome.error.is_none(), "{values:?}: {:?}", outcome.error);
        assert!(outcome.options.netcv_if.is_none());
        assert_eq!(outcome.options.netcv_count, 0);
    }
    // getopt_long_only tries -n as a long prefix first (glibc): it is ambiguous.
    assert_eq!(
        parse_outcome(&["-n", "1"]).warnings,
        ["option '-n' is ambiguous; possibilities: '-name-app' '-no-all-pids'"]
    );
    assert_eq!(
        parse_outcome(&["--netceiver", "1"]).warnings,
        ["unrecognized option '--netceiver'"]
    );
}

// minisatip.cpp:254-258,441-447: the build line and the -n paragraph follow NETCVCLIENT.
#[test]
fn usage_and_build_lines_follow_the_netceiver_feature() {
    let text = ms_config::usage_text();
    let lines = ms_config::built_info();
    if cfg!(feature = "netceiver") {
        assert!(text.contains("* -n --netceiver if:count:"));
        assert!(text.contains(
            "Built with netceiver
"
        ));
        assert!(lines.contains(&"Built with netceiver"));
    } else {
        assert!(
            !text.contains("-n --netceiver"),
            "the -n paragraph is left out"
        );
        assert!(text.contains(
            "Built without netceiver
"
        ));
        assert!(lines.contains(&"Built without netceiver"));
    }
}
