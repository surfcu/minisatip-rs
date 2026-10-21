# ddci.cpp logging coverage

Ported 77 / not applicable 6 (83 active log sites; the `LOG_AND_RETURN` at line
1402 is commented out in C++ and is not a site).

`DEFAULT_LOG` is `LOG_DDCI`. Logs emitted from `minisatip/src/runtime.rs`,
`minisatip/src/ddci.rs` and `ms-config/src/parse.rs` pass `LOG_DDCI`/`log!`
explicitly where the module hosts other sources.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 89 | LOG_AND_RETURN | pid %d invalid | crates/ms-ca-drivers/src/ddci/mod.rs:144 |
| 114 | LOG_AND_RETURN | could not add pid %d and ad %d to the mapping table | crates/ms-ca-drivers/src/ddci/mod.rs:395 |
| 121 | LOGM | Adapter %d pid %d already mapped to pmt %d, already added | crates/ms-ca-drivers/src/ddci/mod.rs:404 |
| 141 | LOG | mapped adapter %d (%d) pid %d (%d) to %d, pmt %d | crates/ms-ca-drivers/src/ddci/mod.rs:435 |
| 160 | LOGM | Mapping Table for all devices | crates/ms-ca-drivers/src/ddci/mod.rs:453 |
| 168 | LOGM | DD %d, ddpid %d, adapter %d pid %d, rewrite %d: %zu PMTS | crates/ms-ca-drivers/src/ddci/mod.rs:460 |
| 205 | LOG | Deleting ad %d pmt %d pid %d ddci_pid %d | crates/ms-ca-drivers/src/ddci/mod.rs:485 |
| 211 | LOGM | Marking pid %d deleted on adapter %d (initial ad %d pid % | crates/ms-ca-drivers/src/ddci/mod.rs:499 |
| 309 | LOG | DDCI %d CAID %04X and mask %04X matched PMT %d | crates/ms-ca-drivers/src/ddci/mod.rs:580 |
| 333 | LOG | %s: DDCI %d not enabled, skipping | crates/ms-ca-drivers/src/ddci/mod.rs:605 |
| 338 | LOG | %s: DDCI %d cannot be used for PMT %d, pid %d, sid, %d ( | crates/ms-ca-drivers/src/ddci/mod.rs:609 |
| 349 | LOG | %s: DDCI %d is initializing, skipping | crates/ms-ca-drivers/src/ddci/mod.rs:622 |
| 364 | LOG | %s: DDCI for PMT %d (sid %d) is initializing, retrying | crates/ms-ca-drivers/src/ddci/mod.rs:642 |
| 370 | LOG | %s: all DDCIs full for PMT %d (sid %d), retrying | crates/ms-ca-drivers/src/ddci/mod.rs:650 |
| 376 | LOG | %s: no suitable DDCI found for PMT %d (sid %d): not mapp | crates/ms-ca-drivers/src/ddci/mod.rs:658 |
| 381 | LOG | %s: no suitable DDCI found for PMT %d (sid %d) | crates/ms-ca-drivers/src/ddci/mod.rs:664 |
| 387 | LOG | %s: Using DDCI %d, running %d channels | crates/ms-ca-drivers/src/ddci/mod.rs:632 |
| 411 | LOG_AND_RETURN | DDCI not ready or busy at the moment: %s | crates/ms-ca-drivers/src/ddci/mod.rs:718 |
| 417 | LOG_AND_RETURN | no suitable DDCI found | crates/ms-ca-drivers/src/ddci/mod.rs:729 |
| 436 | LOG | Skip processing pmt for ddci adapter %d | crates/ms-ca-drivers/src/ddci/mod.rs:695 |
| 448 | LOG | %s: adapter %d, pmt %d, pid %d, sid %d, ddid %d, update | crates/ms-ca-drivers/src/ddci/mod.rs:703 |
| 479 | LOG | Could not find ddci device for adapter %d, ddci %d | crates/ms-ca-drivers/src/ddci/mod.rs:754 |
| 501 | LOG_AND_RETURN | No free slot found for pmt %d on DDCI %d | crates/ms-ca-drivers/src/ddci/mod.rs:777 |
| 535 | LOG | Mapping mandatory PID %d to PMT %d on DDCI %d | crates/ms-ca-drivers/src/ddci/mod.rs:801 |
| 547 | LOG | found DDCI %d for pmt %d, running channels %d, max_chann | crates/ms-ca-drivers/src/ddci/mod.rs:816 |
| 556 | LOGM | DD %d adding ECM pid %d | crates/ms-ca-drivers/src/ddci/mod.rs:836 |
| 562 | LOGM | DD %d adding stream pid %d %s | crates/ms-ca-drivers/src/ddci/mod.rs:841 |
| 580 | LOG | DDCI %d: PMT %d %s on slot %d (+%d -%d pids), keeping dd | crates/ms-ca-drivers/src/ddci/mod.rs:855 |
| 597 | LOG | Skip deleting pmt for ddci adapter %d | crates/ms-ca-drivers/src/ddci/mod.rs:871 |
| 604 | LOG | %s: pid mapping for adapter %d, pmt %d and pid %d not fou | crates/ms-ca-drivers/src/ddci/mod.rs:879 |
| 628 | LOG_AND_RETURN | %s: ddci %d already disabled | crates/ms-ca-drivers/src/ddci/mod.rs:901 |
| 647 | LOG | %s: deleting pmt id %d, sid %d (%X), pid %d, ddci %d, nam | crates/ms-ca-drivers/src/ddci/mod.rs:920 |
| 685 | LOG | adapter %d pid %d not found in the mapping table | crates/ms-ca-drivers/src/ddci/mod.rs:949 |
| 700 | LOGM | %s: transponder %d version %d, len %d | crates/ms-ca-drivers/src/ddci/mod.rs:963 |
| 704 | hexdump | PAT Created CRC %08X:  | crates/ms-ca-drivers/src/ddci/mod.rs:969 |
| 771 | LOGM | Adding PMT %d to SDT, sid %d | crates/ms-ca-drivers/src/ddci/mod.rs:1004 |
| 773 | LOG | adapter %d pid %d not found in the mapping table | crates/ms-ca-drivers/src/ddci/mod.rs:1006 |
| 918 | LOGM | %s: PMT %d AD %d, pid: %04X (%d), ver %d, sid %04X (%d) | crates/ms-ca-drivers/src/ddci/mod.rs:1084 |
| 926 | LOG | %s: truncating PMT %d program CA list, buffer full | crates/ms-ca-drivers/src/ddci/mod.rs:1100 |
| 937 | LOGM | %s: pmt %d added caid %04X, pid %04X | crates/ms-ca-drivers/src/ddci/mod.rs:1112 |
| 952 | LOGM | %s: adapter %d pid %d not requested by the client | crates/ms-ca-drivers/src/ddci/mod.rs:1128 |
| 964 | LOG | %s: truncating PMT %d stream list, buffer full | crates/ms-ca-drivers/src/ddci/mod.rs:1138 |
| 996 | LOGM | %s: pmt %d added pid %04X, type %02X, es_len %d | crates/ms-ca-drivers/src/ddci/mod.rs:1161 |
| 1006 | LOGM | %s: PMT %d content changed, bumping ver %d -> %d | crates/ms-ca-drivers/src/ddci/mod.rs:1174 |
| 1028 | LOG | DD %d: naming PMT %d (sid %d) %s | crates/ms-ca-drivers/src/ddci/mod.rs:1216 |
| 1087 | LOG | %s: could not find PMT %d adapter %d and pid %d to mappin | crates/ms-ca-drivers/src/ddci/mod.rs:1288 |
| 1118 | DEBUGM | Skipping pid %d, ddci_pid %d on adapter %d DDCI %d | crates/ms-ca-drivers/src/ddci/mod.rs:1326 |
| 1128 | DEBUGM | DDCI %d pid %d -> AD %d pid %d, pos %d, len %d, left %j | crates/ms-ca-drivers/src/ddci/mod.rs:1340 |
| 1132 | dump_packets | DDCI -> AD | crates/ms-ca-drivers/src/ddci/mod.rs:1351 |
| 1134 | DEBUGM | popped %d bytes from fifo up to index %d, rlen %d, lbuf  | crates/ms-ca-drivers/src/ddci/mod.rs:1354 |
| 1170 | DEBUGM | Using pid %d in adapter %d as pid %d in DDCI %d | crates/ms-ca-drivers/src/ddci/mod.rs:1388 |
| 1193 | dump_packets | ddci_process_ts -> DD | crates/ms-ca-drivers/src/ddci/mod.rs:1413 |
| 1205 | DEBUGM | pos %d of %d, mapping pid %d to %d, buf pos %d | crates/ms-ca-drivers/src/ddci/mod.rs:1419 |
| 1211 | hexdump | PSI ->  | crates/ms-ca-drivers/src/ddci/mod.rs:1434 |
| 1231 | DEBUGM | writing %d bytes to DDCI device %d, fd %d, sock %d | crates/ms-ca-drivers/src/ddci/mod.rs:1444 |
| 1235 | LOG | %s: write incomplete to DDCI %d,fd %d, wrote %d out of %d | crates/ms-ca-drivers/src/ddci/mod.rs:1453 |
| 1281 | LOG | Registered DDCI CA %d | crates/minisatip/src/ca.rs:164 |
| 1284 | LOGM | %s: ddci %d add pid %d | n/a: `ms_tuner::Tuner` has no `set_pid` hook; PID adds go through the scheduler |
| 1289 | LOGM | %s: ddci %d del pid %d | n/a: `ms_tuner::Tuner` has no `del_filters` hook |
| 1298 | DEBUGM | process_dmx not called as s->rlen %d | n/a: `push_from_cam` receives CAM bytes already read; there is no `read_dmx`/`s->rlen` split |
| 1311 | LOG | dropping %d bytes for ddci_adapter %d | crates/ms-ca-drivers/src/ddci/mod.rs:1501 |
| 1312 | DEBUGM | pushed %d bytes to the adapter buffer from %d [write_index | crates/ms-ca-drivers/src/ddci/mod.rs:1503 |
| 1314 | dump_packets | DDCI ->FIFO  | crates/ms-ca-drivers/src/ddci/mod.rs:1510 |
| 1315 | dump_packets | SOURCE  | crates/ms-ca-drivers/src/ddci/mod.rs:1511 |
| 1354 | LOG | DDCI opening [%d] adapter %d and frontend %d | crates/minisatip/src/ddci.rs:211 |
| 1362 | LOG | %s: could not open %s in WRONLY mode error %d: %s | crates/minisatip/src/ddci.rs:232 |
| 1370 | LOG | %s: could not open %s in RDONLY mode error %d: %s | crates/minisatip/src/ddci.rs:246 |
| 1382 | LOG | Failed to set socket %d nonblocking: %d | n/a: the Rust opens the CI node with `O_NONBLOCK` directly, so there is no separate failure |
| 1385 | LOG | Failed to set socket %d nonblocking: %d | n/a: same as 1382 |
| 1389 | LOG | pipe failed errno %d: %s | n/a: the Rust has no `DDCI_TEST` pipe path |
| 1418 | LOG | opened DDCI adapter %d fe:%d dvr:%d | crates/minisatip/src/ddci.rs:256 |
| 1444 | LOG_AND_RETURN | DDCI %d no longer enabled, not processing CAT | crates/ms-ca-drivers/src/ddci/mod.rs:1524 |
| 1455 | LOG | CAT DDCI %d ver %d, len %d | crates/ms-ca-drivers/src/ddci/mod.rs:1532 |
| 1468 | LOG | CAT pos %d caid %04X, pid %d | crates/ms-ca-drivers/src/ddci/mod.rs:1545 |
| 1478 | LOG | CAT pid %d already in use, skipping CAT | crates/ms-ca-drivers/src/ddci/mod.rs:1556 |
| 1515 | LOG | %s already exists, not saving | crates/ms-ca-drivers/src/ddci/mod.rs:1581 |
| 1579 | LOGM | Adding channel %d: DDCIs: %s | crates/ms-ca-drivers/src/ddci/mod.rs:1627 |
| 1582 | LOG | Loaded %d channels from %s | crates/ms-ca-drivers/src/ddci/mod.rs:1629 |
| 1597 | LOG | Disable passing the CAT to DDCI %d | crates/ms-config/src/parse.rs:1188 |
| 1606 | LOG | Disable passing the CAT to DDCI %d | crates/ms-config/src/parse.rs:1188 (the Rust range loop collapses both C++ branches) |
| 1629 | LOG | DVBCI device detection disabled | crates/minisatip/src/runtime.rs:274 |
| 1637 | LOGM | %s: Starting %s with index %d | crates/minisatip/src/runtime.rs:1594 |
| 1658 | LOGM | %s: adding %d %d to the list of devices | crates/minisatip/src/runtime.rs:1603 |
