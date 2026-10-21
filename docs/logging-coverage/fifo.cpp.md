# `src/utils/fifo.cpp` (includes `logging.h`, defines no `DEFAULT_LOG`; `LOG` = mask `LOG_GENERAL`)

Ported **3** / not applicable **1** / pending **0** (4 active log sites).

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 39 | LOG_AND_RETURN | `Could not allocate FIFO %d` | n/a: Rust `vec![0; size]` allocation is infallible (aborts on OOM) |
| 64 | LOG | `Not enough space in FIFO created at %s:%d: need %u, available %u` | `crates/ms-net/src/fifo.rs:88` |
| 70 | LOG | `Overwriting %d bytes in the FIFO created at %s:%d` | `crates/ms-net/src/fifo.rs:99` |
| 93 | LOG | `Lost %jd bytes in fifo created at %s:%d` | `crates/ms-ca-drivers/src/ddci/fifo.rs:51` (`ms-net` reads with `peek`/`skip` and cannot drop unread bytes) |

Note: `SFIFO.file/line` come from the `create_fifo` macro. The Rust `Fifo` stores `std::panic::Location::caller()` (`#[track_caller]` on `Fifo::new`), so the reported source is the nearest Rust creation site rather than the C++ `socketworks.cpp` line.
