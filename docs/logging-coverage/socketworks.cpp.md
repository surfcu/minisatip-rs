# `socketworks.cpp` logging coverage

Source: `~/upstream-repo/minisatip/src/socketworks.cpp` (1666 lines).
The file switches `DEFAULT_LOG` four times: `LOG_SOCKETWORKS` for lines
1-705, `LOG_SOCKET` for 706-1018, `LOG_SOCKETWORKS` for 1019-1242 and
`LOG_SOCKET` for 1243-end. Rust files that mix both sections (`socket.rs`,
`writeq.rs`) therefore use explicit `logl!`/`debugl!` with the mask of the
C++ line's own section (`LOG_SOCKET` for every `LOGM`/`DEBUGM` site in the
write/queue functions, lines 1243+); files that map to one section use `LOG`/
`LOGM`/`DEBUGM` equivalents (`ms_log::log!` for `LOG`, `logm!` for `LOGM`,
`debugm!` for `DEBUGM`).

Summary: **ported 58 / not applicable 42 / already ported elsewhere 1** (101
active log sites: 100 macro calls plus the direct `_log` call at line 748).

`n/a` means the Rust tree has no code point with the same responsibility:
the C++ helper or failure mode (AF_UNIX sockets, sendmmsg batching, socket
thread ids, fd ownership, fallible allocation, non-Linux branches, receive
buffer helper) does not exist in the assigned crates.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---|---|---|---|
| 77 | LOG | getaddrinfo failed: host %s, port %s, %s | `crates/minisatip/src/runtime.rs:1209` (`resolve_bind_address`, one per bind/port) |
| 153 | LOG | --bind-dev cannot be combined with --bind or --bind-http | `crates/ms-config/src/parse.rs:1610` |
| 158 | LOG | Cannot resolve --bind-dev %s | `crates/ms-net/src/bind.rs:140` |
| 185 | LOG | Device name %s too long | `crates/ms-net/src/bind.rs:192` |
| 191 | LOG | No permission to bind socket %d to %s, using IP binding | `crates/ms-net/src/sys.rs:482` |
| 195 | LOG | Failed to bind socket %d to device %s: %s | `crates/ms-net/src/sys.rs:490` |
| 201 | LOG | SO_BINDTODEVICE unsupported, using IP binding for %s | n/a: no non-Linux socket backend (`sys` is `cfg(target_os = "linux")` only, so the `#else` branch has no counterpart) |
| 234 | LOG | getlocalip: Cannot create socket: %s | `crates/minisatip/src/runtime.rs:1435` |
| 241 | LOG | getlocalip: Error '%s'' during connect | `crates/minisatip/src/runtime.rs:1446` (doubled quote kept) |
| 263 | LOG | udp_bind failed: socket(): %s | `crates/ms-net/src/sys.rs:25` |
| 280 | LOG | setting multicast for %s | `crates/ms-net/src/sys.rs:271` |
| 283 | LOG | membership error: %s | `crates/ms-net/src/sys.rs:285` |
| 290 | LOG | multicast if error: %s | `crates/ms-net/src/sys.rs:307` |
| 295 | LOG | udp_bind failed: setsockopt(SO_REUSEADDR): %s | `crates/ms-net/src/sys.rs:41` |
| 301 | LOG | setsockopt failed to set SO_REUSEPORT %s | `crates/ms-net/src/sys.rs:46` |
| 310 | LOG | udp_bind: failed: bind() on host ANY port %d: error %s | n/a: the Rust `udp_bind` binds the requested group address directly; the ANY-address multicast fallback does not exist |
| 315 | LOG | udp_bind: bound %s to ANY (multicast fallback) | n/a: same missing multicast ANY fallback |
| 317 | LOG | udp_bind: failed: bind() on host %s port %d: error %s | `crates/ms-net/src/sys.rs:56` |
| 326 | LOG | New UDP socket %d bound to %s:%d %s%s%s | `crates/ms-net/src/sys.rs:73` |
| 343 | LOG | udp_bind_connect: failed: bind(): %s | `crates/ms-net/src/transport.rs:121` |
| 347 | LOG | New UDP socket %d connected to %s:%d | `crates/ms-net/src/transport.rs:128` |
| 366 | LOG | setsockopt failed for socket %d | n/a: no connect-timeout `setsockopt` helper in Rust; blocking `connect` errors surface as `io::Error` at the call site |
| 370 | LOG | setsockopt failed for socket %d | n/a: no connect-timeout `setsockopt` helper in Rust |
| 381 | LOG | setsockopt failed for socket %d | n/a: no socket-timeout `setsockopt` helper in Rust |
| 385 | LOG | setsockopt failed for socket %d | n/a: no socket-timeout `setsockopt` helper in Rust |
| 403 | LOG | tcp_connect failed: socket() %s | `crates/ms-net/src/transport.rs:145` |
| 413 | LOG | tcp_connect: setsockopt(SO_REUSEADDR): %s | `crates/ms-net/src/transport.rs:154` |
| 422 | LOG | %s: could not set the socket %d in nonblocking mode %d err: %s | n/a: Rust does not wrap `fcntl(O_NONBLOCK)` in a fallible helper with a log point |
| 437 | LOG | %s: ignoring source %s (family %d) for family %d socket | `crates/ms-net/src/transport.rs:174` |
| 440 | LOG | %s: failed: bind() on address: %s: error %s | `crates/ms-net/src/transport.rs:165` |
| 449 | LOG | tcp_connect: failed: connect to %s:%d failed with errno %d: ... | `crates/ms-net/src/transport.rs:191` |
| 458 | LOG | New TCP socket %d connected to %s:%d | `crates/ms-net/src/transport.rs:201` |
| 476 | LOG | tcp_listen failed: socket(): %s | `crates/ms-net/src/sys.rs:96` |
| 485 | LOG | tcp_listen failed: setsockopt(SO_REUSEADDR): %s | `crates/ms-net/src/sys.rs:110` |
| 491 | LOG | tcp_listen: failed: bind() on address: %s [%s], port %d : ... | `crates/ms-net/src/sys.rs:122` |
| 499 | LOG | tcp_listen: listen() error: %s | `crates/ms-net/src/sys.rs:135` |
| 503 | LOG | New TCP listening socket %d at %s:%d, family %d | `crates/ms-net/src/sys.rs:146` |
| 514 | LOG | tcp_connect failed: socket() %s | n/a: no local-domain (`AF_UNIX`) socket helper in Rust |
| 525 | LOG | %s: could not set the socket %d in nonblocking mode %d err: %s | n/a: no local-domain socket helper / nonblocking wrapper |
| 532 | LOG | tcp_connect: failed: connect to %s failed: %s | n/a: no local-domain (`AF_UNIX`) socket helper in Rust |
| 538 | LOG | New LOCAL socket %d connected to %s | n/a: no local-domain (`AF_UNIX`) socket helper in Rust |
| 552 | LOG | sockets_accept: failed %d: %s | `crates/minisatip/src/runtime.rs:2095` (the Rust loop then stops) |
| 557 | LOG | ERROR on accept: could not add new socket %d to the list | `crates/minisatip/src/runtime.rs:2086` (connection-thread spawn failure) |
| 591 | LOG_AND_RETURN | sockets_add does not add negative sockets %d | n/a: `-1` is a valid "no OS handle" sentinel for `SocketTable::add`; there is no negative-handle rejection path |
| 595 | LOG_AND_RETURN | sockets_add timeout without timeout function | n/a: the Rust timeout hook is an `Option`, so "no timeout function" is not a failure |
| 599 | LOG_AND_RETURN | sockets_add failed for socks %d | `crates/ms-net/src/socket.rs:365` |
| 643 | LOG | sockets_add: handle %d (type %d) returning socket sock %d [%s... | `crates/ms-net/src/socket.rs:374` |
| 667 | LOG | sockets_del: sock %d -> handle %d, sid %d, overflow bytes %d, ... | `crates/ms-net/src/socket.rs:437` |
| 672 | LOGM | sockets_del: closing socket handle %d | n/a: `SocketTable` does not own the fd (`close_unix_socket` has no Rust counterpart) |
| 689 | LOG | sockets_del: sock %d Last open socket is at index %d current... | `crates/ms-net/src/socket.rs:447` |
| 699 | LOG | Closing slave socket index %d (master sock %d) | `crates/ms-net/src/socket.rs:464` |
| 748 | _log | Too many threads, increase MAX_THREAD_INFO from %d | n/a: Rust spawns a thread per connection and has no `MAX_THREAD_INFO` registry |
| 769 | LOG | Starting select_and_execute on thread ID %lx, thread_name %s | `crates/ms-net/src/dataplane.rs:435` (already ported; file owned by coordinator) |
| 793 | LOG | No enabled sockets for Thread ID %lx name %s ... exiting | n/a: there is no `select_and_execute` thread registry; each data-plane thread ends on its stop flag |
| 805 | LOG | select_and_execute: select() error %d: %s | n/a: no `select()` loop; connections use blocking reads with timeouts |
| 823 | DEBUGM | event on socket index %d handle %d type %d buffered %d (poll fd: %d, ... | n/a: no `select()` event dispatch |
| 829 | LOGM | start flush sock id %d, buffered %d | n/a: no `select()` write-ready pass |
| 834 | DEBUGM | Sock %d: No Read event, continuing | n/a: no `select()` event loop |
| 863 | DEBUGM | Socket buffer full, handle %d, sock_id %d (m: %d), type %d, lbuf %d, ... | n/a: the Rust reader grows/limits per request, no fixed-buffer-full select branch |
| 889 | LOGM | WARNING: read on socket id %d, handle %d, took %jd ms | n/a: no per-`select` read timing |
| 902 | LOG | socket %d, handle %d, master %d, errno %d, read_ok %d, clearing buffer ... | n/a: read errors close the connection; there is no select-level buffer-clear path |
| 916 | DEBUGM | Read %s %d (rlen:%d/total:%d) bytes from %d [s: %d m: %d] -> old pos ... | n/a: no select-level read trace |
| 948 | LOG | ignoring error on sock_id %d handle %d type %d error %d : %s | n/a: no select-level ignored-error branch |
| 955 | LOG | select_and_execute[%d]: %s on socket %d (sid:%d) from %s:%d - type %s ... | n/a: no select-level dispatch trace |
| 972 | LOG | Delete socket %d done: sid %d | n/a: no select-level delete trace |
| 1010 | LOG | The main loop ended, run_loop = %d | `crates/minisatip/src/runtime.rs:451` |
| 1146 | LOG | unable to set output socket buffer (force) size to %d | `crates/ms-net/src/sys.rs:183` |
| 1149 | LOG | unable to set output socket buffer size to %d | `crates/ms-net/src/sys.rs:196` |
| 1152 | LOG | output socket buffer size for socket %d is %d bytes | `crates/ms-net/src/sys.rs:201` |
| 1162 | LOG | unable to set receive socket buffer (force) size to %d | n/a: no receive-buffer helper in the assigned crates; the equivalent lives in unassigned `crates/ms-tuner/src/satipc/sys.rs` |
| 1165 | LOG | unable to set receive socket buffer size to %d | n/a: same missing receive-buffer helper |
| 1168 | LOG | receive socket buffer size is %d bytes | n/a: same missing receive-buffer helper |
| 1231 | LOG | set_socket_thread: thread %lx for sockets %i | n/a: Rust has no per-socket thread id (`sockets.tid`), so no setter and no log point |
| 1238 | LOG_AND_RETURN | get_socket_thread: socket is NULL for s_id %d | n/a: no per-socket thread registry in Rust |
| 1282 | LOG | sendmmsg(): errno %d: %s | n/a: Rust emits UDP with `send`/`send_to`; there is no sendmmsg batching helper |
| 1284 | LOG | %s: %d of out %d UDP messages sent | n/a: no sendmmsg batching helper |
| 1304 | DEBUGM | start writev handle %d, iiov %d, len %d | `crates/ms-net/src/writeq.rs:348` |
| 1327 | DEBUGM | writev handle %d, iiov %d, len %d, rv %d, errno %d | `crates/ms-net/src/writeq.rs:366` (also short-write branches at 390 and 404) |
| 1337 | LOG | Connection REFUSED on handle %d, socket %d (sid %d), closing ... | `crates/ms-net/src/writeq.rs:415` |
| 1347 | LOG | writev returned %d handle %d, iiov %d errno %d error %s | `crates/ms-net/src/writeq.rs:425` |
| 1352 | DEBUGM | writev returned %d handle %d, iiov %d, len %d (took %jd ms) | `crates/ms-net/src/writeq.rs:377` (also error branch at 434) |
| 1368 | LOG | sock %d: dropping %d bytes of high priority data, %d queued | `crates/ms-net/src/writeq.rs:107` |
| 1374 | LOG | sock %d: no memory to queue %d bytes | n/a: Rust `Vec` growth is infallible (aborts on OOM), so the allocation-failure branch does not exist |
| 1413 | LOG_AND_RETURN | Unable to allocate the FIFO with size %d | n/a: FIFO allocation is an infallible `Vec` allocation in Rust |
| 1440 | LOG | sock %d dropping %jd enqueued bytes (offset %d) | `crates/ms-net/src/writeq.rs:225` |
| 1491 | LOG | Insufficient bandwidth: sock %d: overflow %d bytes, total %.1f MB, ... | `crates/ms-net/src/writeq.rs:165` |
| 1501 | LOG | Unhandled sock %d data: copied %d offset %d len %d | `crates/ms-net/src/writeq.rs:181` |
| 1503 | LOGM | got %d bytes, offset %d, rv %d, buffered %d bytes - used %d | `crates/ms-net/src/writeq.rs:190` |
| 1539 | LOGM | SOCK %d: flushed %d (out of %d) bytes in %jd ms | `crates/ms-net/src/writeq.rs:256` |
| 1558 | LOG_AND_RETURN | sock %d flushed incomplete packet before priority response: left %d, ... | `crates/ms-net/src/writeq.rs:288` |
| 1566 | LOG | sock %d, flushed %d bytes of priority data from %d | `crates/ms-net/src/writeq.rs:306` |
| 1571 | LOG | sock %d incomplete priority data write %d left %d in %jd ms | `crates/ms-net/src/writeq.rs:317` |
| 1580 | LOGM | SOCK %d: set timeout_ms to 1 from %d | `crates/ms-net/src/socket.rs:271` |
| 1600 | LOGM | sid for socket id %d could not be set | n/a: the Rust `sid` is set at construction; there is no `sockets_set_sid` failure path |
| 1606 | LOG | sockets_set_handle: socket id %d not found | n/a: there is no `sockets_set_handle` in `SocketTable` |
| 1613 | LOG | sockets_set_handle: socket id %d, old handle %d -> new hand... | n/a: there is no `sockets_set_handle` in `SocketTable` |
| 1623 | LOG | %s: setsockopt IP_TOS failed | `crates/ms-net/src/sys.rs:225` |
| 1628 | LOG | %s: setsockopt SO_PRIORITY failed | `crates/ms-net/src/sys.rs:239` |
| 1630 | LOG | %s: setsockopt SO_PRIORITY not implemented | n/a: no non-Linux socket backend (`#else` branch) |
| 1658 | LOG | Unable to set master for sockets %d to %d as one of them is | n/a: `set_master(Option<SocketId>)` models "no master" as `None`; there is no invalid-id failure path |
| 1664 | LOG | sock %d is master for sock %d | `crates/ms-net/src/socket.rs:233` |

## Deviations

- **DEV-250**: C++ `%p` values are pointer values (`ss->read`, `master->buf`,
  `master->action`, `%s:%d` addresses). The ported sites normalise them to the
  stable literal `0x0` (`sockets_add ... read: 0x0`).
- **DEV-252**: `sockets_add`'s `[%s:%d]` host/port comes from `USockAddr`, which
  the Rust `SocketTable` does not carry. The ported line emits `[:0]`.
- **DEV-253**: C++ line 689 prints `max_sock.load()`, the highest allocated slot.
  Rust prints `SocketTable::sockets.len()`; the `Vec` never shrinks, so it
  tracks the same high-water mark.
- **DEV-254**: Files that mix both `DEFAULT_LOG` sections use explicit
  `logl!(LOG_SOCKET, ...)` / `debugl!(LOG_SOCKET, ...)` for the `LOGM`/`DEBUGM`
  sites that fall in the 1243+ `LOG_SOCKET` section (writeq.rs 1304/1327/1352/
  1503/1539, socket.rs 1580); `LOG` sites stay `log!` (LOG_GENERAL)
  regardless of the file's `DEFAULT_LOG`.
- **DEV-255**: `WriteQueue` carries the C++ `s->sock`, `s->id` and `s->sid` for
  its logs through `set_log_ids`, populated by `Socket::new`. Callers that use
  a bare `WriteQueue` outside `Socket` (e.g. `output.rs`) leave the defaults,
  so those fields print `-1` instead of the C++ values.
- **DEV-242**: C++ line 1327 prints the stale `_errno` after a short write. The
  Rust port prints `0` for that field (a stable value), because
  `write_vectored` does not expose the untouched errno.
- **DEV-256**: Line 552 prints `error.raw_os_error().unwrap_or(0)` for the `%d`
  errno field. Line 557's "new socket" number is the accepted fd, because the
  Rust code point is a failed connection-thread spawn rather than a
  `sockets_add` failure.
- **DEV-257**: Line 77 is emitted once per address resolution: `--bind` with the
  RTSP port, `--bind-http` with the HTTP port and the SSDP re-resolution with
  port 1900.

Not deviations:

- C++ line 1491 "Insufficient bandwidth" follows the C++ condition
  (`overflow == 0 || LOG_SOCKET enabled`) and the C++ `LOG` macro, so the port
  uses `log!` (LOG_GENERAL).
- `write_chunks` returns `Ok(None)` for `EWOULDBLOCK`, emits the C++ 1327 debug
  line and skips the C++ 1352 "returned" line, matching the C++ early return
  paths. The 1327/1352 rows therefore map to several Rust call sites (short
  write, error, would-block).
- Arguments that C++ fills with `strerror(errno)` use `ms_log::strerror`,
  which removes `io::Error` Display's `" (os error N)"` suffix, so the text
  matches C++.
