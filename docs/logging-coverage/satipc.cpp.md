# `src/satipc.cpp` (`#define DEFAULT_LOG LOG_SATIPC`)

Ported **86** / not applicable **9** (95 active log sites).

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 100 | LOG | `%s:%d: %s returns NULL for id %d` | n/a: no `get_satip` lookup; `Client` methods return early on `!enabled` |
| 137 | LOG | `Setting adapter %d to restart every time the transponder is cha…` | `crates/ms-tuner/src/satipc/client.rs:359` |
| 144 | LOG | `adapter %d is not RTSP over TCP, switching` | `crates/ms-tuner/src/satipc/client.rs:368` |
| 168 | LOG | `satipc %d: request failed fatally (rc %d)` | `crates/ms-tuner/src/satipc/client.rs:384` |
| 203 | LOG | `satipc: session set for adapter %d to %s with stream_id %d` | `crates/ms-tuner/src/satipc/client.rs:409` |
| 213 | LOG | `accepting packets from RTSP server` | `crates/ms-tuner/src/satipc/client.rs:421` |
| 238 | LOG | `satipc: RTSP header not complete: %d len %d\n%s` | `crates/ms-tuner/src/satipc/client.rs:442` (`s->id` unavailable, printed as 0) |
| 250 | LOG | `satipc_reply (adapter %d): sock %d (receiving from handle %d, state…` | `crates/ms-tuner/src/satipc/client.rs:457` |
| 254 | LOGM | `%s` | `crates/ms-tuner/src/satipc/client.rs:465` |
| 301 | LOG | `satipc %d: server reports %s` | `crates/ms-tuner/src/satipc/client.rs:510` |
| 324 | LOG | `satipc %d: request rejected (rc %d), retrying` | `crates/ms-tuner/src/satipc/client.rs:532` |
| 329 | LOG | `marking device %d as error, rc = %d` | `crates/ms-tuner/src/satipc/client.rs:538` |
| 332 | LOG | `marking device %d not as error but reply is unexpected, rc = %d` | `crates/ms-tuner/src/satipc/client.rs:541` |
| 336 | LOG | `satipc %d, expect_reply %d, want_tune %d want commit %d, state %d…` | `crates/ms-tuner/src/satipc/client.rs:550` |
| 366 | LOG | `satip_close called for adapter %d, socket_id %d, handle %d timeout…` | `crates/ms-tuner/src/satipc/driver.rs:527` |
| 396 | LOG | `per-server source %s overrides --bind-dev source %s` | `crates/ms-tuner/src/satipc/driver.rs:163` |
| 402 | LOG_AND_RETURN | `could not connect to %s:%d, error %s` | `crates/ms-tuner/src/satipc/driver.rs:172` |
| 427 | LOG | `satipc: connected to SAT>IP server %s port %d %s handle %d %s %s` | `crates/ms-tuner/src/satipc/driver.rs:199` |
| 438 | LOG | `adapter %d satip rtsp sock %d, handle %d, keep_open %d, restart_need…` | `crates/ms-tuner/src/satipc/client.rs:684` |
| 497 | LOG | `satipc %d: no response was received from the server for more than %…` | `crates/ms-tuner/src/satipc/client.rs:729` |
| 506 | LOG | `satipc %d no timeout will be performed as we have operations in que…` | `crates/ms-tuner/src/satipc/client.rs:740` |
| 539 | LOG | `SRT connection lost for adapter %d. Restarting SRT.` | `crates/ms-tuner/src/satipc/driver.rs:386` (SRT feature) |
| 573 | LOG | `satipc: Sent keep-alive to the satip server %s:%d, adapter %d, sock…` | `crates/ms-tuner/src/satipc/client.rs:750` |
| 606 | LOG | `satipc: Ignoring server signal status since both strength_multipli…` | `crates/ms-tuner/src/satipc/client.rs:599` |
| 613 | LOG | `satipc: Received signal status from the server for adapter %d, sten…` | `crates/ms-tuner/src/satipc/client.rs:607` |
| 642 | LOG | `satipc: rtp report, adapter %d: rtcp missing packets %d, rtp missing…` | `crates/ms-tuner/src/satipc/client.rs:630` |
| 655 | DEBUGM | `satipc: satipc_rtcp_reply SES1 match at %d` | `crates/ms-tuner/src/satipc/client.rs:648` |
| 697 | LOG | `SRT recv failed for adapter %d, socket %d: %s` | `crates/ms-tuner/src/satipc/driver.rs:764` (SRT feature) |
| 715 | LOG | `Reusing existing SRT socket %d for adapter %d` | `crates/ms-tuner/src/satipc/driver.rs:279` (SRT feature) |
| 723 | LOG | `Closing old SRT socket %d (not connected)` | `crates/ms-tuner/src/satipc/driver.rs:290` (SRT feature) |
| 731 | LOG | `Closing stale UDP socket %d for adapter %d` | `crates/ms-tuner/src/satipc/driver.rs:297` (SRT feature) |
| 746 | LOG_AND_RETURN | `SRT UDP bind failed on ephemeral port` | `crates/ms-tuner/src/satipc/driver.rs:308` (SRT feature) |
| 751 | LOG_AND_RETURN | `srt_create_socket failed: %s` | `crates/ms-tuner/src/satipc/driver.rs:313` (SRT feature) |
| 764 | LOG | `srt_bind_acquire failed: %s` | `crates/ms-tuner/src/satipc/driver.rs:322` (SRT feature) |
| 786 | LOG | `SRT connect to %s:%d failed: %s` | `crates/ms-tuner/src/satipc/driver.rs:339` (SRT feature) |
| 804 | LOG | `SRT connected for adapter %d to %s:%d, streamid=%s, srt_sock=%d` | `crates/ms-tuner/src/satipc/driver.rs:356` (SRT feature) |
| 818 | LOG_AND_RETURN | `Could not listen on port %d: err %d: %s` | `crates/ms-tuner/src/satipc/driver.rs:229` |
| 826 | LOG_AND_RETURN | `Could not listen on port %d: err %d: %s` | `crates/ms-tuner/src/satipc/driver.rs:240` |
| 835 | LOG | `Could not add RTCP socket %d` | n/a: sockets are stored directly; there is no `sockets_add` failure |
| 954 | LOG | `satip device %s:%d is closing` | `crates/ms-tuner/src/satipc/client.rs:799` |
| 979 | LOG | `satip device %s:%d going to standby (expect_reply %d)` | `crates/ms-tuner/src/satipc/client.rs:814` |
| 989 | LOG | `All stream closed, restarting satip adapter %d` | `crates/ms-tuner/src/satipc/client.rs:830` |
| 1048 | LOG | `satipc_read: Error allocating memory!` | n/a: fixed-size stack buffers; no allocation |
| 1054 | DEBUGM | `satipc_read: read error or zero datagrams` | `crates/ms-tuner/src/satipc/driver.rs:722` (only non-transient errors) |
| 1067 | DEBUGM | `satipc_read: read full, so flushing output` | `crates/ms-tuner/src/satipc/driver.rs:732` |
| 1103 | LOGM | `discarding RTP packet without valid TS payload (sock %d, socket_id %…` | `crates/ms-tuner/src/satipc/media.rs:43` |
| 1185 | LOG | `Not processing packet as the type is %02X (not 0 or 1)` | n/a: the demux only yields channel 0 or 1 by construction |
| 1201 | DEBUGM | `start satipc_tcp_read tcp_pos %d tcp_len %d tcp_size %d, buffer len…` | `crates/ms-tuner/src/satipc/driver.rs:554` |
| 1208 | LOG_AND_RETURN | `Cannot alloc memory for tcp_data with size %d` | n/a: `Vec` buffer; no allocation failure path |
| 1221 | DEBUGM | `Moved from the position %d, length %d` | `crates/ms-tuner/src/satipc/media.rs:117` |
| 1232 | DEBUGM | `read %d (from %d) from rtsp socket %d (id %d) [%02X %02X, %02X %0…` | `crates/ms-tuner/src/satipc/driver.rs:648` |
| 1238 | LOG | `read %d from RTSP sock %d, handle %d, errno %d, %s` | `crates/ms-tuner/src/satipc/driver.rs:633, 666` |
| 1249 | LOGM | `buffer is full, skipping read, %d, pos %d [%02X %02X, %02X %02X]` | `crates/ms-tuner/src/satipc/driver.rs:617` |
| 1261 | DEBUGM | `found at pos %d, rtsp_len %d, len %d` | `crates/ms-tuner/src/satipc/media.rs:154` |
| 1271 | LOG | `%s: skipped %d bytes` | `crates/ms-tuner/src/satipc/media.rs:161` |
| 1278 | DEBUGM | `The curent chunk is not completely read @ pos %d, tcp_pos %d, requir…` | `crates/ms-tuner/src/satipc/media.rs:164` |
| 1291 | DEBUGM | `Destination buffer is full @ pos %d, tcp_pos %d, required %d len %d` | n/a: `next_frame` has no destination buffer |
| 1299 | DEBUGM | `ad %d processed %d, socket %d` | `crates/ms-tuner/src/satipc/driver.rs:582` |
| 1309 | LOG | `%s: skipped %d bytes` | `crates/ms-tuner/src/satipc/media.rs:187` |
| 1317 | LOGM | `Unlikely, found newline after the end of string, tcp_pos %d` | n/a: `find_bytes` is bounded by the slice |
| 1334 | LOGM | `End of rtsp rtsp message not found in this block, pos %d, tcp_pos %…` | `crates/ms-tuner/src/satipc/media.rs:190` |
| 1340 | LOGM | `Found newline after the end of string, tcp_pos %d` | n/a: same as line 1317; `find_bytes` cannot pass the end |
| 1354 | LOGM | `sending %d bytes to satipc_reply, cl %d, pos %d, tcp_len %d, left %d` | `crates/ms-tuner/src/satipc/media.rs:213` |
| 1362 | DEBUGM | `ignoring byte %02X` | `crates/ms-tuner/src/satipc/media.rs:224` |
| 1370 | DEBUGM | `%s: returning %d bytes` | `crates/ms-tuner/src/satipc/driver.rs:599` |
| 1410 | LOG | `satipc: set_pid for adapter %d, pid %d, err %d (lap=%d)` | `crates/ms-tuner/src/satipc/client.rs:854` |
| 1420 | LOGM | `satipc: set_pid for pid %d already in the delete list! (ldp=%d)` | `crates/ms-tuner/src/satipc/client.rs:868` |
| 1435 | LOG | `satipc: del_pid for aid %d, pid %d, err %d (ldp=%d)` | `crates/ms-tuner/src/satipc/client.rs:888` |
| 1445 | LOGM | `satipc: del_pid for pid %d already in the add list! (lap=%d)` | `crates/ms-tuner/src/satipc/client.rs:901` |
| 1614 | LOG | `%s: not sending method %s: url %s` | `crates/ms-tuner/src/satipc/client.rs:1156` |
| 1677 | LOG | `satipc_http_request (adapter %d): sock %d: [[ %s %s ]]` | `crates/ms-tuner/src/satipc/client.rs:1221` |
| 1679 | LOGM | `%s` | `crates/ms-tuner/src/satipc/client.rs:1228` |
| 1686 | LOG | `%s: remote socket is -1` | `crates/ms-tuner/src/satipc/client.rs:1235` |
| 1706 | LOG | `No system specified %d` | `crates/ms-tuner/src/satipc/client.rs:1143` |
| 1793 | LOG_AND_RETURN | `adapter %d: Nothing to commit` | `crates/ms-tuner/src/satipc/client.rs:1256` |
| 1814 | LOG | `Unable to send TEARDOWN for adapter %d (stream_id %d)` | `crates/ms-tuner/src/satipc/client.rs:1278` |
| 1845 | LOG | `satipc: request for adapter %d, state %d, freq %d, pids to add %d,…` | `crates/ms-tuner/src/satipc/client.rs:1308` |
| 1900 | LOGM | `satipc: for adapter %d, executing state %d` | `crates/ms-tuner/src/satipc/client.rs:1355` |
| 1927 | LOG_AND_RETURN | `No satip client found for adapter %d` | `crates/ms-tuner/src/satipc/client.rs:1374` |
| 1929 | LOG | `satipc: commit on freq %d, sys %d for adapter %d (expect reply %d)` | `crates/ms-tuner/src/satipc/client.rs:1377` |
| 1942 | LOG | `satipc: tuning to freq %d, sys %d for adapter %d (state %d, expect…` | `crates/ms-tuner/src/satipc/client.rs:1394` |
| 1955 | LOG | `Restart needed for satip adapter %d` | `crates/ms-tuner/src/satipc/client.rs:1411` |
| 2010 | LOG_AND_RETURN | `MAX_ADAPTERS reached` | `crates/minisatip/src/runtime.rs:321` (start-up break) and `:460` (`log_and_return!(Ok(()))`) |
| 2059 | LOG | `AD%d: Satip device %s port %d delsys %d: %s %s, fe %d, total number…` | `crates/ms-tuner/src/satipc/driver.rs:116` |
| 2094 | FAIL | `SRT support is disabled, install libsrt-openssl-dev` | `crates/ms-tuner/src/satipc/config.rs:72` (`log0!` then `Err`) |
| 2126 | LOG | `Found only the system for satip adapter %.*s` | `crates/ms-tuner/src/satipc/config.rs:101` |
| 2223 | LOG | `No satipcap found for %s, XML:\n%s` | `crates/ms-tuner/src/satipc/config.rs:220` (URL unavailable, prints host) |
| 2239 | LOG | `Could not determine the delivery system for %.*s` | `crates/ms-tuner/src/satipc/config.rs:232` |
| 2250 | LOG | `Could not determine the delivery system for %.*s (%d) tuners %d, o…` | `crates/ms-tuner/src/satipc/config.rs:243` |
| 2258 | LOGM | `%s: %d tuners for %s` | `crates/ms-tuner/src/satipc/config.rs:255` |
| 2265 | LOG | `updating tuners for %s, host: %s, satip port %d` | `crates/minisatip/src/runtime.rs:576` |
| 2279 | LOG | `No change for satip xml: %s` | `crates/minisatip/src/runtime.rs:572` |
| 2284 | LOG | `too much data from the satip server for URL %s` | `crates/ms-tuner/src/satipc/config.rs:325` |
| 2289 | DEBUGM | `%s: %s` | `crates/ms-tuner/src/satipc/config.rs:331` |
| 2311 | LOG | `could not allocate %d bytes for satip pointers` | n/a: no `init_satip_pointer` allocation in the port |
