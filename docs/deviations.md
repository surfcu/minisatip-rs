# Deliberate deviations from the C++ reference

Behaviour is defined by the C++ binary. Every intentional difference from it gets
one row here, with the reason. Empty until a deviation is accepted.

| ID | Area | C++ behaviour | Rust behaviour | Reason |
|---|---|---|---|---|
| DEV-001 | ms-ts section assembly | After PUSI, `assemble_packet` always skips one byte and ignores the pointer_field value | Skips `1 + pointer_field` bytes as required by EN 300 468 | A non-zero pointer_field otherwise corrupts the next section |
| DEV-002 | ms-ts section assembly | `assemble_packet` returns at most one section per TS packet | `SectionAssembler::push` emits every complete section in a packet | Several PSI sections may legally share one packet |
| DEV-003 | ms-ts PID filters | Global atomic filter array; slots claimed with compare-and-swap under a mutex | Owned `FilterTable`; callers wrap it in a `Mutex` for threads; ids are per-table slots | PLAN §2.4 forbids shared mutable globals |
| DEV-004 | ms-psi charset | `dvb_get_string` writes to a caller buffer and uses a process-global override | Returns `Option<Vec<u8>>` (same `dst_len - 1` bound) with the override passed per call | Ownership and no globals; decoded bytes and failure cases are unchanged |
| DEV-005 | ms-psi SDT | `process_sdt` accepts only `table_id` 0x42 and ignores other-transport-stream SDT (0x46) | `Sdt::parse` accepts both 0x42 and 0x46 | EN 300 468 defines both; the other-TS table describes services on other muxes |
| DEV-006 | ms-psi PSI parsers | Inner descriptor lengths are trusted; malformed or short CA descriptors can read past the section (undefined behaviour) | Every offset is bounds-checked and malformed lengths return `PsiError`; CA getters return `None` on a short descriptor | Hostile or corrupt input must never panic (fuzz targets) |
| DEV-007 | ms-psi CRC | CRC is checked only by the filter layer when `FILTER_CRC` is set; `process_pat`/`process_pmt`/`process_sdt` never verify it | Parsers take `CrcCheck`: `Skip` matches the C++ parser entry points, `Verify` performs the same check as `FILTER_CRC` | Keeps the C++ test vectors usable directly while the Rust data path can verify |
| DEV-008 | ms-psi SDT names | `process_sdt` decodes provider/service names only while the PMT name is empty, so later SDTs do not refresh it | `Sdt::parse` always decodes names from the service descriptor | Parsers are pure and hold no per-service cache |
