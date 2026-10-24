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
empty `-l`. The heartbeat section is the only event at the moment; later events
use the same envelope with a different `event` value and their own fields.

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

Flight-recorder tail of a crashed process, in the local `-F` file or journal:

```
flight recorder: 20000 lines
...
flight recorder end
```
