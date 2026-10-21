//! Port of the upstream `tests/test_opts.cpp` cases.

use ms_config::{
    DVB_CHARSET_AUTO, DVB_CHARSET_LATIN1, Options, parse_dvbapi_opt, parse_sdt_charset_opt,
};

fn parse_dvbapi(optarg: &str) -> Options {
    let mut opts = Options::default();
    parse_dvbapi_opt(optarg, &mut opts);
    opts
}

#[test]
fn test_parse_dvbapi_opt() {
    // Socket
    let opts = parse_dvbapi("/tmp/camd.socket");
    assert_eq!(opts.pids_all_no_dec, 0, "opts.pids_all_no_dec != 0");
    assert_eq!(opts.dvbapi_offset, 0, "opts.dvbapi_offset != 0");
    assert_eq!(opts.dvbapi_port, 9000, "opts.dvbapi_port != 9000");
    assert_eq!(
        opts.dvbapi_host, "/tmp/camd.socket",
        "opts.dvbapi_host != \"/tmp/camd.socket\""
    );

    // Socket with offset or port doesn't make sense, so skip checking those

    // Host
    let opts = parse_dvbapi("192.168.1.100");
    assert_eq!(opts.pids_all_no_dec, 0, "opts.pids_all_no_dec != 0");
    assert_eq!(opts.dvbapi_offset, 0, "opts.dvbapi_offset != 0");
    assert_eq!(opts.dvbapi_port, 9000, "opts.dvbapi_port != 9000");
    assert_eq!(
        opts.dvbapi_host, "192.168.1.100",
        "opts.dvbapi_host != \"192.168.1.100\""
    );

    // Host with pids_all_no_dec
    let opts = parse_dvbapi("~192.168.1.100");
    assert_eq!(opts.pids_all_no_dec, 1, "opts.pids_all_no_dec != 1");
    assert_eq!(opts.dvbapi_offset, 0, "opts.dvbapi_offset != 0");
    assert_eq!(opts.dvbapi_port, 9000, "opts.dvbapi_port != 9000");
    assert_eq!(
        opts.dvbapi_host, "192.168.1.100",
        "opts.dvbapi_host != \"192.168.1.100\""
    );

    // Host with port
    let opts = parse_dvbapi("192.168.1.100:9001");
    assert_eq!(opts.pids_all_no_dec, 0, "opts.pids_all_no_dec != 0");
    assert_eq!(opts.dvbapi_offset, 0, "opts.dvbapi_offset != 0");
    assert_eq!(opts.dvbapi_port, 9001, "opts.dvbapi_port != 9001");
    assert_eq!(
        opts.dvbapi_host, "192.168.1.100",
        "opts.dvbapi_host != \"192.168.1.100\""
    );

    // Host with port and offset
    let opts = parse_dvbapi("192.168.1.100:9001,1");
    assert_eq!(opts.pids_all_no_dec, 0, "opts.pids_all_no_dec != 0");
    assert_eq!(opts.dvbapi_offset, 1, "opts.dvbapi_offset != 1");
    assert_eq!(opts.dvbapi_port, 9001, "opts.dvbapi_port != 9001");
    assert_eq!(
        opts.dvbapi_host, "192.168.1.100",
        "opts.dvbapi_host != \"192.168.1.100\""
    );

    // Host with port and offset and pids_all_no_dec
    let opts = parse_dvbapi("~192.168.1.100:9001,1");
    assert_eq!(opts.pids_all_no_dec, 1, "opts.pids_all_no_dec != 1");
    assert_eq!(opts.dvbapi_offset, 1, "opts.dvbapi_offset != 1");
    assert_eq!(opts.dvbapi_port, 9001, "opts.dvbapi_port != 9001");
    assert_eq!(
        opts.dvbapi_host, "192.168.1.100",
        "opts.dvbapi_host != \"192.168.1.100\""
    );
}

#[test]
fn test_parse_sdt_charset_opt() {
    assert_eq!(
        parse_sdt_charset_opt("auto"),
        DVB_CHARSET_AUTO,
        "auto should select AUTO"
    );
    assert_eq!(
        parse_sdt_charset_opt("latin1"),
        DVB_CHARSET_LATIN1,
        "latin1 should select LATIN1"
    );
    assert_eq!(
        parse_sdt_charset_opt("iso-8859-1"),
        DVB_CHARSET_LATIN1,
        "iso-8859-1 should select LATIN1"
    );
    assert_eq!(
        parse_sdt_charset_opt("iso8859-1"),
        DVB_CHARSET_LATIN1,
        "iso8859-1 should select LATIN1"
    );
    assert_eq!(parse_sdt_charset_opt("utf8"), -1, "utf8 should be invalid");
    assert_eq!(parse_sdt_charset_opt(""), -1, "empty should be invalid");
}
