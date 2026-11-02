//! `--cam-recovery`: present in every build, off by default.

use ms_config::parse_args;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[test]
fn cam_recovery_defaults_to_off() {
    assert_eq!(parse_args(&[]).options.cam_recovery, 0);
}

#[test]
fn cam_recovery_is_parsed_in_seconds_in_every_build() {
    let outcome = parse_args(&args(&["--cam-recovery", "45"]));
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert_eq!(outcome.options.cam_recovery, 45);
    assert_eq!(
        parse_args(&args(&["--cam-recovery=0"]))
            .options
            .cam_recovery,
        0
    );
    // Unparsable or negative values fall back to off.
    assert_eq!(
        parse_args(&args(&["--cam-recovery", "nope"]))
            .options
            .cam_recovery,
        0
    );
    assert_eq!(
        parse_args(&args(&["--cam-recovery", "-1"]))
            .options
            .cam_recovery,
        0
    );
}

#[test]
fn cam_recovery_does_not_touch_ci_plus_recovery() {
    let options = parse_args(&args(&["--cam-recovery", "45"])).options;
    assert_eq!(options.ci_plus_recovery, 0);
}

#[test]
fn usage_lists_cam_recovery() {
    let usage = ms_config::usage_text();
    assert!(usage.contains("--cam-recovery seconds"), "{usage}");
}

#[test]
fn cam_recovery_no_zap_guard_is_a_long_only_flag() {
    assert!(
        !parse_args(&[]).options.cam_recovery_no_zap_guard,
        "on by default"
    );
    let outcome = parse_args(&args(&[
        "--cam-recovery",
        "45",
        "--cam-recovery-no-zap-guard",
    ]));
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(outcome.options.cam_recovery_no_zap_guard);
    assert_eq!(outcome.options.cam_recovery, 45);
    // A flag takes no value, and it works without --cam-recovery too.
    let alone = parse_args(&args(&["--cam-recovery-no-zap-guard"]));
    assert!(alone.error.is_none(), "{:?}", alone.error);
    assert!(alone.options.cam_recovery_no_zap_guard);
    assert_eq!(alone.options.cam_recovery, 0);
}

#[test]
fn usage_lists_the_zap_guard_flag() {
    let usage = ms_config::usage_text();
    assert!(usage.contains("--cam-recovery-no-zap-guard"), "{usage}");
}
