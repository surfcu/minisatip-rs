# Diagnostics logging setup

Status: implemented behind the `diag-log` build feature. Everything here is
off by default: without the feature the new options do not exist and the normal
outputs are unchanged.

Build with diagnostics enabled:

```sh
cargo build -p minisatip --features diag-log
```

## Options

| Option | Default | Meaning |
|---|---|---|
| `--log-remote tcp://host:port` | off | Send log lines and structured events to a syslog receiver over TCP. `tcp://` may be omitted. |
| `--log-remote-queue lines` | `10000` | Bounded queue between the logging threads and the sink. Values below 1 are raised to 1. |
| `--log-ring lines` | `20000` | Keep the newest lines in memory for the panic dump and `/debug/log`. `0` disables the recorder. |

The local outputs (`-l`, `-g`, `-F`) keep their existing format and masks.
The remote sink follows the local masks: a line reaches it only when it is also
written to the local outputs. It also follows local repeat suppression exactly:
a suppressed line is not sent, and a `Message repeated N times` line is sent
like any other line.

Diagnostics also add:

- A flight recorder: the newest lines are kept in a bounded in-memory ring.
  Per-packet and per-section debug sites are marked "hot" and never enter the
  ring, so a packet flood does not evict useful context.
- `GET /debug/log?lines=N` on the normal HTTP listener. The response is
  `text/plain`, oldest line first; `N` defaults to 1000 and is capped at the
  ring size. The route exists only in a `diag-log` build with the ring enabled.
- On a panic, the ring is written synchronously to stdout (the `-F` file in
  background mode) between `flight recorder: N lines` and `flight recorder
  end`, never through the `-g` syslog route. Each ring line carries its own
  `[timestamp thread]` prefix. The panic line and at most the newest 200 ring
  lines are also queued for the remote sink, even when the ring is off.
- A heartbeat event every 60 seconds.

Lines that can contain key material (control words, CA descriptors, key slots)
are always local-only: they bypass the ring and the remote sink.

## Wire format

The sink uses RFC 5424 over TCP with RFC 6587 octet-counted framing. Each frame
is the message's length in bytes as ASCII decimal, one space, then the message:

```
<LEN> <PRI>1 TIMESTAMP HOSTNAME minisatip PID - [ms@32473 module="..." thread="..." kind="..."] MESSAGE
```

| Field | Value |
|---|---|
| `PRI` | Facility `daemon` (3). Severity `notice` (5) for normal lines, `info` (6) for events, `crit` (2) for panic lines. |
| `TIMESTAMP` | RFC 3339 with milliseconds. The binary uses local time and a numeric UTC offset, e.g. `2026-10-24T10:20:30.400+02:00`. `-` (RFC 5424 NILVALUE) when the clock is unavailable. |
| `HOSTNAME` | `gethostname()`; `-` (RFC 5424 NILVALUE) when it fails. |
| `APP-NAME` | `minisatip`. |
| `PROCID` | Process id. |
| `MSGID` | `-`. |
| `STRUCTURED-DATA` | `[ms@32473 module="..." thread="..." kind="log|event"]`; `"`, `\` and `]` inside values are backslash-escaped. |
| `MESSAGE` | For `kind="log"`: `[THREAD]: TEXT`. For `kind="event"`: the JSON event text, without the thread prefix. |

One sender thread owns the connection, so producers never block. When the
queue is full, lines are dropped and counted. After the queue drains, the sink
sends one `dropped N lines` line (`kind="log"`, module `general`, thread
`logsink`). Reconnects back off 1, 2, 4, ... up to 30 seconds; connect and
write timeouts are 5 seconds. Each connect/disconnect writes one local line.

## Heartbeat event

Every 60 seconds the binary emits:

```json
{"kind":"event","event":"heartbeat","ts":"2026-10-24T10:20:30.400+02:00","thread":"heartbeat","uptime_s":3600,"rss_kb":18432,"threads":12,"sockets":8,"streams":2,"sink_queue":0,"sink_dropped":0}
```

| Field | Meaning |
|---|---|
| `uptime_s` | Seconds since diagnostics started. |
| `rss_kb` | Resident set size from `/proc/self/status` (`VmRSS`). |
| `threads` | Thread count from `/proc/self/status` (`Threads`). |
| `sockets` | Open sockets, counted from `/proc/self/fd` links starting with `socket:`. |
| `streams` | Active streams, the same count as the status page. |
| `sink_queue` | Lines currently queued for the remote sink. |
| `sink_dropped` | Lines dropped by the remote sink since it started. |

Event strings are capped at 256 bytes and string lists at 16 entries. Events
bypass `-l` masks and repeat suppression, so they are emitted even with an
empty `-l`. Every event uses the same envelope:

```json
{"kind":"event","event":"<name>","ts":"<RFC 3339>","thread":"<thread>",...}
```

## Event reference

Units are raw DVB/tuner values unless the table says otherwise. "Flushed"
events are accumulated by the hot path and emitted by the 10 s flush thread,
one event per distinct key.

| Event | Fields | Units / values | Emitted when |
|---|---|---|---|
| `tune` | `adapter`, `freq`, `pol`, `sr`, `msys`, `result` | Frequency in kHz, polarization number, symbol rate in Hz, delivery-system number, `ok`/`error` | The scheduler applies a tune to an adapter. |
| `lock` | `adapter`, `locked`, `status` | `status` is the known frontend status bits: `0x10` when locked, `0` otherwise | The first status poll after a tune, then whenever the frontend lock state changes. |
| `signal` | `adapter`, `strength`, `snr`, `ber` | Strength/SNR 0..255, raw BER | Strength or SNR moved by at least 5% of the 0..255 range, or BER changed; at most once per adapter per 10 s. |
| `cc_errors` | `adapter`, `pid`, `count`, `period_s` | PID, error count, window seconds | Flushed: continuity errors counted on the data path. |
| `send_drops` | `client`, `session`, `kind`, `count`, `period_s` | `host:port`, session id, `rtp`/`tcp`/`http`, dropped bytes (the count is in bytes), window seconds | Flushed: output queue or socket send drops. |
| `session_open` | `session`, `proto`, `adapter`, `client` | Session id, `rtsp`/`http`, adapter, `host:port` | Playback starts for a session (once per session). |
| `session_close` | `session`, `proto`, `adapter`, `client` | Same as `session_open` | A live session is closed or disabled. |
| `capmt` | `ca`, `pmt_ids`, `hash`, `version`, `reason`, `skipped` | CA id, PMT ids (max 16), content hash, CAPMT version, reason, skipped flag | Every CAPMT send attempt, including one skipped because the content hash did not change. |
| `cam_state` | `ca`, `state`, `mode`, `suppressed` | State `init`/`ready`/`reset`/`removed` (`mmi_recovery` with `--ci-plus-recovery`); mode `ci`/`ci+`; `suppressed` only when > 0 | A CAM slot initialises, reports CA_INFO, is reset, or its module is removed. Only changes are sent: a repeat is dropped, and a slot reset straight after `init` (an empty or silent slot, reset by the 15 s command watchdog) is reported once, then its init/reset loop stays muted until another state or a mode change; that next event carries `suppressed`, the number of muted events. The text log still has every reset (`Requesting CA N close`). |
| `cw` | `pmt`, `parity`, `ecm_ms`, `source` | PMT id, even/odd parity, milliseconds from ECM section to key (null when unknown), `dvbapi`/`ci+` | A control word is installed. |
| `ddci_batch` | `adapter`, `grown_to` | Bytes | Flushed: CAM output grew the adapter batch; the largest value in the window is kept. |
| `ddci_fifo_overflow` | `adapter`, `dropped` | DDCI device id (the device map key), bytes | Flushed: bytes lost when a DDCI FIFO overflowed. |
| `ci_decrypt` | `adapter`, `keys_installed`, `errors`, `period_s` | Active CW services, descrambler error delta, window seconds | Every 10 s for a CI adapter when keys or errors were seen. |
| `ciplus_sac` | `ca`, `state` | `open`/`dh_ok`/`auth_ok`/`auth_failed`/`sac_ok`/`sac_failed` | CI+ SAC/DH handshake progress (feature `ci-plus`). |
| `ciplus_key` | `ca`, `parity`, `target_adapters` | CA id, parity, adapter ids (max 16) | A CI+ key is installed for the offered PMTs (feature `ci-plus`). |
| `ciplus_uri` | `ca`, `length` | CA id, URI message length in bytes | A CI+ URI message is confirmed; the bytes are never parsed or logged (feature `ci-plus`). |
| `heartbeat` | see above | | Every 60 s. |

No event contains control words, key or IV bytes. `ciplus_uri` carries only the
length because the EN 50221 layer (like the C++ implementation it ports) hashes
the element 25 message without parsing its fields; there are no non-secret URI
fields to report.

### `capmt` reasons

| `reason` | Meaning |
|---|---|
| `join` | First CAPMT for a program; it always forces a send. |
| `update` | The PMT changed, or a list-management update re-sends an existing slot. |
| `rejoin` | A program rejoins a CAPMT slot it was already part of. |
| `leave` | The last program leaves the slot; sent as not-selected. |
| `retry` | The previous send failed with no session or a write error and is being retried. |

`skipped` is `true` when the built CAPMT hashes to the content already sent, so
nothing was written. `version` is the CAPMT version after the attempt; a skipped
update keeps the current version because only a real send bumps it.

## Collecting with Grafana Alloy

The receiver must understand RFC 5424 and octet counting. A generic pipeline:

```alloy
loki.relabel "syslog" {
  rule {
    source_labels = ["__syslog_message_sd_ms_32473_module"]
    target_label  = "module"
  }
  rule {
    source_labels = ["__syslog_message_sd_ms_32473_kind"]
    target_label  = "kind"
  }
  rule {
    source_labels = ["__syslog_severity"]
    target_label  = "level"
  }
  forward_to = []
}

loki.source.syslog "minisatip" {
  listener {
    // Bind to the interface and port that the receiver should listen on.
    address       = "0.0.0.0:5514"
    protocol      = "tcp"
    syslog_format = "rfc5424"
    framing       = "octet-counting"
    relabel_rules = loki.relabel.syslog.rules
  }
  labels     = { job = "minisatip" }
  forward_to = [loki.write.default.receiver]
}
```

Point `--log-remote` at the same address, for example
`--log-remote tcp://receiver.example:5514`. Keep the collector reachable only
from the hosts that run minisatip; the sink does not authenticate or encrypt.

## Queries

Heartbeat gaps (a stalled process stops emitting through the same path):

```logql
sum by (host) (count_over_time({job="minisatip", kind="event"} | json | event="heartbeat" [10m]))
```

```logql
absent_over_time({job="minisatip", kind="event"} | json | event="heartbeat" [3m])
```

Remote lines dropped because the receiver could not keep up:

```logql
{job="minisatip"} |= "dropped " |= " lines"
```

Heartbeat-reported drops:

```logql
{job="minisatip", kind="event"} | json | event="heartbeat" | sink_dropped > 0
```

Continuity errors per PID per hour:

```logql
sum by (adapter, pid) (
  sum_over_time(
    {job="minisatip", kind="event"} | json | event="cc_errors" | unwrap count [1h]
  )
)
```

RTP send drops per client per hour (`kind` is extracted as `kind_extracted`
because the stream already carries a `kind` label):

```logql
sum by (client) (
  sum_over_time(
    {job="minisatip", kind="event"}
      | json
      | event="send_drops" | kind_extracted="rtp"
      | unwrap count [1h]
  )
)
```

CAPMT sends per hour by reason (add `| skipped="false"` to count real writes
only):

```logql
sum by (reason) (
  count_over_time({job="minisatip", kind="event"} | json | event="capmt" [1h])
)
```

CAM reset episodes per host per hour (a muted init/reset loop counts once; every single reset is a
`Requesting CA N close` line in the text log):

```logql
sum by (host) (
  count_over_time(
    {job="minisatip", kind="event"} | json | event="cam_state" | state="reset" [1h]
  )
)
```

Keys installed more than 500 ms after their ECM section:

```logql
{job="minisatip", kind="event"} | json | event="cw" | ecm_ms > 500
```

Flight-recorder tail of a crashed process, in the local `-F` file or journal:

```
flight recorder: 20000 lines
...
flight recorder end
```
