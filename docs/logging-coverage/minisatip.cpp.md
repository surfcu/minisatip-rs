# `src/minisatip.cpp` (`#define DEFAULT_LOG LOG_HTTP`, `LOG_SSDP` from 1695 to 1868)

Ported **33** / not applicable **23** (3 compile-time-disabled branches + 20
`main()` start-up sites outside this request-path task) / already ported
elsewhere **8** (64 active log sites).

`LOG` is the general module (`logging.h:56`), so every plain `LOG`/
`LOG_AND_RETURN` uses `ms_log::log!` here. `LOGM` resolves the file's
`DEFAULT_LOG`: the SSDP sites between lines 1695 and 1868 are
`ms_log::logl!(ms_log::LOG_SSDP, ...)`. The `DEFAULT_LOG` of the read/reply
lines only gates whether the request/response buffer is appended to the
message (`log_buf`); it does not change the logging module.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 280 | LOG0 | `%s` (version banner) | already ported: `crates/minisatip/src/main.rs:86` |
| 284 | LOG | `%s` (built_info) | already ported: `crates/minisatip/src/main.rs:87-89` |
| 750 | FAIL | `Invalid --bind-dev value` | already ported: `crates/ms-config/src/parse.rs:687-690` (logged by `main.rs:44`) |
| 770 | LOG | `The LOG option has changed, please run ./minisatip -h` | `crates/ms-config/src/parse.rs:708` |
| 774 | LOG | `module %.*s not recognised` | `crates/ms-config/src/parse.rs:715` |
| 820 | LOG0 | `IPv6 mode is enabled` | `crates/ms-config/src/parse.rs:740` |
| 887 | LOG | `playlist length is too big %zd bytes` | `crates/ms-config/src/parse.rs:793` |
| 953 | LOG | `%s was not compiled with DVBAPI support, please install ...` | n/a: `DISABLE_DVBAPI` build only |
| 975 | LOG | `%s was not compiled with satip client support, please change ...` | n/a: `DISABLE_SATIPCLIENT` build only |
| 1025 | LOG | `%s was not compiled with netceiver client support, please change ...` | n/a: `DISABLE_NETCVCLIENT` build only |
| 1070 | LOG | `Not a valid path for the xml file` | `crates/ms-config/src/parse.rs:833` |
| 1078 | LOG | `Demux device can be 0-3 and not %d` | `crates/ms-config/src/parse.rs:842` |
| 1110 | LOG | `Invalid --sdt-charset value, expected auto or latin1` | already ported: `crates/ms-config/src/parse.rs:856-857`, logged by `main.rs:44` |
| 1123 | FAIL | `Invalid bind configuration` | `crates/ms-config/src/parse.rs:1497-1503` (reason + message, logged by `main.rs:44`); `crates/minisatip/src/runtime.rs:1242` for the `resolve_bind_opts` failure |
| 1138 | LOG | `Listening configuration RTSP:%s (bind address: %s, bind device: %s), HTTP:%s ...` | `crates/minisatip/src/runtime.rs:1266` |
| 1192 | LOG | `Received RTSP over tcp packet (sock_id %d) sid %d, rlen %d, packet len: %d, ...` | `crates/ms-net/src/rtsp.rs:164` (caller passes `conn_id` and `sid = -1`) |
| 1214 | LOG | `Discarding %d bytes from the socket buffer, request > %d, consider increasing  RBUF` | `crates/ms-net/src/rtsp.rs:198` |
| 1220 | LOG | `RTSP header not complete: %d\n%s` | `crates/ms-net/src/rtsp.rs:206` |
| 1229 | LOG | `Read RTSP (sock %d, handle %d) [%s:%d] sid %d, len: %d %s%s` | `crates/minisatip/src/runtime.rs:625` |
| 1244 | LOG_AND_RETURN | `Most likely not an RTSP packet sock_id: %d sid: %d rlen: %d, dropping ....` | `crates/ms-net/src/rtsp.rs:304` (the parser returns `Malformed` instead of returning 0) |
| 1269 | LOGM | `Updating sid %d time to %jd, current time %jd` | `crates/ms-net/src/session.rs:1387` (`logl!(LOG_HTTP, ...)`; `touch_stream`) |
| 1497 | LOG | `Discarding %d bytes from the socket buffer, request > %d, consider increasing  RBUF` | `crates/ms-net/src/rtsp.rs:198` (shared with the RTSP branch) |
| 1503 | LOG | `HTTP header not complete: %d\n%s` | `crates/ms-net/src/rtsp.rs:204` |
| 1556 | LOG | `Read HTTP (handle %d) [%s:%d] sid %d, sock %d %s%s` | `crates/minisatip/src/runtime.rs:638` |
| 1680 | LOG | `Requested sid close %d timeout %d type %d, sock %d, handle %d, timeout %d` | `crates/minisatip/src/runtime.rs:1062` |
| 1716 | LOGM | `%s: bootid: %d deviceid: %d http: %s` (`__FUNCTION__` = `ssdp_notify`) | `crates/ms-net/src/ssdp.rs:612` |
| 1738 | LOGM | `%s packet %d:\n%s` | `crates/ms-net/src/ssdp.rs:622` |
| 1742 | LOG | `incomplete ssdp_discovery: wrote %d out of %d: error %d: %s` | `crates/ms-net/src/ssdp.rs:630` (helper `ssdp.rs:376-389`) |
| 1794 | LOGM | `Dropping packet from the same UUID as mine (from %s:%d)` | `crates/ms-net/src/ssdp.rs:489` |
| 1803 | LOGM | `Received SSDP packet (handle %d) from %s:%d %s%s` | `crates/ms-net/src/ssdp.rs:499` |
| 1814 | LOG | `A new device joined the network with the same Device ID:  %s, asking ...` | `crates/ms-net/src/ssdp.rs:517` |
| 1820 | LOG | `incomplete ssdp_reply notify: wrote %d out of %d: error %d: %s` | `crates/ms-net/src/ssdp.rs:522` |
| 1840 | LOG | `Device ID conflict, changing our device id to %d, destination SAT>IP server %s` | `crates/ms-net/src/ssdp.rs:551` |
| 1849 | LOG_AND_RETURN | `ssdp_reply: the message is a reply, ignoring....` | `crates/ms-net/src/ssdp.rs:582` |
| 1855 | LOGM | `Send Reply SSDP packet (fd: %d) %s:%d, bootid: %d deviceid: %d http: %s\n%s` | `crates/ms-net/src/ssdp.rs:592` |
| 1863 | LOG | `incomplete ssdp_reply: wrote %d out of %d: error %d: %s` | `crates/ms-net/src/ssdp.rs:604` (helper `ssdp.rs:376-389`) |
| 1913 | LOG0 | `init_utils failed with %d` | n/a: start-up, `init_utils` has no Rust counterpart |
| 1918 | LOG | `Running mode: %s` | already ported: `crates/minisatip/src/main.rs:77` |
| 1926 | LOG | `Command line: %s` | already ported: `crates/minisatip/src/main.rs:90` |
| 1930 | LOGL | ` LOG  of the module enabled: %s` | already ported: `crates/minisatip/src/main.rs:91-95` |
| 1931 | DEBUGL | `DEBUG of the module enabled: %s` | already ported: `crates/minisatip/src/main.rs:91-95` |
| 1935 | FAIL | `Failed to create cache directory %s` | n/a: start-up; the Rust error path returns `Err` from `runtime::run` (out of scope) |
| 1939 | FAIL | `RTSP: Could not listen on port %d` | n/a: start-up; returned as an `Err` from `runtime::run` (out of scope) |
| 1942 | FAIL | `Could not listen on http port %d` | n/a: start-up; returned as an `Err` from `runtime::run` (out of scope) |
| 1945 | FAIL | `SSDP: Could not bind on udp port 1900` | n/a: start-up; returned as an `Err` from `runtime::run` (out of scope) |
| 1947 | FAIL | `SSDP: Could not bind on %s udp port 1900` | n/a: start-up; returned as an `Err` from `runtime::run` (out of scope) |
| 1950 | LOG | `SSDP: Bound to device %s` | n/a: start-up (out of scope) |
| 1952 | LOG | `SSDP: Device %s unavailable, using IP binding` | n/a: start-up (out of scope) |
| 1964 | FAIL | `sockets_add failed for ssdp` | n/a: start-up; the Rust port has no sockets_add (out of scope) |
| 1969 | LOG | `SRT library initialized` | n/a: start-up (out of scope) |
| 1971 | LOG | `Warning: SRT listener failed to start (SRT transport will not work)` | n/a: start-up (out of scope) |
| 1979 | FAIL | `sockets_add failed for rtsp` | n/a: start-up; listen failure already covers this (out of scope) |
| 1982 | FAIL | `sockets_add failed for http` | n/a: start-up; listen failure already covers this (out of scope) |
| 1986 | FAIL | `sockets_add failed for signal thread` | n/a: start-up; no sockets_add (out of scope) |
| 1993 | FAIL | `sockets_add failed for BW calculation` | n/a: start-up; no sockets_add (out of scope) |
| 2003 | FAIL | `sockets_add failed for satip_xml retrieval` | n/a: start-up; no sockets_add (out of scope) |
| 2008 | LOG | `Added sockets id %d for polling the satip servers` | n/a: start-up (out of scope) |
| 2016 | LOG0 | `Initializing with %d devices` | n/a: start-up (out of scope) |
| 2025 | LOG0 | `Closing...` | n/a: start-up (out of scope) |
| 2032 | LOG0 | `Exit OK.` | n/a: start-up (out of scope) |
| 2047 | LOG | `Initial values bootid %d, device_id %d, UUID %s (reading new from %s)` | `crates/ms-net/src/ssdp.rs:654` |
| 2075 | LOG | `Running with bootid %d, device_id %d, UUID %s` | `crates/ms-net/src/ssdp.rs:681` |
| 2097 | LOG | `Reply hidden because another GET while streaming (handle %d) [%s:%d] iteration:%jd, sock %d` | `crates/minisatip/src/runtime.rs:703` |
| 2165 | LOG | `Reply %s(handle %d) [%s:%d] content_len:%d, sock %d %s%s` | `crates/minisatip/src/runtime.rs:732` |

Commented, not active: line 1566 (`LOG("args: %s -> %s -> %s", ...)`).
