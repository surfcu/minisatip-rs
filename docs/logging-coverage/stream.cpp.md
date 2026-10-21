# `stream.cpp` logging coverage

Ported 49 / not applicable 21 (70 active `LOG`/`LOGM`/`LOG0`/`LOG_AND_RETURN`/
`DEBUGM`/`_dump_packets` sites in upstream `src/stream.cpp`). Commented-out
upstream log lines are listed after the table; they are not part of the count.

`DEFAULT_LOG` is `LOG_STREAM` for this file, `LOG_DMX` for
`check_new_transponder`, `check_cc`, `check_cc2`, `process_packets_for_stream`
and `process_dmx`, then `LOG_STREAM` again from `calculate_bw` onwards.
`session.rs`/`output.rs` define `DEFAULT_LOG`; `dataplane.rs` spells the mask out
(`logl!(LOG_DMX, ...)` / `debugl!`) so the switch points stay visible.

Rust values without a C++ counterpart print `-1`: the socket-table ids
(`s->id`, `rsock_id`, `rtcp_sock`) do not exist, and the TCP sink is type-erased
so its fd is unknown. `%p` has no stable Rust counterpart; that site is not
ported.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 76 | LOG | `%s:%d get_sid returns NULL for s_id = %d` | n/a: `get_sid1` helper is not ported; lookups return `None` without logging |
| 131 | LOG_AND_RETURN | `DESCRIBE BUFFER is full` | n/a: DESCRIBE builds a growing `String`; there is no fixed-size overflow path |
| 175 | LOG | `Setup stream sid %d parameters, sock_id %d, handle %d` | session.rs:512 |
| 181 | LOG_AND_RETURN | `Could not add a new stream` | session.rs:523 |
| 193 | LOG_AND_RETURN | `sockets_add failed for stream timeout sid %d` | n/a: the `st_sock` timeout socket is not ported |
| 199 | LOG | `Setup stream done: sid %d for sock %d handle %d` | session.rs:527 |
| 203 | LOG_AND_RETURN | `stream sid %d not enabled for sock_id %d handle %d` | session.rs:538 |
| 250 | LOG_AND_RETURN | `No Transport header was specified, for stream sid %d` | session.rs:615 |
| 254 | LOG | `Play for stream sid %d, type %d, rsock %d, adapter %d, sock_` | session.rs:630 |
| 286 | LOG_AND_RETURN | `Tune requested with no real parameters, ignoring ...` | session.rs:664 |
| 290 | LOG | `slave stream tuning to a new frequency, finding a new adapter` | session.rs:671 |
| 295 | LOG | `Got adapter %d on sid %d socket %d` | session.rs:681 |
| 344 | LOG | `%s: start close_stream_for_socket for id %d %p` | n/a: `close_stream_for_socket` is not ported; forced close uses the `timeout_ms` flag |
| 352 | LOG | `closing stream %d` | session.rs:751 |
| 376 | LOG | `Closing RTP sock %d handle %d` | output.rs:349 (from dataplane.rs:808,1147) |
| 385 | LOG | `Closing RTCP sock %d handle %d` | output.rs:350 (from dataplane.rs:808,1147) |
| 408 | LOG | `closed stream %d` | session.rs:776 |
| 416 | LOG_AND_RETURN | `SRT listener not initialized` | session.rs:849 |
| 429 | LOG_AND_RETURN | `SRT transport missing srt_streamid= parameter` | session.rs:856 |
| 436 | LOG_AND_RETURN | `Failed finding pending SRT socket for sid %d, srt_streamid %s` | session.rs:864 |
| 441 | LOG | `SRT stream setup for sid %d, srt_streamid='%s', srt_sock=%d ` | session.rs:875 |
| 446 | LOG0 | `SRT not supported, install libsrt-openssl-dev` | session.rs:842 |
| 457 | LOG | `Error: No stream to set transport to, sock_id %d, arg %.*s ` | session.rs:801 |
| 466 | LOG | `Assuming SRT transport for stream sid %d, arg %.*s` | session.rs:832 |
| 471 | LOG | `Assuming RTSP over TCP for stream sid %d, arg %.*s` | session.rs:814 |
| 511 | LOG | `decode_transport ->type %d, ttl %d new socket to: %s:%d` | session.rs:927 |
| 519 | LOG | `Transport for this connection is already setup to %s:%d, lea` | session.rs:941 |
| 525 | LOG | `Stream has already a transport header associated with it to ` | session.rs:952 |
| 540 | LOG_AND_RETURN | `stream %d closed while changing transport` | n/a: transport replacement is synchronous; there is no unlock/recheck window |
| 560 | LOG_AND_RETURN | `decode_transport failed: UDP connection on rtp port to %s:%d ` | output.rs:176 |
| 568 | LOG_AND_RETURN | `RTP sockets_add failed` | n/a: the socket table / `sockets_add` is not ported |
| 577 | LOG_AND_RETURN | `decode_transport failed: UDP connection on rtcp port to %s:%d` | output.rs:190 |
| 589 | LOG_AND_RETURN | `RTCP sockets_add failed` | n/a: the socket table / `sockets_add` is not ported |
| 598 | LOG | `Detected stream with the same destination as sid %d: sid %d ` | session.rs:983 |
| 614 | LOG_AND_RETURN | `streams_add failed` | session.rs:476 |
| 718 | LOG_AND_RETURN | `Stream sid is null for s_id %d` | n/a: `send_rtcp` only runs for an existing `StreamOutput` |
| 785 | DEBUGM | `%s: sent %d bytes for sid %d, handle %d seq %d => %s:%d` | output.rs:641 |
| 817 | LOG | `SRT send failed for sid %d: %s` | output.rs:518 |
| 838 | LOG | `Start streaming for stream sid %d, len %d to handle %d => %s` | output.rs:537 |
| 845 | DEBUGM | `%s: sent %d bytes for sid %d, handle %d, sock_id %d, seq %d ` | output.rs:547 |
| 877 | LOG | `Got the new transponder %04X %d, position %d, %jd ms after t` | dataplane.rs:124 |
| 883 | LOGM | `Got old transponder id %04X %d, position %d, %jd ms after tu` | dataplane.rs:134 |
| 912 | `_dump_packets` | `check_cc -> ` | dataplane.rs:270 |
| 917 | LOGM | `%s: pid %03d not found` | n/a: missing-PID accounting is not ported (DEV-112); only active PIDs are delivered |
| 937 | LOG | `PID Continuity error (adapter %d, pos %d): pid: %03d, Expe` | dataplane.rs:232 |
| 955 | hexdump | `packet` | n/a: inside `#ifdef CRC_TS`, which is not defined in this build |
| 956 | LOG | `pid %d cnt %d count %d cc %d err %d crc %08X ` | n/a: inside `#ifdef CRC_TS`, which is not defined in this build |
| 966 | LOG | `Reseting count from %d, CRC_TS %d` | n/a: inside `#ifdef CRC_TS`, which is not defined in this build |
| 996 | `_dump_packets` | `check_cc2 -> ` | n/a: `check_cc2` is not ported; Rust has a single continuity pass |
| 1013 | LOGM | `PID Continuity error post-processing (adapter %d, pos %d): p` | n/a: `check_cc2` is not ported; Rust has a single continuity pass |
| 1047 | LOG_AND_RETURN | `Stream %d rsock id not found %d` | n/a: outputs are keyed by stream id; there is no `rsock_id` lookup |
| 1054 | LOG_AND_RETURN | `rsock %d disabled for stream %d` | n/a: outputs are keyed by stream id; there is no socket enable flag |
| 1061 | LOG_AND_RETURN | `stream disabled %d for addapter %d` | n/a: the output map only holds live streams; no check/log point |
| 1096 | LOG | `Non TS packet found %02X, pos %d` | rtp.rs:191 |
| 1114 | LOG | `stream sid %d, flushing intermediary stream iiov %d max_iiov` | rtp.rs:207 |
| 1144 | LOGM | `Processing done sid %d at pos %d, rtp header %d, rtp_pos %d,` | output.rs:458 |
| 1174 | LOGM | `process_dmx start called for adapter %d -> %d out of %d byte` | dataplane.rs:947 |
| 1249 | LOGM | `Flushing adapter %d buffer of %d bytes after the tune %jd ms` | dataplane.rs:982 |
| 1269 | LOGM | `read_dmx send=%d, force_send=%d, cnt %d called for adapter %` | dataplane.rs:1008 |
| 1310 | LOG | `BW %jd KB/s, DMX %jd KB/s, Buffered %.1f MB, Dropped: %.1f M` | n/a: `calculate_bw` lives in `crates/ms-net/src/bandwidth.rs` (outside the assigned files) and has no ported log |
| 1347 | LOG | `Stream forced close requested for sid %d` | session.rs:407 |
| 1357 | LOG | `no data sent for more than 1s sid: %d for %s:%d` | output.rs:580 |
| 1367 | LOG | `Stream timeout sid %d, closing (ctime %jd , sid->rtime %jd, ` | session.rs:418 |
| 1384 | LOG | `Dumping streams:` | n/a: `dump_streams` is not ported |
| 1387 | LOG | `%d|  a:%d rsock:%d type:%d play:%d remote:%s:%d` | n/a: `dump_streams` is not ported |
| 1414 | LOG | `Forcing session id %d on stream sid %d` | session.rs:1374 |
| 1429 | LOG | `fix master_sid to %d for adapter %d` | n/a: `fix_master_sid` is implemented in `ms-sched` (outside the assigned files) |
| 1439 | LOG | `recovered session id from a closed connection, sid %d , id: ` | session.rs:1337 |
| 1455 | LOG | `%s: called for stream sid %d from %s:%d` | dataplane.rs:1079 (`None` fallback dataplane.rs:1086) |
| 1459 | LOGM | `Acknowledging stream sid %d via rtcp packet` | dataplane.rs:1089 |

Commented-out upstream lines (not ported, not counted): 244-248 (assumed
RTSP/TCP setup), 731 (`send_rtcp`), 933-935 (PAT TID), 958 (`count`),
1197-1198 (pid not found), 1213 (buffer rounding).
