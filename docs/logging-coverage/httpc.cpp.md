# `src/httpc.cpp` (`#define DEFAULT_LOG LOG_HTTP`)

Ported **4** / not applicable **7** (11 active log sites).

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 34 | LOG_AND_RETURN | `Could not add new http client` | n/a: `fetch_satip_xml` has no client table or slot limit |
| 43 | LOG | `returning new http client %d` | n/a: no persistent http client id |
| 58 | LOGM | `Stopping http client %d` | n/a: no persistent client record to stop |
| 65 | LOG | `HTTP Client record not found for sockets id %d, http client id %d` | n/a: no client table (`http_client_close`) |
| 80 | LOG | `HTTP Client record not found for sockets id %d, http client id %d` | n/a: no client table (`http_client_read`) |
| 88 | LOGM | `%s: sending to %d: %s` | `crates/ms-tuner/src/satipc/config.rs:308` |
| 107 | LOG_AND_RETURN | `Only http support for %s` | `crates/ms-tuner/src/satipc/config.rs:264` |
| 112 | LOG_AND_RETURN | `Could not add http client` | n/a: no client table |
| 131 | LOG_AND_RETURN | `%s: connect to %s:%d failed` | `crates/ms-tuner/src/satipc/config.rs:289` |
| 138 | LOG_AND_RETURN | `%s: sockets_add failed` | n/a: no `sockets_add`; connect errors return earlier |
| 144 | LOGM | `%s url %s using handle %d s_id %d` | `crates/ms-tuner/src/satipc/config.rs:300` |
