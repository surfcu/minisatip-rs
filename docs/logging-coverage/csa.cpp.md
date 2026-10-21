# csa.cpp logging coverage

C++ source: `src/csa.cpp` (read-only oracle at `~/upstream-repo/minisatip`).
Rust counterpart: `crates/ms-ca-drivers/src/descramble/csa.rs`.

The Rust crate links `dvbcsa_bs_key_set_ecm` at compile time through the
`dvbcsa-icam` cargo feature, so the C++ weak-symbol runtime probe has no direct
equivalent.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---:|---|---|---|
| 73 | LOGL | `minisatip required libdvbcsa with ICAM support. Please ` | n/a: no weak-symbol probe; iCAM availability is the compile-time `dvbcsa-icam` feature (the symbol is declared only under it) |
| 125 | LOG | `libdvbcsa does not support ICAM (dvbcsa_bs_key_set_ecm missing)` | crates/ms-ca-drivers/src/descramble/csa.rs:46 (`#[cfg(not(feature = "dvbcsa-icam"))]`) |

ported 1 / not applicable 1

Test: `crates/ms-ca-drivers/tests/logging_csa.rs` (run with
`--features dvbcsa`, without `dvbcsa-icam`) asserts the exact line.
