//! Ports of upstream `tests/test_dvb_charset.cpp`.

use ms_psi::{CharsetOverride, dvb_get_string};

#[test]
fn test_charset_latin1_override() {
    // "RTP Noticias" with raw Latin-1 i (0xED), no DVB prefix
    let latin1 = b"RTP Not\xEDcias";
    let out =
        dvb_get_string(latin1, 64, CharsetOverride::Latin1).expect("latin1 name should decode");
    assert_eq!(
        out,
        b"RTP Not\xC3\xADcias".to_vec(),
        "latin1 0xED should become U+00ED"
    );
    // "Localvisao" with raw Latin-1 a-tilde (0xE3)
    let local = b"Localvis\xE3o";
    let out =
        dvb_get_string(local, 64, CharsetOverride::Latin1).expect("latin1 name should decode");
    assert_eq!(
        out,
        b"Localvis\xC3\xA3o".to_vec(),
        "latin1 0xE3 should become U+00E3"
    );
}

#[test]
fn test_charset_auto_keeps_prefix_detection() {
    // explicit UTF-8 still wins in auto mode
    let utf8 = b"\x15Not\xC3\xADcias";
    let out = dvb_get_string(utf8, 64, CharsetOverride::Auto).expect("utf8 name should decode");
    assert_eq!(
        out,
        b"Not\xC3\xADcias".to_vec(),
        "utf8 prefix should pass through"
    );
    // explicit Latin-1 via the Table A.4 escape (0x10 0x00 0x01)
    let escaped = b"\x10\x00\x01Not\xEDcias";
    let out =
        dvb_get_string(escaped, 64, CharsetOverride::Auto).expect("escaped latin1 should decode");
    assert_eq!(
        out,
        b"Not\xC3\xADcias".to_vec(),
        "table escape should select 8859-1"
    );
    // documents the #1477 default: prefix-less raw Latin-1 is read as
    // ISO-6937, whose E0-EF rows differ (0xED maps to U+0166)
    let raw = b"Not\xEDcias";
    let out = dvb_get_string(raw, 64, CharsetOverride::Auto).expect("raw name should decode");
    assert_eq!(
        out,
        b"Not\xC5\xA6cias".to_vec(),
        "auto 6937 maps raw 0xED to U+0166 (#1477)"
    );
}

#[test]
fn test_charset_override_ignores_prefix() {
    // forced latin1 also decodes prefixed strings as Latin-1 (the 0x15
    // control byte is skipped, UTF-8 bytes become mojibake)
    let utf8 = b"\x15Not\xC3\xADcias";
    let out =
        dvb_get_string(utf8, 64, CharsetOverride::Latin1).expect("forced latin1 should decode");
    assert_eq!(
        out,
        b"Not\xC3\x83\xC2\xADcias".to_vec(),
        "forced latin1 reads UTF-8 bytes as Latin-1"
    );
}

#[test]
fn test_charset_ucs2_odd_length() {
    let ucs2 = [0x11, 0x00, 0x41, 0x00];
    let out =
        dvb_get_string(&ucs2, 64, CharsetOverride::Auto).expect("odd UCS-2 name should decode");
    assert_eq!(out, b"A".to_vec(), "odd UCS-2 name should keep its pairs");
}
