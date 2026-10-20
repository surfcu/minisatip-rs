# Replay mechanism decision (M0)

Status: decided 2026-10-21. Scope: how the differential harness (M1) feeds
deterministic MPEG-TS to the C++ reference binary and the Rust binary without a
tuner.

## What was inspected

Upstream sources were fetched from `catalinii/minisatip` master over
`raw.githubusercontent.com` on 2026-10-21. A clone or GitHub API listing was not
available from this session, so the test list below comes from the root
`CMakeLists.txt`.

| Path | Result | Finding |
|---|---|---|
| `tools/replay.py` | exists, 186 lines | Python 2 RTSP transcript replayer, not a TS source. |
| `tests/Makefile` | **does not exist (HTTP 404)** | Tests are built by root `CMakeLists.txt` (`BUILD_TESTING`). |
| `tests/test_play.cpp` | **does not exist (HTTP 404)** | There is no play/replay test binary upstream. |
| `CMakeLists.txt` | exists | Lists 17 tests: adapter, opts, ca, dvbapi, pmt, ddci, dvb, minisatip, utils, satipc, satipc_rtsp, socketworks, utils_dir, stream, hw_descrambler, srt, dvb_charset. |
| `src/opts.cpp` | exists, 75 lines | Contains only `parse_dvbapi_opt` and `parse_sdt_charset_opt`; no `-k` or file-input option. |
| `src/minisatip.cpp` | exists | Owns `long_options`, `short_opts` and the `getopt_long_only` switch. |

## What the existing mechanisms actually do

### `tools/replay.py`

The script takes a minisatip log and replays the logged RTSP client exchanges
against a live server (`replay.py MINISATIP_IP LOG_FILE`). It parses
`read RTSP`, `select_and_execute` and `reply ->` records with regexes, reconnects
TCP to port 554, resends OPTIONS/SETUP/PLAY/TEARDOWN, and opens UDP/RTCP sockets
to count missing RTP packets and inspect TS continuity. It needs a running
minisatip with working adapters: it replays the *client*, not the *transport
stream*. It is also Python 2, timing-dependent, and coupled to log formatting.

### `-k` / file-related options

`-k` is the short form of `--no-all-pids` (`NO_PIDS_ALL_OPT`). It sets
`opts.emulate_pids_all = 1`, i.e. it emulates `pids=all` on hardware whose PID
filtering cannot express it. It has nothing to do with files or replay. The help
text ("Emulate pids=all when the hardware does not support it") matches this.

No upstream option takes a TS file as an adapter input. `--playlist` inserts an
`X_SATIPM3U` URL into the SAT>IP description, `-R` sets the web document root,
and `-F` sets the log file. None of them is a replay source.

Consequently, PLAN.md §5's reference to "the C++ `-k` output" as a way to record
captures is incorrect for current upstream master; captures should be made with
`dvbsnoop`, an external RTP/TS dump, or the replay backend proposed below.

### Unit tests

The C++ tests link the object library and call internal functions with in-memory
buffers. They are useful for porting module tests, but they do not drive either
full binary, so they cannot serve as the differential harness's input source.

## Decision

**Add a minimal file-replay tuner backend to the C++ fork (`surfcu/minisatip`),
and implement the matching backend in `ms-tuner`.**

The C++ backend should:

- expose one explicit option that maps a TS file to a virtual adapter;
- read MPEG-TS in 188-byte packets at a configurable rate (real-time by default,
  as fast as possible for tests), optionally looping or stopping at EOF;
- use the existing adapter/tuner path so RTSP, PAT/PMT rewrite and streaming
  code under test stays unchanged;
- need no kernel DVB devices and no special privileges, so it runs in CI.

`ms-tuner` will implement the same semantics behind the planned `Tuner` trait.
The harness can then run both binaries with identical files, timing and RTSP
scenarios.

`tools/replay.py` stays useful as a complement: once both binaries accept file
input, it can replay a captured client transcript against each of them to compare
RTSP responses and RTP streams. It is not sufficient on its own.

## Reasons

1. `tools/replay.py` has no way to supply deterministic TS without a tuner, and no
   equivalent of `tests/test_play.cpp` exists upstream.
2. The harness exit criterion requires hardware-free, repeatable runs in CI; a
   file source is the smallest component that provides this for both binaries.
3. Keeping the backend on the normal adapter path exercises the real RTSP/PSI/
   streaming code rather than a test-only shortcut.
4. The change is small, isolated and can live behind a build option in the fork,
   so the C++ oracle remains upstream-compatible for every other path.

## Open questions

- Exact CLI spelling and rate-control semantics must be fixed together with the
  harness scenario format in M1.
- The fork's replay backend must be kept out of the Rust binary's compatibility
  surface; it is a harness facility, not a new upstream feature.
