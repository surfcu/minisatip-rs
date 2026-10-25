//! `--ci-plus-recovery`: present only in builds that enable the `ci-plus` feature.

use ms_config::parse_args;

fn args(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_string()).collect()
}

#[cfg(feature = "ci-plus")]
#[test]
fn ci_plus_recovery_defaults_to_off() {
    assert_eq!(parse_args(&[]).options.ci_plus_recovery, 0);
}

#[cfg(feature = "ci-plus")]
#[test]
fn ci_plus_recovery_is_parsed_in_seconds() {
    let options = parse_args(&args(&["--ci-plus-recovery", "45"])).options;
    assert_eq!(options.ci_plus_recovery, 45);
    assert_eq!(
        parse_args(&args(&["--ci-plus-recovery=0"]))
            .options
            .ci_plus_recovery,
        0
    );
    // Unparsable or negative values fall back to off.
    assert_eq!(
        parse_args(&args(&["--ci-plus-recovery", "nope"]))
            .options
            .ci_plus_recovery,
        0
    );
    assert_eq!(
        parse_args(&args(&["--ci-plus-recovery", "-1"]))
            .options
            .ci_plus_recovery,
        0
    );
}

#[cfg(feature = "ci-plus")]
#[test]
fn usage_lists_ci_plus_recovery() {
    let usage = ms_config::usage_text();
    assert!(usage.contains("--ci-plus-recovery seconds"), "{usage}");
}

#[cfg(not(feature = "ci-plus"))]
#[test]
fn ci_plus_recovery_is_unknown_without_the_feature() {
    assert_eq!(parse_args(&[]).options.ci_plus_recovery, 0);
    let outcome = parse_args(&args(&["--ci-plus-recovery", "45"]));
    assert!(outcome.error.is_none(), "{:?}", outcome.error);
    assert!(!outcome.warnings.is_empty(), "the option was accepted");
    assert_eq!(outcome.options.ci_plus_recovery, 0);
    let usage = ms_config::usage_text();
    assert!(!usage.contains("--ci-plus-recovery"), "{usage}");
}
