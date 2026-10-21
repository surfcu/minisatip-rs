# `src/srt.cpp` (`#define DEFAULT_LOG LOG_STREAM`)

Ported **11** / not applicable **0** (11 active log sites).

`LOG` is always mask `LOG_GENERAL` (`logging.h:56`), so all ported sites use `ms_log::log!`.
`LOG_STREAM` is only the file's `DEFAULT_LOG`; this file has no `LOGM`/`DEBUGM` site.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 78 | LOG | `SRT socket %d closed (not connected), streamid='%s'` | `crates/ms-net/src/srt.rs:300` |
| 94 | LOG | `SRT socket %d returned to pending pool for reuse, streamid='%s'` | `crates/ms-net/src/srt.rs:316` |
| 104 | LOG | `SRT accept: empty streamid, rejecting connection` | `crates/ms-net/src/srt.rs:99` |
| 112 | LOG | `SRT accept: pending pool full (%d), rejecting connection` | `crates/ms-net/src/srt.rs:104` |
| 127 | LOG | `SRT accepted connection srt_sock=%d, streamid='%s' (%zu pending)` | `crates/ms-net/src/srt.rs:119` |
| 146 | LOG_AND_RETURN | `SRT listener: UDP bind failed on port %d` | `crates/minisatip/src/runtime.rs:2282` |
| 154 | LOG_AND_RETURN | `SRT listener: srt_create_socket failed: %s` | `crates/ms-net/src/srt.rs:213` |
| 166 | LOG_AND_RETURN | `SRT listener: srt_bind_acquire failed: %s` | `crates/ms-net/src/srt.rs:222` |
| 176 | LOG_AND_RETURN | `SRT listener: srt_listen_callback failed: %s` | `crates/ms-net/src/srt.rs:231` |
| 184 | LOG_AND_RETURN | `SRT listener: srt_listen failed: %s` | `crates/ms-net/src/srt.rs:240` |
| 195 | LOG | `SRT listener started on port %d, srt_sock=%d, udp_fd=%d` | `crates/minisatip/src/runtime.rs:2295` |

Note: the `%s` error text uses `ms_srt::last_error()` (`srt_getlasterror_str`), since `SrtListener::init` has no handle on the concrete `LibSrtOps`.
