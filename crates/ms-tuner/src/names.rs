//! `EnumMap::reverse_lookup` name tables from dvb.cpp:100-201.
//!
//! Every function returns `""` for a missing value or for an entry whose key is
//! `" "`, exactly like `EnumMap::reverse_lookup` (utils.h:55-63).

fn lookup(value: i32, entries: &[(i32, &'static str)]) -> &'static str {
    for (entry_value, name) in entries {
        if *entry_value == value {
            return if *name == " " { "" } else { name };
        }
    }
    ""
}

/// `fe_delsys_map` (dvb.cpp:100-120).
pub fn fe_delsys(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "undefined"),
            (1, "dvbc"),
            (2, "dvbcb"),
            (3, "dvbt"),
            (4, "dss"),
            (5, "dvbs"),
            (6, "dvbs2"),
            (7, "dvbh"),
            (8, "isdbt"),
            (9, "isdbs"),
            (10, "isdbc"),
            (11, "atsc"),
            (12, "atscmh"),
            (13, "dmbth"),
            (14, "cmmb"),
            (15, "dab"),
            (16, "dvbt2"),
            (17, "turbo"),
            (18, "dvbcc"),
            (19, "dvbc2"),
        ],
    )
}

/// `fe_pol_map` (dvb.cpp:122-123).
pub fn fe_pol(value: i32) -> &'static str {
    lookup(
        value,
        &[(0, "none"), (1, "v"), (2, "h"), (3, "r"), (4, "l")],
    )
}

/// `fe_rolloff_map` (dvb.cpp:125-131).
pub fn fe_rolloff(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "0.35"),
            (1, "0.20"),
            (2, "0.25"),
            (3, " "),
            (4, "0.15"),
            (5, "0.10"),
            (6, "0.05"),
        ],
    )
}

/// `fe_modulation_map` (dvb.cpp:133-150).
pub fn fe_modulation(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "qpsk"),
            (1, "16qam"),
            (2, "32qam"),
            (3, "64qam"),
            (4, "128qam"),
            (5, "256qam"),
            (6, " "),
            (7, "8vsb"),
            (8, "16vsb"),
            (9, "8psk"),
            (10, "16apsk"),
            (11, "32apsk"),
            (12, "dqpsk"),
            (13, "qam_4_nr"),
            (14, "64apsk"),
            (15, "128apsk"),
            (16, "256apsk"),
        ],
    )
}

/// `fe_fec_map` (dvb.cpp:152-166).
pub fn fe_fec(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "none"),
            (1, "12"),
            (2, "23"),
            (3, "34"),
            (4, "45"),
            (5, "56"),
            (6, "67"),
            (7, "78"),
            (8, "89"),
            (9, " "),
            (10, "35"),
            (11, "910"),
            (12, "25"),
            (13, "14"),
            (14, "13"),
        ],
    )
}

/// `fe_pilot_map` (dvb.cpp:168-169).
pub fn fe_pilot(value: i32) -> &'static str {
    lookup(value, &[(0, "on"), (1, "off"), (2, " ")])
}

/// `fe_gi_map` (dvb.cpp:171-182).
pub fn fe_guard_interval(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "132"),
            (1, "116"),
            (2, "18"),
            (3, "14"),
            (4, " "),
            (5, "1128"),
            (6, "19128"),
            (7, "19256"),
            (8, "pn420"),
            (9, "pn595"),
            (10, "pn945"),
        ],
    )
}

/// `fe_tmode_map` (dvb.cpp:184-193).
pub fn fe_transmission_mode(value: i32) -> &'static str {
    lookup(
        value,
        &[
            (0, "2k"),
            (1, "8k"),
            (2, " "),
            (3, "4k"),
            (4, "1k"),
            (5, "16k"),
            (6, "32k"),
            (7, "c1"),
            (8, "c3780"),
        ],
    )
}

/// `fe_inversion_map` (dvb.cpp:200-201).
pub fn fe_inversion(value: i32) -> &'static str {
    lookup(value, &[(0, "0"), (1, "1"), (2, " ")])
}

/// `fe_pls_mode_map` (dvb.cpp:195-198).
///
/// No dvb.cpp log site looks up the PLS mode in this port, but the table is
/// kept next to the others for completeness.
#[allow(dead_code)]
pub fn fe_pls_mode(value: i32) -> &'static str {
    lookup(value, &[(0, "root"), (1, "gold"), (2, "combo")])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_and_unknown_reverse_lookups_are_empty() {
        assert_eq!(fe_modulation(6), "");
        assert_eq!(fe_fec(9), "");
        assert_eq!(fe_inversion(2), "");
        assert_eq!(fe_delsys(42), "");
        assert_eq!(fe_pol(5), "");
    }

    #[test]
    fn known_values_use_the_cpp_keys() {
        assert_eq!(fe_delsys(6), "dvbs2");
        assert_eq!(fe_modulation(9), "8psk");
        assert_eq!(fe_fec(10), "35");
        assert_eq!(fe_guard_interval(7), "19256");
        assert_eq!(fe_transmission_mode(8), "c3780");
        assert_eq!(fe_pol(4), "l");
        assert_eq!(fe_rolloff(0), "0.35");
        assert_eq!(fe_pilot(2), "");
    }
}
