# dvbapi.cpp logging coverage

Ported 38 / not applicable 4 (42 active log sites; the hexdump at line 787 is
commented out in C++ and is not a site).

`DEFAULT_LOG` is `LOG_DVBAPI`. Logs that live in `minisatip/src/ca.rs` pass
`ms_log::LOG_DVBAPI` explicitly because that module's `DEFAULT_LOG` is
`LOG_PMT`.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 91 | LOG | write to dvbapi socket failed (%d out of %d), closing soc | crates/minisatip/src/ca.rs:1126 |
| 187 | LOG | dvbapi: changing endianness from %06X to %08X | crates/ms-ca-drivers/src/dvbapi/client.rs:424 |
| 193 | LOG | dvbapi read from socket %d the following data (%d bytes) | crates/ms-ca-drivers/src/dvbapi/client.rs:434 |
| 219 | LOG | dvbapi: server version %d, build %d, found, name = %s | crates/ms-ca-drivers/src/dvbapi/client.rs:498 |
| 226 | LOG0 | Oscam build %d (< 11553) is not supported after minisa | crates/ms-ca-drivers/src/dvbapi/client.rs:507 |
| 257 | LOG | dvbapi set filter for pid %04X (%d), key %d, demux %d,  | crates/ms-ca-drivers/src/dvbapi/client.rs:533 |
| 262 | LOGM | filter: %02X %02X %02X %02X %02X, mask: %02X %02X %02X | crates/ms-ca-drivers/src/dvbapi/client.rs:543 |
| 297 | LOG | dvbapi: DMX_SET_FILTER failed, fid %d, index %d | crates/minisatip/src/ca.rs:560 |
| 300 | LOG | dvbapi: filter for pid %d and key %d already exists, fid | crates/ms-ca-drivers/src/dvbapi/client.rs:566 |
| 332 | LOG | dvbapi: received DMX_STOP for key %d, index %d, adapter  | crates/ms-ca-drivers/src/dvbapi/client.rs:611 |
| 351 | LOG | received DVBAPI_CA_SET_PID | crates/ms-ca-drivers/src/dvbapi/client.rs:468 |
| 376 | LOG | dvbapi: received DVBAPI_CA_SET_DESCR, key %d parity %d,  | crates/ms-ca-drivers/src/dvbapi/client.rs:663 |
| 385 | LOG | dvbapi: invalid DVBAPI_CA_SET_DESCR, key %d parity %d, k  | crates/ms-ca-drivers/src/dvbapi/client.rs:639 (`%p` normalised to the slot id, DEV-250) |
| 439 | LOG | dvbapi: ECM_INFO: key %d, SID = %04X, CAID = %04X (%s), P | crates/ms-ca-drivers/src/dvbapi/client.rs:691 |
| 457 | LOG | dvbapi: received DVBAPI_CA_SET_DESCR_MODE, key %d, algo %d | crates/ms-ca-drivers/src/dvbapi/client.rs:479 |
| 471 | LOG | dvbapi: unknown operation: %02X %02X %02X %02X %02X %02X | crates/ms-ca-drivers/src/dvbapi/client.rs:1198 |
| 540 | LOG | Key %d truncating stream list, dvbapi buffer full | crates/ms-ca-drivers/src/dvbapi/proto.rs:164 |
| 549 | LOGM | Key %d adding stream pid %d (%X) type %d (%x) | crates/ms-ca-drivers/src/dvbapi/proto.rs:173 |
| 561 | LOG | dvbapi key %d: PMT %d update, CAPMT unchanged (%08X), not | crates/ms-ca-drivers/src/dvbapi/client.rs:880 |
| 567 | LOG | Sending pmt %d to dvbapi server for pid %d, Channel ID %0 | crates/ms-ca-drivers/src/dvbapi/client.rs:889 |
| 584 | LOG | requested dvbapi close for sock %d, sock_id %d | n/a: the Rust client is not a sockets-table entry, so `s->id` does not exist; `Dvbapi::close` owns teardown |
| 636 | LOG | Key %d active but no active filter, closing  | crates/ms-ca-drivers/src/dvbapi/client.rs:1078 |
| 669 | LOG_AND_RETURN | %s: connect to %s failed | crates/minisatip/src/ca.rs:1083 |
| 677 | LOG_AND_RETURN | %s: sockets_add failed | n/a: the DVBAPI connection is a plain `TcpStream` in a helper thread, not a `sockets_add` entry |
| 720 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-ca-drivers/src/dvbapi/client.rs:994 |
| 726 | LOG_AND_RETURN | %s: key is null pid %d and filter %d | crates/ms-ca-drivers/src/dvbapi/client.rs:1000 |
| 730 | LOG_AND_RETURN | %s: key is null pid %d and filter %d | crates/ms-ca-drivers/src/dvbapi/client.rs:1000 (one check replaces C++'s pre-lock and post-lock checks) |
| 734 | LOG_AND_RETURN | %s: PMT not found for pid %d and filter %d | crates/ms-ca-drivers/src/dvbapi/client.rs:1009 |
| 739 | LOG_AND_RETURN | %s: filter %d not found | crates/ms-ca-drivers/src/dvbapi/client.rs:994 |
| 770 | LOG | dvbapi: sending ECM key %d for pid %04X (%d), ecm_parity | crates/ms-ca-drivers/src/dvbapi/client.rs:1041 |
| 817 | LOG_AND_RETURN | %s: PMT %d not found  | crates/ms-ca-drivers/src/dvbapi/client.rs:738 |
| 820 | LOG_AND_RETURN | %s: no free key id | crates/ms-ca-drivers/src/dvbapi/client.rs:751 |
| 849 | LOG | returning new key %d for adapter %d, pmt %d pid %d sid %0 | crates/ms-ca-drivers/src/dvbapi/client.rs:779 |
| 920 | LOG | Stopped key %d, active keys %d, sock %d, pmt pid %d, sid | crates/ms-ca-drivers/src/dvbapi/client.rs:833 |
| 931 | LOG | %s: adapter %d, pmt %d, pid %d, update %d | crates/ms-ca-drivers/src/dvbapi/client.rs:913 |
| 935 | LOG_AND_RETURN | %s: PMT %d is gone, not adding a key | n/a: the scheduler only offers live PMTs to the registered CA (`CaService` has no enabled flag) |
| 940 | LOG_AND_RETURN | %s: Disabling dvbapi on DDCI adapter %d | n/a: DDCI adapters are served by the DDCI CA, never offered to DVBAPI |
| 946 | LOG_AND_RETURN | %s: dvbapi socket is not open, adapter %d, pmt %d | crates/ms-ca-drivers/src/dvbapi/client.rs:921 |
| 970 | LOG_AND_RETURN | Could not add key for pmt %d | crates/ms-ca-drivers/src/dvbapi/client.rs:942 |
| 992 | LOG | %s: deleted key %d, PMT pid %d, sid %d (%X), PMT %d | crates/ms-ca-drivers/src/dvbapi/client.rs:964 |
| 1047 | LOG | Descramble watchdog: no CW for PMT %d, restarting it for  | crates/ms-ca-drivers/src/dvbapi/client.rs:1134 |
| 1074 | LOG | unregistering dvbapi as the socket is closed | crates/ms-ca-drivers/src/dvbapi/client.rs:405 |
