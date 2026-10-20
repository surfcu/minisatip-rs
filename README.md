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
