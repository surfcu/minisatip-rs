//! `diag-log` options: present only in builds that enable the feature.

use ms_config::parse_args;

#[cfg(feature = "diag-log")]
#[test]
fn diag_options_have_defaults() {
    let options = parse_args(&[]).options;
    assert_eq!(options.log_remote, None);
    assert_eq!(options.log_remote_queue, 10_000);
    assert_eq!(options.log_ring, 20_000);
}

#[cfg(feature = "diag-log")]
#[test]
fn diag_options_are_parsed() {
    let options = parse_args(&[
        "--log-remote".to_owned(),
        "tcp://127.0.0.1:5514".to_owned(),
        "--log-remote-queue".to_owned(),
        "7".to_owned(),
        "--log-ring".to_owned(),
        "123".to_owned(),
    ])
    .options;
    assert_eq!(options.log_remote.as_deref(), Some("tcp://127.0.0.1:5514"));
    assert_eq!(options.log_remote_queue, 7);
    assert_eq!(options.log_ring, 123);
}

#[cfg(feature = "diag-log")]
#[test]
fn diag_option_values_are_clamped() {
    // A zero-length queue would reject every line.
    assert_eq!(
        parse_args(&["--log-remote-queue".to_owned(), "0".to_owned()])
            .options
            .log_remote_queue,
        1
    );
    // Zero disables the ring; unparsable values fall back to off.
    assert_eq!(
        parse_args(&["--log-ring".to_owned(), "0".to_owned()])
            .options
            .log_ring,
        0
    );
    assert_eq!(
        parse_args(&["--log-ring".to_owned(), "nope".to_owned()])
            .options
            .log_ring,
        0
    );
}

#[cfg(feature = "diag-log")]
#[test]
fn ddci_capture_options_are_parsed_with_defaults() {
    let options = parse_args(&[]).options;
    assert_eq!(options.ddci_capture, None);
    assert_eq!(options.ddci_capture_max_mb, 2048);
    let options = parse_args(&[
        "--ddci-capture".to_owned(),
        "/tmp/cap".to_owned(),
        "--ddci-capture-max-mb".to_owned(),
        "512".to_owned(),
    ])
    .options;
    assert_eq!(options.ddci_capture.as_deref(), Some("/tmp/cap"));
    assert_eq!(options.ddci_capture_max_mb, 512);
}

#[cfg(feature = "diag-log")]
#[test]
fn usage_lists_the_diag_options() {
    let usage = ms_config::usage_text();
    assert!(usage.contains("--log-remote tcp://host:port"), "{usage}");
    assert!(usage.contains("--log-remote-queue lines"), "{usage}");
    assert!(usage.contains("--log-ring lines"), "{usage}");
    assert!(usage.contains("--ddci-capture dir"), "{usage}");
    assert!(usage.contains("--ddci-capture-max-mb mb"), "{usage}");
    assert!(usage.contains("--cam-menu: serve POST"), "{usage}");
    assert!(
        usage.contains("--cam-menu-unsolicited answer1|answer0|open"),
        "{usage}"
    );
    assert!(usage.contains("--cam-ecm-trace: every 10 s"), "{usage}");
    assert!(usage.contains("--cam-menu-snapshot HASH:CHOICE"), "{usage}");
}

#[cfg(feature = "diag-log")]
#[test]
fn cam_menu_snapshot_takes_hash_pinned_paths() {
    assert!(parse_args(&[]).options.cam_menu_snapshot.is_empty());
    let good = "00000000000000aa:1/00000000000000bb:2,00000000000000aa:4";
    let options = parse_args(&["--cam-menu-snapshot".to_owned(), good.to_owned()]).options;
    assert_eq!(
        options.cam_menu_snapshot,
        [vec![(0xaa, 1), (0xbb, 2)], vec![(0xaa, 4)]]
    );
    for bad in [
        "",
        "aa:1",
        "00000000000000aa:0",
        "00000000000000aa:10",
        "00000000000000ag:1",
        "00000000000000aa:1/00000000000000aa:1/00000000000000aa:1/00000000000000aa:1",
        "00000000000000aa:1,,00000000000000aa:2",
    ] {
        let outcome = parse_args(&["--cam-menu-snapshot".to_owned(), bad.to_owned()]);
        assert!(outcome.error.is_some(), "{bad:?} must be refused");
    }
    let nine = ["00000000000000aa:1"; 9].join(",");
    assert!(
        parse_args(&["--cam-menu-snapshot".to_owned(), nine])
            .error
            .is_some(),
        "at most 8 paths"
    );
}

#[cfg(feature = "diag-log")]
#[test]
fn cam_ecm_trace_is_a_flag() {
    assert!(!parse_args(&[]).options.cam_ecm_trace);
    assert!(
        parse_args(&["--cam-ecm-trace".to_owned()])
            .options
            .cam_ecm_trace
    );
}

#[cfg(feature = "diag-log")]
#[test]
fn cam_menu_options_are_parsed_with_defaults() {
    let options = parse_args(&[]).options;
    assert!(!options.cam_menu);
    assert_eq!(options.cam_menu_unsolicited, "answer1");
    for mode in ["answer1", "answer0", "open"] {
        let outcome = parse_args(&[
            "--cam-menu".to_owned(),
            "--cam-menu-unsolicited".to_owned(),
            mode.to_owned(),
        ]);
        assert!(outcome.error.is_none(), "{mode}: {:?}", outcome.error);
        assert!(outcome.options.cam_menu);
        assert_eq!(outcome.options.cam_menu_unsolicited, mode);
    }
    let outcome = parse_args(&["--cam-menu-unsolicited".to_owned(), "2".to_owned()]);
    assert!(outcome.error.is_some(), "a bad mode is fatal");
}

#[cfg(not(feature = "diag-log"))]
#[test]
fn diag_options_are_unknown_without_the_feature() {
    let outcome = parse_args(&[]);
    assert_eq!(outcome.options.log_remote, None);
    assert_eq!(outcome.options.ddci_capture, None);
    for name in [
        "--log-remote",
        "--log-remote-queue",
        "--log-ring",
        "--ddci-capture",
        "--ddci-capture-max-mb",
        "--cam-menu-unsolicited",
        "--cam-menu-snapshot",
    ] {
        let outcome = parse_args(&[name.to_owned(), "1".to_owned()]);
        assert!(outcome.error.is_none(), "{name}: {:?}", outcome.error);
        assert!(!outcome.warnings.is_empty(), "{name} was accepted");
    }
    let usage = ms_config::usage_text();
    assert!(!usage.contains("--log-remote"), "{usage}");
    assert!(!usage.contains("--log-ring"), "{usage}");
    assert!(!usage.contains("--ddci-capture"), "{usage}");
    assert!(!usage.contains("--cam-menu"), "{usage}");
    let outcome = parse_args(&["--cam-ecm-trace".to_owned()]);
    assert!(!outcome.options.cam_ecm_trace);
    assert!(!outcome.warnings.is_empty(), "--cam-ecm-trace was accepted");
    let outcome = parse_args(&["--cam-menu".to_owned()]);
    assert!(!outcome.options.cam_menu);
    assert!(!outcome.warnings.is_empty(), "--cam-menu was accepted");
}
