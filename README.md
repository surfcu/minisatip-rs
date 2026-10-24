# minisatip-rs

A Rust rewrite of [minisatip](https://github.com/catalinii/minisatip), aiming to be
a drop-in replacement: the same RTSP/HTTP streaming, SAT>IP behaviour, CLI flags,
web UI and hardware support, with CA, adapter allocation and PSI processing each
owned by one component.

The architecture, milestones and conventions are in [PLAN.md](PLAN.md).

## Status

**Pre-release, M9 (parity hardening).** All modules of the C++ reference are
ported. The binary runs as a tvheadend SAT>IP backend on real hardware, but it
has not yet passed the 7-day soak test, so it is not a recommended replacement yet.

- Upstream baseline: `catalinii/minisatip` `28dfd78`.
- The C++ test suites are ported, except `test_utils.cpp`.
- The differential harness matches the C++ binary on 12 RTSP/HTTP/RTP scenarios.
- Intentional differences from C++ are listed in [docs/deviations.md](docs/deviations.md).

### Hardware validation

| Feature | State |
|---|---|
| Linux DVB (8 DVB-S2 tuners) | verified: FTA through tvheadend |
| DDCI (DuoFlex CI, Irdeto CAM, `-c 8:**2`) | verified: 3 channels descrambled without glitches, including rejoin |
| DVBAPI / oscam | fake oscam only |
| Multi-CAM | fake CAM only |
| SAT>IP client, netceiver, unicable/JESS, hardware descrambler | unit tests only |
| Enigma2 paths | deferred |

Details are in [docs/hardware-matrix.md](docs/hardware-matrix.md).

### Known limitations

- CI+ content control (DEV-151) is experimental and only built with the `ci-plus`
  cargo feature; it has not been validated on a real CAM. A default build has no
  CI+ support and forces plain CI on every slot, so CI+ CAMs work in CI-compatible
  mode and CI+-protected channels will not descramble.
- The CA harness scenarios are still missing. CAPMT sequences are compared with
  C++ only by the ported unit tests and by the real-CAM check.
- The runtime drives `ms_ca::CaDevice` directly; the `Reconciler` and its policies
  are covered by tests only, so there are no alternative CA policies yet.
- SAT>IP client adapters report `ad_type` 1 on the status page, which scales
  their SNR as DVB (DEV-053).
- Netceiver is parked (PLAN §3): the `ms-tuner` backend exists but the binary does
  not use it, and `-n` is an unknown option, as in a C++ build without NETCVCLIENT.

## Layout

Cargo workspace (Rust 2024) with these members:

| Crate | Role |
|---|---|
| `ms-ts` | TS packet/section handling, CRC, PID router |
| `ms-psi` | PAT/PMT/CAT/SDT parsing and generated charset tables |
| `ms-tuner` | `Tuner` trait and backends: Linux DVB, SAT>IP client, netceiver, file replay |
| `ms-sched` | adapter allocation, PMT/PID state, DiSEqC/unicable constraints |
| `ms-ca` | CAPMT building, CA device slots, reconciler and policies |
| `ms-ca-drivers` | EN50221, DDCI, DVBAPI, CSA/AES and hardware descramblers |
| `ms-net` | RTSP, HTTP, RTP, SSDP, the adapter data plane and the web/status pages |
| `ms-srt` | SRT bindings |
| `ms-log` | logging compatible with the C++ `-l`/`-v` modules |
| `ms-config` | CLI compatibility and options |
| `ms-harness` | differential test runner |
| `ms-fakecam` | EN50221/DDCI CAM simulator |
| `minisatip` | binary: wiring, CA runtime, signals |

Fuzz targets for the PSI parsers and the section assembler are in `fuzz/`.

## Build and test

```sh
cargo build --workspace
cargo test --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all --check
```

Release build and version check:

```sh
cargo build --release -p minisatip
./target/release/minisatip --version
```

Cargo features of the `minisatip` crate:

| Feature | Default | Effect |
|---|---|---|
| `linux-dvb` | yes | Linux DVB tuner backend and `/dev/dvb` enumeration |
| `dvbcsa` | no | software DVB-CSA descrambling; links `libdvbcsa` |
| `dvbcsa-icam` | no | iCAM CSA (implies `dvbcsa`); needs a `libdvbcsa` with `dvbcsa_bs_key_set_ecm` |
| `srt` | no | SRT output; links `libsrt` |
| `ci-plus` | no | CI+ content control (DEV-151); needs OpenSSL (`libssl-dev`) |
| `ci-plus-vendored` | no | `ci-plus` with a statically built OpenSSL (musl builds) |

```sh
cargo build --release -p minisatip --features dvbcsa,srt
```

The release binary is about 1.5 MB stripped (x86_64).

Cross builds use [`cross`](https://github.com/cross-rs/cross); see
`.github/workflows/ci.yml` for the musl target matrix (x86_64, armv7, aarch64 and
mipsel). `scripts/check-cross.sh` type-checks the same targets locally. The C++
reference build is also produced there (`scripts/build-cpp-reference.sh`).

The differential harness runs both binaries over the YAML scenarios in
`harness/scenarios/` and diffs normalised RTSP/HTTP/RTP output. See
[docs/harness.md](docs/harness.md) for the scenario format, normalisation rules
and commands; `cargo run -p ms-harness -- run --fast --runs 10` is the CI subset.

## Running the binary

The binary starts an RTSP listener (`-y`, default 554), an HTTP listener (`-x`,
default 8080) and, unless `--disable-ssdp` is given, the SSDP discovery socket.
Each scheduler adapter has one data-plane thread. `SIGINT`/`SIGTERM` send the
SSDP byebye set and close the listeners.

The default build includes the DVB hardware backend (`linux-dvb` feature). On
Linux it enumerates `/dev/dvb` frontends at startup; every frontend becomes a
scheduler adapter whose data-plane thread opens the frontend and DVR device and
tunes it. A frontend that cannot be opened is listed but disabled, like the C++
startup path, and `-N`/`--disable-dvb` skips detection entirely.

To exercise the data path without hardware, point the hidden `MINISATIP_REPLAY`
variable at a TS capture:

```sh
MINISATIP_REPLAY=/path/to/capture.ts \
  ./target/release/minisatip -V 127.0.0.1 -P 5500 -x 8080 -y 554 -e 0
```

The variable is a PLAN §3 harness aid, not a user-facing option: when it is set,
every adapter listed by `-e` (one adapter by default) is backed by
`FileReplayTuner`, looping over the file, and DVB enumeration is skipped. See
`docs/deviations.md` (DEV-056).

The end-to-end test starts the binary on OS-assigned ports with a generated
capture and covers OPTIONS, DESCRIBE, SETUP, PLAY (RTP/UDP), TEARDOWN and the
`/desc.xml` and `/state.json` endpoints:

```sh
cargo test -p minisatip --test smoke
```

## Logging

Log output follows the C++ reference and is implemented by `ms-log`:

```sh
minisatip -f -l http,pmt -v stream
```

- `-l module1,module2` raises the listed modules to the normal log level.
  The module names are `general`, `http`, `socketworks`, `stream`, `adapter`,
  `satipc`, `pmt`, `tables`, `dvbapi`, `lock`, `netceiver`, `ca`, `socket`,
  `utils`, `dmx`, `ssdp`, `dvb`, `ddci`; `-l all` enables every module.
  Listing an unknown name logs `module <name> not recognised`.
- `-v module1,module2` raises the listed modules to the debug level and implies
  `-l` for them (debug output is a superset). Both options accumulate, and
  `-v all` enables every debug module.
- With neither option the mask is empty and only the always-on `LOG0` lines
  print, exactly as in C++.
- `-f` keeps the process in the foreground and writes to stdout. Without `-f`
  the binary daemonises and stdout/stderr are redirected to `-F file`
  (default `/tmp/minisatip.log`), appending when the file already exists.
- `-g` writes through syslog (ident `minisatip`, `LOG_NOTICE`) instead of
  stdout; `-gg` also mirrors the lines to stderr (`LOG_PERROR`).

A foreground line looks like

```text
[22/10 14:07:09.042 main]: Running mode: foreground
```

The bracketed fields are the local timestamp with milliseconds and the thread
name. The main thread is `main`, each adapter data-plane thread is `AD<n>`, the
CI poller is `CA_poll`, and the listener threads use their own names.
