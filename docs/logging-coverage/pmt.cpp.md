# pmt.cpp logging coverage

Ported 68 / not applicable 29 (97 active log sites; 4 commented-out calls are not sites).

`DEFAULT_LOG` is `LOG_PMT`. Sites rendered by modules that also host other C++
sources pass `ms_log::LOG_PMT` explicitly.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 234 | LOG | filter %d A: %d: pid %d, master %d, next %d, flags %d, | n/a: `dump_filters` and the filter link chain were redesigned away (filters are an id-indexed table) |
| 263 | LOG_AND_RETURN | %s failed, pid %d | crates/ms-sched/src/pmt.rs:1534 |
| 268 | LOG_AND_RETURN | %s failed | crates/ms-sched/src/pmt.rs:1537 |
| 296 | LOG | new filter %d added for adapter %d, pid %d, flags %d, ma | crates/ms-sched/src/pmt.rs:1566 |
| 319 | LOG | deleting filter %d | crates/ms-sched/src/pmt.rs:1580 |
| 341 | LOGM | warning: filter %d was also found for pid %d | n/a: no master/next filter linked list in Rust |
| 368 | LOG | deleted filter %d, ad %d, pid %d, max filters %d | crates/ms-sched/src/pmt.rs:1609 |
| 375 | LOGM | found filter %d for pid %d, master %d (max filters %d) | n/a: no master/next filter linked list in Rust |
| 390 | LOGM | Found %d filters for adapter %d pid %d master %d with f | n/a: no master/next filter linked list in Rust |
| 412 | LOGM | pid not found or pid in use by %lu streams | crates/ms-sched/src/pmt.rs:1669 |
| 422 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-sched/src/pmt.rs:1636 |
| 425 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-sched/src/pmt.rs:1646 |
| 432 | LOG_AND_RETURN | %s: filter %d not found | n/a: `set_filter_opaque` has no Rust counterpart (opaque is attached through the sink) |
| 435 | LOG_AND_RETURN | %s: filter %d not found | n/a: `set_filter_opaque` has no Rust counterpart |
| 443 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-sched/src/pmt.rs:1678 |
| 446 | LOGM | %s: filter %d not found | crates/ms-sched/src/pmt.rs:1687 |
| 471 | DEBUGM | filter %d, pid %d, index %d did not match: %02X & %02X  | crates/minisatip/src/ca.rs:950 |
| 481 | DEBUGM | filter %dmatch: id %d: pid %d, flags %d, mask_len %d, f | crates/minisatip/src/ca.rs:967 |
| 495 | LOGM | %s: filter %d not enabled | n/a: `filters_for_pid` only yields enabled filters |
| 502 | DEBUGM | matching pid %d, filter %d, match %d, flags %d, isEMM % | crates/minisatip/src/ca.rs:858 |
| 509 | DEBUGM | assemble_packet returned %d for pid %d | crates/minisatip/src/ca.rs:875 |
| 524 | DEBUGM | EMM: match %d id: %d len: %d: %02X %02X %02X %02X %02X | crates/minisatip/src/ca.rs:901 |
| 550 | LOG | filter %d with pid %d is wrong for pid %d | n/a: no master/next filter linked list in Rust |
| 588 | LOG | disabling CW %d, parity %d created %jd ms ago | n/a: `Descrambler::remove_service` has no clock argument for the "ms ago" value |
| 639 | LOGM | %s: found pid %d with parity change without start indica | crates/ms-ts/src/section.rs:247 |
| 669 | LOGM | Testing len %d, CW: %s | n/a: `test_decrypt_packets` has the cipher key but no CW slot/time for `cw_to_string` |
| 672 | hexdump | test_decrypt_packet before: | crates/ms-ca-drivers/src/descramble/pipeline.rs:132 |
| 685 | hexdump | test_decrypt_packet after: | crates/ms-ca-drivers/src/descramble/pipeline.rs:136 |
| 711 | LOG | List of CWs: | crates/ms-ca-drivers/src/descramble/pipeline.rs:384 |
| 714 | LOG | * CW %d: %s | crates/ms-ca-drivers/src/descramble/pipeline.rs:390 |
| 750 | LOGM | correct CW found (len %d): %s | crates/ms-ca-drivers/src/descramble/pipeline.rs:570 |
| 763 | LOGM | candidate CW %s | crates/ms-ca-drivers/src/descramble/pipeline.rs:583 |
| 772 | LOGM | New candidate found: %s | crates/ms-ca-drivers/src/descramble/pipeline.rs:587 |
| 777 | LOGL | found CW: %d %s for %s PMT %d, old cw %d, packets %d | n/a: the descrambler service has no PMT name/pid/blen fields |
| 803 | LOG | Expiring CW %d for pmt %d: %s | crates/ms-ca-drivers/src/descramble/pipeline.rs:619 |
| 831 | LOG_AND_RETURN | %s: pmt not found %d | crates/ms-ca-drivers/src/descramble/pipeline.rs:413 |
| 833 | LOG_AND_RETURN | op not found for algo %d | crates/ms-ca-drivers/src/descramble/pipeline.rs:420 |
| 840 | LOG_AND_RETURN | %s: pmt %d gone | n/a: `send_cw` fetches the service once and has no second gone check |
| 845 | LOG_AND_RETURN | cw already exist at position %d: %s  | crates/ms-ca-drivers/src/descramble/pipeline.rs:444 |
| 853 | LOG | CWS is full %d | crates/ms-ca-drivers/src/descramble/pipeline.rs:459 |
| 888 | LOG | CW %d for PMT %d (%s) Warning! New CW using the current  | n/a: `send_cw` has no PMT name or active-CW state for this warning |
| 909 | LOG | CW %d for PMT %d (%s), pid %d, for %s parity, %s | n/a: `send_cw` has no PMT name/pid fields |
| 936 | LOGM | pmt: decrypted pmt %d, packets %d, CW %d, old CW %d, par | n/a: the descrambler service has no channel/sid field |
| 973 | DEBUGM | PMT not found for pid %d, id %d, packet %d, pos %d | crates/ms-ca-drivers/src/descramble/pipeline.rs:673 |
| 987 | LOG | Parity change for %s PMT %d, new active parity %d pid %d | n/a: the descrambler service has no PMT name |
| 1001 | LOG0 | Failed to allocate batch with size %d in the stack | n/a: the Rust batch is a `Vec`, allocation cannot fail |
| 1172 | LOG | PMT %d takes over from lower-priority PMT %d | crates/ms-sched/src/pmt.rs:799 |
| 1234 | LOG | Stopping started PMT %d: %s | crates/ms-sched/src/pmt.rs:871 |
| 1260 | LOGM | PMT %d steals pid %d from PMT %d | crates/ms-sched/src/pmt.rs:906 |
| 1265 | LOGM | PMT %d claimed pid %d | crates/ms-sched/src/pmt.rs:912 |
| 1343 | LOG | Marking PID %d packet %d pos %d as NULL | crates/ms-ca-drivers/src/descramble/pipeline.rs:724 |
| 1407 | LOG | %s: adding pid %d sid %d to emulate all pids | crates/ms-sched/src/pmt.rs:1126 |
| 1416 | LOG | %s: adding (enforced) pid %d sid %d to emulate all pid | crates/ms-sched/src/pmt.rs:1136 |
| 1422 | LOG | %s: adding (enforced) pid 8191 (NULL) too | crates/ms-sched/src/pmt.rs:1145 |
| 1476 | LOG | **** PMT %d, global key %PRIx64 packets per 10s interval | n/a: no per-PMT packet statistics dump in Rust |
| 1497 | DEBUGM | Non-ts packet %d, pos %d | n/a: no non-TS packet scan/statistics pass in Rust |
| 1535 | LOG_AND_RETURN | PMT buffer is full, could not add new pmts | crates/ms-sched/src/pmt.rs:331 |
| 1570 | LOG | returning new pmt %d for adapter %d, pmt pid %d, sid %d % | crates/ms-sched/src/pmt.rs:354 |
| 1590 | LOG | deleting PMT %d, name %s  | crates/ms-sched/src/pmt.rs:370 |
| 1730 | DEBUGM | returning EMM len %d (%X) from %d, found %d emms | crates/minisatip/src/ca.rs:825 |
| 1743 | LOG | %s: data too large %d | crates/minisatip/src/ca.rs:807 |
| 1768 | LOG_AND_RETURN | assemble_packet: len %d not valid for pid %d [%02X %02X % | crates/ms-ts/src/section.rs:74 |
| 1799 | LOG_AND_RETURN | assemble_packet: CRC check: flags %d len %d not valid for | n/a: DEV-251 — the Rust assembler does not keep filter flags; the CRC mismatch site below covers the observable case |
| 1807 | LOG | pid %d (%04X) CRC failed %08X != %08X len %d | crates/ms-ts/src/section.rs:115 |
| 1809 | hexdump | packet failed CRC  | crates/ms-ts/src/section.rs:123 |
| 1827 | LOG | adapter %d active PMT list full, dropping PMT %d | crates/ms-sched/src/pmt.rs:417 |
| 1853 | LOG_AND_RETURN | Adapter %d no longer enabled, not processing PAT | crates/ms-sched/src/pmt.rs:1799 |
| 1858 | LOG_AND_RETURN | PAT AD %d: tsid %d version %d | crates/ms-sched/src/pmt.rs:1802 |
| 1863 | LOG | PAT AD %d new version for transponder %d, version %d | crates/ms-sched/src/pmt.rs:1805 |
| 1868 | LOG | PAT AD %d new transponder %d (old %d), version %d | crates/ms-sched/src/pmt.rs:1814 |
| 1889 | LOG | PAT Adapter %d, Transponder ID %d, version %d, len %d | crates/ms-sched/src/pmt.rs:1839 |
| 1900 | LOG | Adapter %d, PMT %d sid %d (%04X), pid %d | crates/ms-sched/src/pmt.rs:1859 |
| 1941 | LOG | Caching PMT %d (%s) and filter %d as it is not present | crates/ms-sched/src/pmt.rs:1924 |
| 1984 | LOGM | %s: CAID %04X CAPID %d already exists in PMT %d | crates/ms-sched/src/pmt.rs:468 |
| 1989 | LOG | Too many CAIDs for pmt %d, discarding %04X | crates/ms-sched/src/pmt.rs:477 |
| 1995 | LOG | PMT %d discarding CA descriptor with length %d | n/a: Rust CA descriptors are slices, a negative length cannot occur |
| 1999 | LOG | PMT %d PI pos %d caid %04X => pid %04X (%d), index %d | crates/ms-sched/src/pmt.rs:480 |
| 2045 | LOGM | PMT %d already has descriptor with type %d, len %d | crates/ms-sched/src/pmt.rs:539 |
| 2072 | LOGM | PMT %d pid %d already has descriptor with type %d, len %d | crates/ms-sched/src/pmt.rs:566 |
| 2110 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-sched/src/pmt.rs:1949 |
| 2118 | LOG_AND_RETURN | Adapter %d does not exist | n/a: the PMT table lookup is the adapter check in Rust |
| 2126 | LOG | PMT %d does not exist | crates/ms-sched/src/pmt.rs:1963 |
| 2159 | LOG | new PMT %d AD %d, pid: %04X (%d), filter %d, len %d, pi_l | crates/ms-sched/src/pmt.rs:2031 |
| 2195 | LOG | PMT pid %d - stream pid %04X (%d), type %d%s, es_len %d | n/a: the ms-psi PMT parser does not expose es_len/position/AC-3 flags here |
| 2202 | LOGM | pmt processing complete, es_len + i %d, len %d, es_len %d | n/a: same parser model as 2195 |
| 2259 | DEBUGM | Processing SDT for transponder %d (%x) with length %d, sd | n/a: DEV-212 — SDT parsing is done by ms-psi; this loop has no Rust counterpart |
| 2270 | DEBUGM | Detected service ID %d (%X) status:%d CA:%d, pos %d len  | n/a: DEV-212 |
| 2273 | DEBUGM | %s: no PMT found for sid %d (%X) | n/a: DEV-212 |
| 2295 | LOG | SDT PMT %d: name %s provider %s, sid: %d (%X) | n/a: DEV-212 |
| 2305 | LOGM | starting PMT %d, pid %d, sid %d, filter %d for channel: %s | crates/ms-sched/src/pmt.rs:965 |
| 2319 | LOGM | stopping PMT %d pid %d sid %d filter %d for channel %s | crates/ms-sched/src/pmt.rs:993 |
| 2358 | LOG | PMT %d keeps running without pid %d | crates/ms-sched/src/pmt.rs:1035 |
| 2367 | LOGM | %s: pid %d adapter %d pmt %d, channel %s | crates/ms-sched/src/pmt.rs:1050 |
| 2382 | LOGM | found active pid %d for pmt id %d, pid %d | crates/ms-sched/src/pmt.rs:1072 |
| 2425 | DEBUGM | SCA %d cannot handle CAID %04X, skipping | n/a: the Rust offer no longer has the CA slot id at this point |
| 2432 | LOG | PMT %d CA descriptor %d does not fit, truncating at %d | crates/ms-ca/src/capmt.rs:172 |
| 2443 | LOGM | PMT %d added caid %04X, pid %04X, pos %d | crates/ms-ca/src/capmt.rs:187 |
