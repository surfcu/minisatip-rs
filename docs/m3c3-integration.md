# M3c3 integration: RTP outputs and the adapter data plane

M3c3 adds the data path in `ms-net`; the session layer (M3c2, `ms-sched` + the
`StreamBackend` implementation) drives it. Nothing here touches `handler.rs`.

## Modules

| Module | C++ origin | Content |
|---|---|---|
| `rtp` | `enqueue_rtp_header`, `process_packets_for_stream` | `RtpState` (seq, SSRC), `rtp_timestamp`, `packetize` → `Batch` per `flush_stream` call |
| `rtcp` | `send_rtcp` | `build_rtcp`: SR + SDES(`minisatip`) + APP `SES1`, optional `$\x01len` prefix |
| `describe` | `describe_adapter`, `get_stream_pids` | `describe_adapter(&DescribeInput)` → `ver=...;tuner=...;pids=...` |
| `output` | `flush_stream`, `send_rtcp`, `stream_timeout`, UDP part of `decode_transport` | `StreamOutput` (UDP, RTSP-TCP interleaved, HTTP), each with its own `WriteQueue` |
| `dataplane` | `read_dmx`, `process_dmx`, `adapter_update_threshold` | `AdapterHandle::spawn`: one thread per adapter, commands in, events out |

## Session-layer calls

```rust
// Once per adapter, when the scheduler creates it.
let adapter = AdapterHandle::spawn(Box::new(tuner), DataPlaneConfig {
    adapter_id: aid,
    adapter_buffer: opts.adapter_buffer as usize,
    threshold_ms: clamp_threshold(opts.udp_threshold),
})?;
adapter.tune(params.clone());                       // tune(); also feeds RTCP text

// PLAY with RTP/AVP (UDP), replacing decode_transport's socket setup.
let target = UdpTarget {
    local: SocketAddr::new(bind_ip, rtp_server_port(opts.start_rtp, sid)),
    dest: SocketAddr::new(client_ip, client_port),
    ttl: None,                                      // C++ parses ttl= but never applies it
};
let cfg = OutputConfig { ssrc: session_id as u32, seq: 0, fifo_size: opts.max_sbuf as usize << 20 };
adapter.start_output(sid, StreamOutput::udp(target, cfg, 0)?, pids);

// PLAY with RTP/AVP/TCP: hand over a non-blocking clone of the RTSP socket.
let sink = rtsp_stream.try_clone()?;               // set_nonblocking(true) first
adapter.start_output(sid, StreamOutput::rtsp_tcp(Box::new(sink), Some(peer), cfg, 0), pids);
// While interleaved, RTSP replies must go through the same queue:
adapter.send(Command::WritePriority { stream_id: sid, data: reply_bytes });

// HTTP: send the 200 response header first, then hand the socket over.
adapter.start_output(sid, StreamOutput::http(Box::new(sink), Some(peer), cfg, 0), pids);

adapter.set_pids(sid, new_pids);                    // addpids/delpids/pids=
adapter.send(Command::SetDestination { stream_id: sid, dest }); // client_port change
adapter.send(Command::SetThreshold(adapter_threshold(
    opts.udp_threshold, opts.tcp_threshold, sid_cnt, master_type, any_live_udp)));
adapter.query(sid)  // -> OutputStats { seq, ssrc, packets, octets, overflow, buffered, remote }
adapter.stop_output(sid);                           // TEARDOWN / close_stream
adapter.stop();                                     // adapter close
```

`OutputStats::seq` is the value for `RTP-Info: seq=`; `remote` answers
`get_stream_rhost`/`get_stream_rport`; `overflow`/`buffered` back the status API.

## Events (`adapter.events()`)

- `RtcpReceived(sid)` → set `sid->rtime` (`rtcp_confirm`): keeps the session alive.
- `OutputClosed(sid)` → a write failed (client gone): close the stream like C++.
- `OutputStopped(sid, stats)` → reply to `stop_output`.
- `TunerError(text)` → log; C++ handles tune errors in `tune()`.
- `ContinuityErrors(total)` → `SessionManager::set_adapter_ccerr`, exposed as `ad_ccerr`.

## Left for the integration step

- Session timeout (`sid->timeout`, `stream_timeout`'s close path) lives in the session
  layer (`expired_streams` on an injectable `Clock`); the data plane only does
  keepalive RTP (1 s idle) and RTCP (≥ 200 ms). The runtime calls `expired_streams`
  each control-loop tick and closes the returned streams, releasing adapters.
- `adapter_update_threshold` needs global stream knowledge: call `adapter_threshold`
  from the scheduler whenever streams are added/removed and send `SetThreshold`.
- The tuner PID filter is the union of the outputs' sets; once `ms-sched` owns
  `update_pids`, decide whether the scheduler or the data plane calls `set_pids`.
- Implemented in M3: `check_new_transponder`/`wait_new_stream` (200 ms PAT search
  after a tune, buffer truncated at the new PAT), `check_cc` (aggregate continuity
  errors; see DEV-111/112) and SRT output (chunked `srt_send` on an accepted socket,
  behind the `srt` feature).
- Still for later milestones: `wait_pusi`, PMT processing and CA decoding (DEV-113).
- `start_play` sets `flush_enqued_data` on the RTSP socket; for TCP streams that
  socket's queue now lives in the data plane, so forward it as a command if needed.
- `describe_adapter` is shared with DESCRIBE SDP; `TuneParams.mtype`/`fec` should
  become optional to remove DEV-040.
- DEV-042: confirm on the wire how the C++ binary frames UDP RTP (one packet per
  datagram is assumed).
- `StreamOutput::udp` binds without `SO_REUSEADDR` and does not set DSCP
  (`IPTOS_DSCP_EF`) or `--output-buffer` (`SO_SNDBUF`); add via `sys.rs` if needed.
