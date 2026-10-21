# hw_descrambler.cpp logging coverage

C++ source: `src/hw_descrambler.cpp` (read-only oracle at `~/upstream-repo/minisatip`).
Rust counterpart: `crates/ms-ca-drivers/src/hw_descrambler`.

Every `LOG` in this file is `LOG_GENERAL` (the file's `DEFAULT_LOG` is
`LOG_DVBCA`, but it uses `LOG`, not `LOGM`). `strerror(errno)` text is produced
by `ms_log::errno`/`ms_log::strerror`, and `result=%d%s` by
`result_code`/`result_suffix`; only pointer formatting would need
normalisation, and no ported line prints a pointer.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---:|---|---|---|
| 54 | LOG | `hw_descrambler: CA_GET_DESCR_INFO returned %u hardware ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:289 |
| 59 | LOG | `hw_descrambler: CA_GET_DESCR_INFO unavailable, defaulting to %d slots` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:296 |
| 103 | LOG | `hw_descrambler: opened fallback %s (ca_fd = %d) for ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:266 |
| 112 | LOG | `hw_descrambler: successfully opened shared %s (ca_fd = ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:310 |
| 117 | LOG | `hw_descrambler: failed to open %s: %s (errno %d)` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:278 |
| 153 | LOG | `hw_descrambler: Allocated hardware descrambler slot index ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:345 |
| 160 | LOG | `hw_descrambler: Error: all slots full on physical adapter %d for ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:337 |
| 176 | LOG | `hw_descrambler: Released hardware descrambler slot index ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:361 |
| 234 | LOG | `hw_descrambler: closing shared ca_fd %d for physical adapter ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:620 |
| 250 | LOG | `hw_descrambler: hw_create_key cw_id = %d, algo = %d` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:226 |
| 265 | LOG | `hw_descrambler: --hw-descrambler not enabled, skipping hw_set_cw` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:390 |
| 270 | LOG | `hw_descrambler: hw_set_cw key or pmt is nullptr` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:394 |
| 275 | LOG | `hw_descrambler: ICAM mode (CA_ALGO_DVBCSA_ICAM) not supported by ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:399 |
| 283 | LOG | `hw_descrambler: adapter for PMT %d not found` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:405 |
| 292 | LOG | `hw_descrambler: unable to obtain ca_fd for %s` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:413 |
| 306 | LOG | `hw_descrambler: slot allocation failed for PMT %d on adapter %d` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:428 |
| 311 | LOG | `hw_descrambler: hw_set_cw called for PMT %d, adapter %d ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:437 |
| 324 | LOG | `hw_descrambler: CA_SET_DESCR_MODE ioctl (slot=%d, algo=%d, mode=%d) ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:456 |
| 335 | LOG | `hw_descrambler: CA_SET_PID ioctl (pid=%d, slot=%d) -> result=%d%s` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:470 |
| 347 | LOG | `hw_descrambler: CA_SET_DESCR (DVBCSA) ioctl (slot=%d, parity=%d) ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:488 |
| 360 | LOG | `hw_descrambler: CA_SET_DESCR_DATA (AES Key) ioctl (slot=%d, ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:505 |
| 374 | LOG | `hw_descrambler: CA_SET_DESCR_DATA (AES IV) ioctl (slot=%d, ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:516 |
| 436 | LOG | `hw_descrambler: hw_ca_del_pmt called for PMT %d on ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:591 |
| 469 | LOG | `hw_descrambler: PMT %d %s: %zu pids bound, %zu removed` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:574 |
| 517 | LOG | `hw_descrambler: Initializing Hardware Descrambler ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:212 and crates/minisatip/src/hw.rs:33 (production `init_hw_descrambler` path) |
| 532 | LOG | `hw_descrambler: --hw-descrambler not enabled, hardware ` | crates/ms-ca-drivers/src/hw_descrambler/mod.rs:207 and crates/minisatip/src/runtime.rs:271 (production `--hw-descrambler` off path) |

ported 26 / not applicable 0

Test: `crates/ms-ca-drivers/tests/logging_hw_descrambler.rs` asserts the exact
`hw_set_cw`, slot and ioctl-result lines.
