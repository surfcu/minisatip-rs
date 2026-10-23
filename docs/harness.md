# Differential harness (`ms-harness`)

`ms-harness` starts two minisatip binaries with identical generated TS captures and
identical scenario input, then diffs the normalised RTSP, HTTP and RTP/TS output.
The C++ build is the oracle; a scenario passes when the two transcripts are equal
and every repeated run produces the same transcript as run 1.

## Building the C++ reference

```sh
scripts/build-cpp-reference.sh          # prints the binary path last
```

The script clones `catalinii/minisatip` (or reuses `target/upstream/minisatip`),
applies `harness/cpp-replay.patch` and builds with the replay adapter enabled. The
patch is the only C++ change: it adds a file-backed tuner and is off unless
`REPLAY=ON`. Like the Rust replay adapters, the C++ ones advertise DVB-S2/DVB-S,
carry no frontend name and replace the local DVB adapters while a replay is set.

The default build matches the Rust default build: DVBCA and DDCI on (always
compiled in Rust), DVBCSA, SRT and NETCVCLIENT off; `--full` adds DVBCSA.

Both binaries take the capture from the same environment variables:

| Variable | Meaning |
|---|---|
| `MINISATIP_REPLAY` | TS file to loop |
| `MINISATIP_REPLAY_ADAPTERS` | Adapter count (the harness passes the scenario's `adapters`) |
| `MINISATIP_REPLAY_BITRATE` | Fixed replay rate in bit/s (see DEV-120) |

## Commands

```sh
# Rust against itself, ten deterministic runs (CI subset)
cargo run -p ms-harness -- run --fast --runs 10

# C++ oracle against Rust
cargo run -p ms-harness -- run --cpp target/upstream/minisatip/build-harness/minisatip

# Two arbitrary binaries
cargo run -p ms-harness -- run --a BIN --b BIN --filter http

# Inspect one side, record or compare goldens, generate a capture
cargo run -p ms-harness -- show --bin ./target/debug/minisatip harness/scenarios/01-fta-single.yaml
cargo run -p ms-harness -- run --a CPP --b CPP --record harness/golden
cargo run -p ms-harness -- run --golden harness/golden
cargo run -p ms-harness -- gen multi /tmp/multi.ts
cargo run -p ms-harness -- list
```

Options: `--runs N`, `--fast`, `--filter TEXT`, `--work DIR`, `-v`. `--cpp` puts the
C++ binary in slot A, so `--record` writes the oracle transcript. `--golden DIR`
checks every run of both sides against `DIR/<scenario>.txt`; `--record` writes only
the first run of A.

The fast subset is all twelve scenarios in `harness/scenarios/`; CI runs it ten
times Rust-vs-Rust. The `cpp-reference` CI job builds the reference with the
script above, uploads it, and runs every scenario three times C++-vs-Rust and
against `harness/golden` (recorded from C++). If upstream no longer builds or
takes the patch, that job warns and skips the comparison.

## Scenario format

A scenario is one YAML file with `name`, `description`, `capture` (generator
profile: `fta`, `multi` or `pmtver`), `adapters` (default 1), `bitrate` (default
2 000 000), optional extra `args`, `fast`, and a list of `steps`.

```yaml
name: fta-single
capture: fta
steps:
  - rtsp: { conn: c, method: OPTIONS, url: "{base}/" }
  - udp: u1
  - rtsp:
      conn: c
      method: SETUP
      url: "{base}/?src=1&freq=11494&pol=h&ro=0.35&msys=dvbs2&mtype=8psk&plts=off&sr=22000&fec=23&pids=0,17,256"
      headers: { Transport: "RTP/AVP;unicast;client_port={rtp:u1}-{rtcp:u1}" }
  - rtsp: { conn: c, method: PLAY, url: "{base}/stream={stream:c}", session: true }
  - capture: { from: u1 }
  - rtsp: { conn: c, method: TEARDOWN, url: "{base}/stream={stream:c}", session: true }
  - silence: { from: u1, ms: 500 }
```

Template placeholders: `{base}` (`rtsp://127.0.0.1:<rtsp>`), `{host}`, `{rtsp}`,
`{http}`, `{rtp:NAME}`, `{rtcp:NAME}`, `{session:CONN}` and `{stream:CONN}`. The
session and stream ids stay available after a `close` step.

| Step | Effect |
|---|---|
| `rtsp` | One request on a persistent `conn`; records the reply. `session: true` adds the last `Session` header, `raw` replaces the whole request text, `no_cseq` omits CSeq |
| `http` | One request on a fresh connection; `body` selects `full` (default), `numbers`, `length` or `none` |
| `http_stream` | Opens a GET whose TS body is captured under `name` |
| `udp` | Binds an RTP/RTCP client port pair under `name` |
| `capture` | Summarises one replay period from a UDP pair, an RTSP connection (interleaved) or an HTTP stream; skips `settle_ms` (300) and waits up to `timeout_ms` (5000) for a complete period. `mode: sets` (the default for `file:` captures) instead records one full loop of the file |
| `silence` | Records whether the source stays quiet for `ms` |
| `close` | Closes a connection or stream |
| `sleep` | Waits `ms` |

## Capture summaries

`analysis.rs` extracts TS packets from RTP payloads (checking version, payload
type, SSRC stability, sequence gaps and payload alignment) and reduces them to:

- one replay period, anchored on the lowest ES index (`MSRS` packets written by the
  generator) or, for PID-filtered captures, on the PSI section with the longest
  repeat;
- total packet count, sync errors, continuity-counter errors and an order hash;
- per-PID packet and scrambled counts;
- PAT/SDT/PMT sections with table id, extension, version, length, CRC and repeat
  count.

The generator writes `MSRS` plus a big-endian ES index into every ES packet, so
counts do not depend on when the capture started. Profiles: `fta` (one service),
`multi` (three services, null/stuffing PIDs) and `pmtver` (PMT version change).

## Recorded captures

`capture: file:<name>` replays `<DIR>/<name>.ts` from `--captures DIR`. Recordings
from the box stay outside git; without `--captures` those scenarios are reported as
`SKIP`, and a run where every scenario is skipped fails.

```
cargo run -p ms-harness -- run --runs 3 --cpp $CPP --captures ~/captures harness/scenarios-real
```

| Scenario | File | Checks |
|----------|------|--------|
| `real-fta-single` | `fta-dmax` | DMAX HD over RTSP/TCP, `/state.json` |
| `real-fta-multi` | `fta-multi` | 19-service mux over HTTP `pids=all`, two services over RTSP/TCP |
| `real-scrambled` | `scr-bein3` | Irdeto service with ECM/EMM pids, no CA: TSC kept |

A recorded file has no ES index, so `sets` summaries hold only what does not depend
on timing: PIDs with their scrambling state (`clear`, `scrambled`, `mixed`), distinct
PSI sections without repeat counts, and RTP gaps, sync and CC errors as `none`/`some`.
The capture runs for the file length at the scenario `bitrate` plus one second. CC
transitions the file makes itself (its loop seam and recorded discontinuities) are not
errors, and a CC break drops a partly assembled section. Set `bitrate` to the file's
PCR rate.

## Normalisation

Both transcripts run through the same rules before the diff:

- `Date`, `Last-Modified` and `Expires` become `<DATE>`;
- the RTSP and HTTP listener ports and every RTP port become `<RTSP>`, `<HTTP>`,
  `<SRTP+n>` or `<u1.rtp>`/`<u1.rtcp>`;
- each distinct `Session:` id becomes `<SESSIONn>`; `ssrc`, `seq` and `rtptime`
  values become `<SSRC>`, `<SEQ>` and `<RTPTIME>`;
- the version after `minisatip/`, inside `<modelNumber>` and in JSON `"version"`
  becomes `<VER>`;
- SDP `o=-` ids become `<SDPID> <SDPVER>` and UUIDs become `<UUID>`;
- per-binary scratch paths become `<CACHE>` and `<WORKDIR>`.

Text bodies are normalised too. Because that changes their byte length, the echoed
`Content-Length` header is recomputed from the normalised body, so C++ and Rust
headers can be compared directly. Bodies with `body: numbers` use `<N>` for every
digit run; `body: length` keeps only the length and an FNV-1a hash.

## Known difference

`http-state` fails C++-vs-Rust only on `/state.json`: Rust emits the M3 symbol
subset and different absent values, which is DEV-051 and arrives with later
milestones. Rust-vs-Rust and C++-vs-C++ are green for all twelve scenarios.
