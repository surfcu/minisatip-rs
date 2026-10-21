# C++ logging coverage

One table per C++ source file. Each row is one active C++ log site, in source
order:

| column | meaning |
|---|---|
| `C++ line` | line of the site in `~/upstream-repo/minisatip/src/<file>` |
| `macro` | the C++ macro: `LOG`, `LOGM`, `LOG_AND_RETURN`, `LOG0`, `LOGL`, a `DEBUG*` variant, or `hexdump`/`_dump_packets` |
| `text` | the C++ format string (truncated to 60 characters) |
| `Rust file:line or "n/a: <reason>"` | where the line is emitted in this workspace, or why the site has no Rust code point |

Ported calls carry the C++ text byte for byte; printf conversions are
translated to the equivalent Rust formatting. `LOG` maps to `ms_log::log!`
(general), `LOGM`/`DEBUGM` to `logm!`/`debugm!` with the file's
`DEFAULT_LOG`, and `hexdump`/`_dump_packets` to the matching macro. Sites
commented out in C++ are not sites and are excluded from the counts (some
files list them after the table for reference). `n/a` entries state the
reason; the deviations that apply across files are in
[`../deviations.md`](../deviations.md). A `Rust file:line` is the location when
that file was ported; later edits (such as `cargo fmt`) can move it by a few
lines, so the file path is the authoritative part.

## Summary

| C++ file | Ported | Not applicable | Total sites |
|---|---:|---:|---:|
| `adapter.cpp` | 39 | 39 | 78 |
| `aes.cpp` | 0 | 8 | 8 |
| `ca.cpp` | 117 | 118 | 235 |
| `csa.cpp` | 1 | 1 | 2 |
| `ddci.cpp` | 77 | 6 | 83 |
| `dvb.cpp` | 88 | 34 | 122 |
| `dvbapi.cpp` | 38 | 4 | 42 |
| `hw_descrambler.cpp` | 26 | 0 | 26 |
| `minisatip.cpp` | 41 | 23 | 64 |
| `netceiver.cpp` | 23 | 3 | 26 |
| `pmt.cpp` | 68 | 29 | 97 |
| `satipc.cpp` | 86 | 9 | 95 |
| `socketworks.cpp` | 59 | 42 | 101 |
| `srt.cpp` | 11 | 0 | 11 |
| `stream.cpp` | 49 | 21 | 70 |
| `tables.cpp` | 9 | 4 | 13 |
| `utils.cpp` | 16 | 24 | 40 |
| `utils/fifo.cpp` | 3 | 1 | 4 |
| `api/variables.cpp` | 1 | 0 | 1 |
| `httpc.cpp` | 4 | 7 | 11 |
| `opts.cpp` | 3 | 0 | 3 |
| **total** | **759** | **373** | **1132** |

`minisatip.cpp`'s 41 ported sites include the 8 start-up sites ported with the
logging core (its coverage file lists them as "already ported"); likewise the
`1` of `socketworks.cpp`'s 59. The remaining ported sites were added in the
per-file porting step.
