# minisatip-rs

A Rust rewrite of [minisatip](https://github.com/catalinii/minisatip), aiming to be
a drop-in replacement: the same RTSP/HTTP streaming, SAT>IP behaviour, CLI flags,
web UI and hardware support, with CA, adapter allocation and PSI processing each
owned by one component.

The architecture, milestones and conventions are in [PLAN.md](PLAN.md). Current
status: **M0 (groundwork)** — the workspace and CI exist; all crates are
placeholders.

## Layout

Cargo workspace (Rust 2024) with these members:

| Crate | Role |
|---|---|
| `ms-ts` | TS packet/section handling, CRC, PID router |
| `ms-psi` | PAT/PMT/CAT/SDT parsing and charset tables |
| `ms-tuner` | `Tuner` trait and backends |
| `ms-sched` | adapter allocation and DiSEqC/unicable constraints |
| `ms-ca` | CA state reconciler and CAPMT policies |
| `ms-ca-drivers` | EN50221, DDCI, DVBAPI and descramblers |
| `ms-net` | RTSP, HTTP, RTP, SRT and SSDP |
| `ms-config` | CLI/config compatibility and status API |
| `ms-harness` | differential test runner |
| `ms-fakecam` | EN50221/DDCI CAM simulator |
| `minisatip` | binary: wiring, logging, signals |

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

Cross builds use [`cross`](https://github.com/cross-rs/cross); see
`.github/workflows/ci.yml` for the musl target matrix. The C++ reference build is
also produced there.

## Running the binary

The binary starts an RTSP listener (`-y`, default 554), an HTTP listener (`-x`,
default 8080) and, unless `--disable-ssdp` is given, the SSDP discovery socket.
Each scheduler adapter has one data-plane thread. `SIGINT`/`SIGTERM` send the
SSDP byebye set and close the listeners.

This milestone has no DVB hardware backend yet. To exercise the data path, point
the hidden `MINISATIP_REPLAY` variable at a TS capture:

```sh
MINISATIP_REPLAY=/path/to/capture.ts \
  ./target/release/minisatip -V 127.0.0.1 -P 5500 -x 8080 -y 554 -e 0
```

The variable is a PLAN §3 harness aid, not a user-facing option: when it is set,
every adapter listed by `-e` (one adapter by default) is backed by
`FileReplayTuner`, looping over the file. When it is unset, no adapters are
created. See `docs/deviations.md` (DEV-056).

The end-to-end test starts the binary on OS-assigned ports with a generated
capture and covers OPTIONS, DESCRIBE, SETUP, PLAY (RTP/UDP), TEARDOWN and the
`/desc.xml` and `/state.json` endpoints:

```sh
cargo test -p minisatip --test smoke
```
