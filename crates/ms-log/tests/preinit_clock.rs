//! The clock for lines logged before `init`; its own binary, as the clock
//! is process-wide.

use ms_log::Timestamp;

fn fixed() -> Timestamp {
    Timestamp::new(23, 10, 18, 27, 52, 799)
}

#[test]
fn lines_before_init_use_the_installed_clock() {
    ms_log::set_preinit_clock(fixed);
    assert_eq!(ms_log::preinit_timestamp(), fixed());
}
