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
    ] {
        let outcome = parse_args(&[name.to_owned(), "1".to_owned()]);
        assert!(outcome.error.is_none(), "{name}: {:?}", outcome.error);
        assert!(!outcome.warnings.is_empty(), "{name} was accepted");
    }
    let usage = ms_config::usage_text();
    assert!(!usage.contains("--log-remote"), "{usage}");
    assert!(!usage.contains("--log-ring"), "{usage}");
    assert!(!usage.contains("--ddci-capture"), "{usage}");
}
