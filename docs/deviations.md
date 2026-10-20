# Deliberate deviations from the C++ reference

Behaviour is defined by the C++ binary. Every intentional difference from it gets
one row here, with the reason. Empty until a deviation is accepted.

| ID | Area | C++ behaviour | Rust behaviour | Reason |
|---|---|---|---|---|
| DEV-001 | ms-ts section assembly | After PUSI, `assemble_packet` always skips one byte and ignores the pointer_field value | Skips `1 + pointer_field` bytes as required by EN 300 468 | A non-zero pointer_field otherwise corrupts the next section |
| DEV-002 | ms-ts section assembly | `assemble_packet` returns at most one section per TS packet | `SectionAssembler::push` emits every complete section in a packet | Several PSI sections may legally share one packet |
| DEV-003 | ms-ts PID filters | Global atomic filter array; slots claimed with compare-and-swap under a mutex | Owned `FilterTable`; callers wrap it in a `Mutex` for threads; ids are per-table slots | PLAN §2.4 forbids shared mutable globals |
| DEV-004 | ms-psi charset | `dvb_get_string` writes to a caller buffer and uses a process-global override | Returns `Option<Vec<u8>>` (same `dst_len - 1` bound) with the override passed per call | Ownership and no globals; decoded bytes and failure cases are unchanged |
