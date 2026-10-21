# tables.cpp logging coverage

Ported 9 / not applicable 4 (13 log sites). All sites are in
`crates/ms-sched/src/pmt.rs`, `crates/minisatip/src/ca.rs` or
`crates/ms-ca/src/program.rs`, so each call passes `ms_log::LOG_TABLES`
explicitly where the module's `DEFAULT_LOG` belongs to another C++ source.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 71 | LOG_AND_RETURN | No free CA slots for %p | n/a: the Rust CA list is a `Vec` that grows; there is no fixed `MAX_CA` slot allocation to fail |
| 154 | LOG | %s: No adapter %d found | crates/minisatip/src/ca.rs:313 |
| 168 | LOG | CA %d can handle CAID %04X mask %04X on adapter %d at posi | crates/minisatip/src/ca.rs:323 |
| 171 | LOG | CA not enabled %d or too many added CAIDs %d | n/a: the Rust CAID table is a growable `Vec`; there is no disabled/`MAX_CAID` overflow branch to log |
| 185 | LOGM | CA %d will handle adapter %d | crates/ms-sched/src/pmt.rs:1194 |
| 190 | LOGM | CA %d is disabled for adapter %d | n/a: `ca_init_dev` has no failure return in the Rust `CaSink`; a CA that cannot handle the adapter is simply not registered |
| 200 | LOGM | %s: match caid %04X (%d/%d) with CA caid %04X and mask %0 | crates/ms-ca/src/program.rs:148 |
| 241 | LOGM | Closing pmt %d for ca %d and adapter %d | crates/ms-sched/src/pmt.rs:1414 |
| 260 | LOGM | Closing pmt %d for adapter %d | crates/ms-sched/src/pmt.rs:1440 |
| 290 | LOG | CAID %04X and mask %04X matched PMT %d | n/a: the per-CAID match is decided inside each `CaSink::accepts_service`; there is no generic match loop to log from |
| 309 | LOGM | PMT %d -> CA %d: %s | crates/ms-sched/src/pmt.rs:1341 |
| 332 | LOGM | In processing PMT %d, ca %d, CA matched %d, update %d,  | crates/ms-sched/src/pmt.rs:1382 |
| 349 | LOG | Sending PMT %d to all CAs: ad_ca_mask %X, pmt_ca_mask %X | crates/ms-sched/src/pmt.rs:1297 |
