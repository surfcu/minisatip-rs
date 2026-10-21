# netceiver.cpp logging coverage

C++ source: `src/netceiver.cpp` (read-only oracle at `~/upstream-repo/minisatip`).
Rust counterparts: `crates/ms-tuner/src/netceiver/{backend,discover,api}.rs`.

All sites use `LOG` (`LOG_GENERAL`), not the file's netceiver-module mask. The
Rust port replaces the OS DVR pipe with an in-memory `NetcvShared` queue.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---:|---|---|---|
| 59 | LOG | `netceiver: receiver instance is NULL (id=%d)` | crates/ms-tuner/src/netceiver/backend.rs:263 |
| 61 | LOG | `netceiver: delete receiver instance for adapter %d` | crates/ms-tuner/src/netceiver/backend.rs:266 |
| 85 | LOG | `netceiver: creating pipe failed (%s)` | n/a: the port has no OS pipe; TS data lands in the in-memory `NetcvShared` queue |
| 87 | LOG | `netceiver pipe buffer size change failed (%s)` | n/a: no OS pipe buffer to resize |
| 90 | LOG | `netceiver: creating DVR pipe for adapter %d  -> dvr: %d` | n/a: no DVR pipe/fd in the port |
| 98 | LOG | `netceiver: set_pid for adapter %d, pid %d, err %d` | crates/ms-tuner/src/netceiver/backend.rs:90 |
| 113 | LOG | `netceiver: del_pid for aid %d, pid %d, err %d` | crates/ms-tuner/src/netceiver/backend.rs:113 |
| 131 | LOG | `netceiver: call netcv_commit for adapter %d want_tune=%d` | crates/ms-tuner/src/netceiver/backend.rs:150 |
| 143 | LOG | `netceiver: add a new receiver instance for adapter %d` | crates/ms-tuner/src/netceiver/backend.rs:157 |
| 186 | LOG | `netceiver: netcv_commit tune tp->sys=%d` | crates/ms-tuner/src/netceiver/backend.rs:166 |
| 237 | LOG | `netceiver: adapter %d tuning to %d%s pol:%s sr:%d fec:%s ` | crates/ms-tuner/src/netceiver/backend.rs:173 |
| 270 | LOG | `netceiver: adapter %d tuning to %d sr:%d delsys:%s mod:%s` | crates/ms-tuner/src/netceiver/backend.rs:186 |
| 306 | LOG | `netceiver: DVB-T: unknown bandwith: %d` | crates/ms-tuner/src/netceiver/backend.rs:212 |
| 330 | LOG | `netceiver: adapter %d tuning to %d inv: %d mtype: %d ` | crates/ms-tuner/src/netceiver/backend.rs:215 |
| 338 | LOG | `netceiver: adapter %d tuning not supported for type %d` | crates/ms-tuner/src/netceiver/backend.rs:230 |
| 347 | LOG | `netceiver: Tuning receiver failed` | crates/ms-tuner/src/netceiver/backend.rs:237 |
| 366 | LOG | `netceiver: seting PIDs failed` | crates/ms-tuner/src/netceiver/backend.rs:248 |
| 370 | LOG | `netceiver: removing all PIDs failed` | crates/ms-tuner/src/netceiver/backend.rs:251 |
| 381 | LOG | `netceiver: called netcv_tune for adapter %d` | crates/ms-tuner/src/netceiver/backend.rs:139 |
| 424 | LOG | `network interface %s not available` | crates/ms-tuner/src/netceiver/discover.rs:84 |
| 431 | LOG | `Netceiver init failed` | crates/ms-tuner/src/netceiver/discover.rs:89 |
| 435 | LOG | `Netceiver API socket init failed` | crates/ms-tuner/src/netceiver/discover.rs:92 |
| 463 | LOG | `%s` (the search/`Found NetCeiver` stringstream) | crates/ms-tuner/src/netceiver/discover.rs:125 |
| 541 | LOG | `%s` (the `netceiver: adding ...` stringstream) | crates/ms-tuner/src/netceiver/discover.rs:171 |
| 566 | LOG | `netceiver: TS data malformed: buf[0]=0x%02x len=%lu` | crates/ms-tuner/src/netceiver/api.rs:80 |
| 574 | LOG | `netceiver: not all data forwarded (%s)` | crates/ms-tuner/src/netceiver/api.rs:94 |

ported 23 / not applicable 3

Test: `crates/ms-tuner/tests/logging_netceiver.rs` asserts the exact
`network interface ... not available` line.
