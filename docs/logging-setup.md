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
| `cam_state` | `ca`, `state`, `mode`, `suppressed` | State `init`/`ready`/`reset`/`removed` (`mmi_recovery` with `--ci-plus-recovery`; `cam_recovery` with `--cam-recovery`, a CAPMT re-send, not a reset); mode `ci`/`ci+`; `suppressed` only when > 0 | A CAM slot initialises, reports CA_INFO, is reset, or its module is removed. Only changes are sent: a repeat is dropped, and a slot reset straight after `init` (an empty or silent slot, reset by the 15 s command watchdog) is reported once, then its init/reset loop stays muted until another state or a mode change; that next event carries `suppressed`, the number of muted events. The text log still has every reset (`Requesting CA N close`). |
| `cw` | `pmt`, `parity`, `ecm_ms`, `source` | PMT id, even/odd parity, milliseconds from ECM section to key (null when unknown), `dvbapi`/`ci+` | A control word is installed. |
| `ddci_batch` | `adapter`, `grown_to` | Bytes | Flushed: CAM output grew the adapter batch; the largest value in the window is kept. |
| `ddci_fifo_overflow` | `adapter`, `dropped` | DDCI device id (the device map key), bytes | Flushed: bytes lost when a DDCI FIFO overflowed. |
| `ddci_capture` | `slot`, `files`, `bytes`, `drops` | CA slot, the two file names, bytes written, bytes the writer missed | A `--ddci-capture` pair and its sidecar are written (feature `diag-log`). |
| `cam_menu` | `ca`, `kind`, `items`, `lines`, `text_hash`, `answer`, `solicited` | Kind `menu`/`list`/`enquiry`/`menu_more`/`list_more`; menu items (0 for an enquiry or fragment); text lines (title, subtitle and bottom included); FNV-1a hash of the lines as 16 hex digits; what the host answered: `1`/`0`/`open` (unsolicited, per `--cam-menu-unsolicited`), `request` (left to a `--cam-menu` request), `snapshot` (a `--cam-menu-snapshot` walk, or a page closed during one), `pin`/`cancel`/`close` (enquiry), `none`; `solicited` inside a request | Every MMI page from the CAM (feature `diag-log`). The page text itself is in the `[UI n]` log lines; a PIN is never logged. |
| `ca_tables` | `ddci`, `ecm_pkts`, `emm_pkts`, `ecm_sections`, `emm_sections`, `parity_dropped`, `period_s` | DDCI device, ECM and EMM packets written to the CAM, packets that start a section, parity changes of this device not reported (over 1024 in a window for all devices), window seconds | Every 10 s per DDCI device that received ECM or EMM packets (`--cam-ecm-trace`). |
| `ecm_parity` | `ddci`, `adapter`, `pid`, `table_id`, `at_ms` | DDCI device, source adapter and ECM PID, the new table_id (`128`/`129`), UTC ms when the first packet with it was written | An ECM PID switched between table_id 0x80 and 0x81, i.e. a crypto-period change (`--cam-ecm-trace`); reported with the next 10 s window. |
| `ci_decrypt` | `adapter`, `keys_installed`, `errors`, `packets`, `period_s` | Active CW services, descrambler error delta, packets in the window (scrambled, plus clear with pid > 0x1F; null packets and pids=all not counted), window seconds | Every 10 s for a CI adapter when keys or errors were seen. |
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
| `recovery` | `--cam-recovery` re-sends an active slot as a forced update with a new version (it takes precedence over `retry`). On a DDCI CAM the silent per-service path re-joins only that service's CAPMT with the same reason, once per join. |

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

## DDCI capture

`--ddci-capture dir` (diag-log builds only) records the transport stream that
a DDCI CI device exchanges with its CAM, so a CAM that stops descrambling
after service changes can be analysed afterwards.

- **What is recorded:** per DDCI device, the bytes written to the CI device
  after PID remapping (`in`), and the bytes read back from the CAM (`out`).
  Nothing else is stored: no MMI text, keys or CW/CI+ data.
- **Trigger:** a CAM MMI menu or enquiry on the device's slot. The slot number
  is the DDCI adapter index, as in `ca<slot>` below.
- **Output:** on a trigger, two files are written to `dir`:
  `ca<slot>-<UTC YYYYMMDDTHHMMSS>-in.ts` and `-out.ts`, holding the last 60 s
  before the trigger plus the next 60 s, and a `ca<slot>-<time>.json` sidecar.
  The sidecar holds `format` (`ddci-capture/2`), the slot, the trigger time and
  kind, `window_ms` (60000), the `files` names, a time index per file with one
  entry `[monotonic ms, UTC ms, byte offset]` for each recorded chunk (each CI
  write or CAM read, so its spacing follows the traffic), the PID remap pairs,
  `short_writes` (CI writes the device accepted only in part), `bytes`,
  `dropped_bytes` and `truncated` per direction.
- **APDU trace:** `apdus_before` lists the slot's last 512 APDUs before the
  trigger and `apdus_after` those up to the end of the capture, each as
  `[seq, monotonic ms, UTC ms, "cam"|"host", session, "tag", length]`. Only
  headers are kept, never an APDU body. A host APDU is listed when it is
  handed to the CI device, even if the write then fails. `apdus_after_truncated` is set when
  more than 512 APDUs followed the trigger and the oldest of them are missing.
- **Limits:** at most one capture per slot per 10 minutes. A trigger is
  refused, with a log line, when `dir` already holds `--ddci-capture-max-mb`
  (default 2048) MB of `ca*.ts`/`ca*.json` files; one accepted capture can
  overshoot the cap by its own size.
- **Memory:** each device and direction keeps a ring of at most 160 MiB and
  60 s. A 70 Mbit/s CI fills the byte bound after about 19 s, so the oldest
  data is dropped first. A capture shares the ring's chunks instead of copying
  them; chunks queued for the writer stay alive after the ring drops them, up
  to 320 MiB in flight for all devices together. With the option on, the
  bound is 320 MiB per CI device (160 MiB in each direction) plus the shared
  320 MiB writer queue.
- **Disk:** a capture holds at most 320 MiB per direction (the pre-window is at
  most 160 MiB and the post-window is capped at 160 MiB; a capture that hit the
  cap has `truncated` set), so a pair takes at most 640 MiB. At 8 Mbit/s a full
  120 s pair is about 240 MB.
- **Drops:** the writer thread gets chunks through a bounded queue. When it
  cannot keep up, chunks are dropped and counted in the sidecar as
  `dropped_bytes`. A dropped chunk breaks 188-byte packet alignment in the
  `.ts` file, so analysis tools must resync on `0x47`.
- **Timing:** the hot path records a monotonic timestamp only. Wall time in
  the index is the trigger's UTC time plus the monotonic offset.
- **Fetching:** copy `dir` off the host, for example with `scp` or `rsync`.
  Then run the capture analysis script (kept with the homelab logging notes,
  not in this repository) for continuity errors, input gaps, bitrates, ECM
  intervals and the scrambled share of the output.

Example:

```sh
minisatip --ddci-capture /var/lib/minisatip/ddci --ddci-capture-max-mb 1024
```

## ECM trace

`--cam-ecm-trace` (diag-log builds only) counts the ECM and EMM packets that
each DDCI CAM receives and reports every ECM crypto-period change, so CAM
errors can be lined up with the key changes without a capture.

- ECM PIDs come from the programs' CA descriptors, EMM PIDs from the CAT.
- Packets are counted when they are written to the CI device, before the
  `writev`; a short write still counts them.
- Only the `table_id` of a section that starts in a packet is read (0x80 even,
  0x81 odd); nothing else of an ECM or EMM is looked at or logged.
- The first `table_id` seen on a PID is a baseline; each later switch is one
  `ecm_parity` event. A PID that stops being an ECM PID loses its baseline.
- Packets with the scrambling bits set are counted, but their table_id is not
  read.

## CAM menu

`--cam-menu` (diag-log builds only) lets an operator open the CAM's own menu,
for example its card status pages while a CAM is failing, and walk it over
HTTP. Without the option the routes are 404.

- `POST /debug/cam/<ca>/menu` sends `enter_menu` on the slot's Application
  Information session and opens a request.
- `POST /debug/cam/<ca>/menu/answer?choice=N` answers the waiting page: a menu
  item 1..N or 0 (back); a list takes only 0.
- `POST /debug/cam/<ca>/menu/close` closes the MMI and ends the request.

Replies: `202` queued, `400` bad choice, `404` unknown CA or path, `405` GET or
HEAD, `409` no session or no open request, `503` no CA worker. The
check uses the status snapshot, which can be one worker loop old, so a `202`
can still fail in the worker; the log then has `menu command ... not sent`.
The pages are not in the reply: read them in the log (`[UI n]` lines) or as
`cam_menu` events.

While a request is open, menu and list pages wait for an answer instead of
getting C++'s automatic choice 1 and close. An enquiry is answered with the
`-3` PIN when its length fits, else cancelled; a PIN never travels in a URL.
A requested page triggers no `--ddci-capture` and does not arm
`--ci-plus-recovery`. After 120 s without a page or command the host answers
0 (when a page waits) and closes. A close from the CAM, or the MMI session
closing, ends the request at once.

### CAM status snapshot

`--cam-menu-snapshot` (diag-log builds only) reads the CAM's own status pages
automatically when the CAM opens a menu, list or enquiry by itself (an error
message), so the card state at that moment ends up in the log next to the episode.

- The value is a list of paths: `HASH:CHOICE` steps joined by `/` (up to 3), paths
  joined by `,` (up to 8). HASH is the 16-digit `text_hash` of the page the choice
  is made on, from its `cam_menu` event; CHOICE is the item number (1-9).
- Get the hashes once by opening the menu by hand with `--cam-menu` and reading the
  `text_hash` of each page. Only list steps that lead to read-only status pages:
  never a choice that changes a setting, resets the CAM or asks for a PIN.
- 5 s after the CAM's own page, each path is walked: the main menu is opened, a step
  is answered only when the page's hash is exactly the step's HASH and the choice is
  on it, and the page reached after the last step is logged and closed. Any other
  page (another hash, a list, the error message again) ends the path and closes the
  MMI. A page that does not come within 10 s ends the path; the next path starts 4 s
  later. Any enquiry that arrives while a path waits for a page is cancelled, never answered with the PIN, even one the CAM raises for a service (it asks again on the next tune).
- At most one snapshot per slot per 10 minutes, counted from its trigger; it does
  not start while a `--cam-menu` request or an `open` held page is in progress, and is
  dropped if it cannot start within 5 minutes. A `--cam-menu` command cancels it.
- The walk's pages count as requested: no `--ddci-capture`, no recovery arming, no
  automatic answer. After the first path has opened, a menu the CAM opens by itself is
  closed with no choice (it still arms recovery and may trigger a capture); an
  enquiry of its own keeps the PIN answer.

Example (placeholder hashes):

```sh
minisatip --cam-menu --cam-menu-snapshot 1111111111111111:1/2222222222222222:1,1111111111111111:1/2222222222222222:4
```

`--cam-menu-unsolicited answer1|answer0|open` sets the reply to a menu or list
the CAM opens by itself: `answer1` (default) is the C++ choice 1 and close,
`answer0` sends 0 (back) and close, as enigma2 does, and `open` leaves the
page open for up to 120 s, then answers 0 and closes. Enquiries keep the PIN
behaviour in every mode.

Example:

```sh
minisatip --cam-menu --log-remote tcp://logs:6514
curl -X POST http://host:8080/debug/cam/8/menu
curl -X POST 'http://host:8080/debug/cam/8/menu/answer?choice=2'
curl -X POST http://host:8080/debug/cam/8/menu/close
```

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
