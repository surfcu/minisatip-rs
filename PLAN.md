# minisatip-rs — Rust rewrite plan

Living plan for rewriting minisatip in Rust. Every Claude Code session reads this
file first and updates **Status** and the milestone checkboxes last.

Reference implementation: `catalinii/minisatip` master (C++, ~31k lines src,
~13.5k lines tests). Work happens from the fork `surfcu/minisatip`.

---

## 1. Goal

A Rust implementation of minisatip that is a drop-in replacement for the C++
binary: same RTSP/HTTP behaviour, same CLI flags, same web UI, same hardware
support. Its internals are restructured so that CA/CAPMT handling, adapter
allocation and PSI processing are each owned by one component.

## 2. Principles

1. **The C++ binary is the oracle.** Behaviour is defined by what the C++ build
   does, verified with the differential harness. Deliberate deviations are listed
   in `docs/deviations.md`, each with a reason.
2. **Harness before port.** No module is ported until the harness can exercise it
   and compare outputs.
3. **A module is done when the two outputs match**, not when it compiles.
4. **No shared mutable globals.** Data plane: one thread per adapter. Control
   plane: one async task owning all decisions. They communicate only through messages.
5. **Keep it small.** Target the same devices (MIPS/ARM routers, Enigma2 boxes):
   static musl builds, release binary < 3 MB stripped, no heavy dependencies.
6. **Port the tests first.** Each module starts by porting its C++ test file.

## 3. Scope

### In scope (parity target)
- RTSP/SAT>IP server, HTTP streaming, RTP/UDP, RTP-over-TCP, SRT output
- SSDP/UPnP discovery, web UI (reuse `html/` as-is), status/variables API
- Linux DVB frontends: DiSEqC 1.0/1.1, unicable/JESS, LNB sharing, adapter allocation
- SAT>IP client backend (satipc), netceiver backend
- PSI: PAT/PMT/CAT/SDT, section assembly, charset decoding
- CA: EN50221 CI stack incl. CI+, DDCI, DVBAPI (oscam), software CSA/AES,
  hardware descrambler
- Full CLI option compatibility (`opts.cpp`)

### Explicitly deferred
- Enigma2-specific paths: until after M9, only if someone can test them
- CI+ crypto: ported last, behind a cargo feature, marked experimental until
  validated on real CI+ CAMs

### Non-goals
- New features before parity (exception: anything needed by the harness)
- Changing the web UI or wire protocols

## 4. Architecture

```
            ┌──────────────── control plane (tokio, single task) ────────────────┐
 clients ──▶│ rtsp / http / ssdp ──▶ session mgr ──▶ scheduler ──▶ adapter cmds │
            │                             │              ▲                       │
            │                       psi service ──▶ ca reconciler ──▶ ca drivers │
            └─────────────────────────────┼────────────────────────────┼─────────┘
                                          │ events                     │ CAPMT / CW
 data plane (thread per adapter):  dvr read → pid router → subscribers (rtp/http/srt)
                                                     └──▶ descrambler (csa/hw/ddci)
```

### Crates (cargo workspace)
| Crate | Responsibility | C++ origin |
|---|---|---|
| `ms-ts` | TS packet, PES/section assembly, CRC, PID router, zero-copy buffers | parts of utils, pmt |
| `ms-psi` | PAT/PMT/CAT/SDT parsing, versioning, charset tables | tables, pmt, dvb_charset |
| `ms-tuner` | `Tuner` trait + backends: linux-dvb, satipc, netceiver, file-replay | dvb, satipc, netceiver |
| `ms-sched` | adapter allocation, LNB/DiSEqC/unicable constraints, same-mux reuse | adapter |
| `ms-ca` | desired/actual state reconciler, CAPMT diffing, policies | ca, ddci, dvbapi (logic) |
| `ms-ca-drivers` | en50221 (+ci-plus feature), ddci, dvbapi, hw descrambler, csa/aes | ca, ddci, dvbapi, csa, aes, hw_descrambler |
| `ms-net` | rtsp, http, rtp/udp/tcp, srt, ssdp | socketworks, stream, minisatip, srt, httpc |
| `ms-config` | CLI compat parser, config file, variables/status API | opts, api |
| `minisatip` | binary: wiring, logging, signals | minisatip |
| `ms-harness` | differential test runner (see §5) | new |
| `ms-fakecam` | EN50221/DDCI CAM simulator (see §6) | new |

### Key interfaces (sketch)
```rust
trait Tuner {
    fn tune(&mut self, t: &TuneParams) -> Result<()>;
    fn set_pids(&mut self, pids: &PidSet) -> Result<()>;
    fn status(&self) -> SignalStatus;
    fn read(&mut self, buf: &mut TsBuf) -> Result<usize>;
}

trait CaDriver {
    fn capabilities(&self) -> CaCaps;              // max services, bandwidth
    fn apply(&mut self, diff: &CapmtDiff) -> Result<()>;
    fn events(&mut self) -> Option<CaEvent>;        // CW, MMI, ready, error
}
```

### CA reconciler (core change versus C++)
- Desired state: the set of services each CAM must descramble, derived from active subscriptions.
- Actual state: the CAPMT list last acknowledged by each CAM.
- The reconcile step emits a minimal diff (prefer add/update over `only`).
- Policies are pluggable and individually testable:
  - lazy leave with hysteresis
  - rejoin debounce
  - least-used CAM selection
  - bandwidth balancing
  - pre-packing most-requested services
- Must reproduce the current C++ CAPMT sequences first (see M6), then policies may
  deviate, documented in `docs/deviations.md`.

## 5. Differential harness (`ms-harness`)

Runs the C++ binary and the Rust binary side by side with identical inputs and diffs:
- RTSP/HTTP responses (normalised: session IDs, timestamps, ports)
- Output TS per stream: PID set, packet counts, PAT/PMT/SDT rewrites, CC continuity
- CAPMT/APDU sequence sent to the fake CAM (order and content)
- DVBAPI messages sent to a fake oscam socket

Inputs:
- **TS captures** in `harness/captures/` (recorded on the real box with dvbsnoop or the
  C++ `-k` output): FTA, scrambled, multi-service mux, PMT version change, zapping.
- **Scenario scripts** (YAML): sequences of RTSP SETUP/PLAY/TEARDOWN, HTTP requests,
  timing, expected tuner commands.
- **file-replay tuner** in both builds: add a minimal replay backend to the C++ fork
  if `tools/replay.py` / `test_play` aren't sufficient (decide in M0).

Output: per-scenario pass/fail, plus a readable diff. CI runs every scenario.

## 6. Fake CAM (`ms-fakecam`)

- Speaks EN50221 over a pty/socket for CI, and the DDCI TS interface.
- Configurable: number of services, CI bandwidth, descramble start latency, CAPMT
  handling quirks (e.g. ignores `update`, resets on `only`).
- Records every APDU with a timestamp for the harness.
- Multi-CAM: N instances, to test selection and balancing without buying CAMs.
- Calibrated against the real CAM using the numbers from the `measure-ddci` branch.

## 7. Milestones

Each milestone lists exit criteria. Tick only when all criteria hold.

### M0 — Groundwork
- [ ] Cargo workspace, CI (x86_64 + cross-build armv7/aarch64/mipsel musl)
- [ ] Decide replay mechanism for the C++ side; build C++ reference binary in CI
- [ ] Record initial TS captures on the box (FTA, scrambled, multi-service)
- [ ] `docs/deviations.md`, `docs/hardware-matrix.md` created
- **Exit:** CI green on all targets with an empty binary; C++ reference builds in CI

### M1 — Harness + fake CAM
- [ ] `ms-harness` drives the C++ binary through scenarios, captures all outputs
- [ ] `ms-fakecam` EN50221 + DDCI, records APDUs
- [ ] 10+ scenarios recorded as golden output from C++
- **Exit:** harness runs C++ against itself deterministically (zero diffs, 10 runs)

### M2 — TS + PSI
- [ ] Port `test_pmt.cpp` (PSI parts), `test_dvb_charset.cpp`
- [ ] `ms-ts`, `ms-psi` incl. charset tables (generate, don't hand-port)
- [ ] Fuzz targets for section parsing
- **Exit:** ported tests pass; fuzzers run 1 h clean

### M3 — Networking + streaming (file-replay tuner)
- [ ] Port `test_socketworks.cpp`, `test_stream.cpp`, `test_srt.cpp`
- [ ] RTSP, HTTP, RTP/UDP, RTP-over-TCP, SRT, SSDP; reuse `html/`
- [ ] CLI compat parser; port `test_opts.cpp`
- **Exit:** harness FTA scenarios match C++; tvheadend plays from the Rust build via replay

### M4 — Linux DVB + scheduler
- [ ] Port `test_dvb.cpp`, `test_adapter.cpp`
- [ ] Linux DVB backend, DiSEqC, unicable/JESS, LNB sharing
- [ ] Scheduler incl. same-mux reuse (#1509 behaviour)
- **Exit:** FTA on real tuners on the box, all 8 adapters; zapping scenarios match C++

### M5 — DVBAPI + software descrambling
- [ ] Port `test_dvbapi.cpp`
- [ ] DVBAPI protocol against fake oscam and real oscam; CSA (libdvbcsa FFI) + AES
- **Exit:** scrambled channels play via oscam on the box; DVBAPI message diff clean

### M6 — CA reconciler + DDCI
- [ ] Port `test_ddci.cpp` and DDCI-related parts of `test_ca.cpp`
- [ ] `ms-ca` reconciler with a "C++-compatible" policy set reproducing current sequences
- [ ] DDCI driver: virtual adapter, PID remapping, generated PAT/SDT (#1511 behaviour)
- **Exit:** CAPMT sequences identical to C++ in all scenarios; real CAM on the box
  descrambles with no new glitches on join/leave/rejoin

### M7 — EN50221 CI stack
- [ ] Port remaining `test_ca.cpp`
- [ ] Resources: resource manager, application info, CA support, date-time, MMI
- **Exit:** APDU diff clean with fake CAM; real CAM works through the CI path

### M8 — Remaining backends
- [ ] SAT>IP client (port `test_satipc.cpp`, `test_satipc_rtsp.cpp`, `fake_satip_server.h`)
- [ ] Netceiver, hardware descrambler (port `test_hw_descrambler.cpp`)
- [ ] Status/variables API parity
- **Exit:** ported tests pass; satipc against at least one real SAT>IP server

### M9 — Parity hardening
- [ ] All C++ tests ported and passing; full harness suite clean
- [ ] Soak test on the box: 7 days as tvheadend backend, no regressions
- [ ] Memory/CPU/binary size comparison vs C++ documented
- **Exit:** daily-driver replacement for the user's own setup

### M10 — Experimental + improvements
- [ ] CI+ behind `ci-plus` feature; needs external testers with CI+ CAMs
- [ ] New CA policies (bandwidth balancing, pre-packing) as documented deviations
- [ ] Decide on upstream proposal vs separate project

## 8. Hardware matrix

| Feature | Validated on | How |
|---|---|---|
| Linux DVB, 8 tuners | user's box | real |
| DDCI, 1 CAM | user's box | real |
| Multi-CAM | — | fake CAM only |
| DVBAPI / oscam | user's box | real |
| Unicable/JESS | — | tests + external testers |
| CI+ | — | external testers |
| SAT>IP client | TBD | real server if available |
| Netceiver, hw descrambler, Enigma2 | — | tests only |

Unvalidated rows must be called out in release notes.

## 9. Risks

| Risk | Mitigation |
|---|---|
| Subtle CAPMT timing/order differences cause glitches | harness diffs CAPMT sequences; M6 requires identical output first |
| Hardware quirks in C++ lost in translation | port tests first; read git blame on quirky code before porting |
| CI+ crypto wrong and unverifiable | feature-gated, experimental, external testers only |
| Upstream won't adopt Rust | treat parity for own setup (M9) as the success bar; decide in M10 |
| Harness nondeterminism (timing) | fake clock in Rust build; normalise timestamps; retry-free scenarios |
| Scope creep before parity | non-goals list; new ideas go to §11 Backlog |

## 10. Conventions

- Code comments: max 2 lines (matches upstream `AGENTS.md`).
- Rust 2024 edition, `clippy -D warnings`, `rustfmt`, no `unsafe` outside FFI/ioctl
  modules, and each `unsafe` block has a `// SAFETY:` line.
- Git identity: `surfcu <59124393+surfcu@users.noreply.github.com>`.
- No AI co-author or session trailers in commits or PRs.
- One milestone = one branch (`rs/m3-net` etc.); small commits; PRs to own fork.

## 11. Session workflow (Claude Code)

1. Read `PLAN.md`, then the current milestone's open items.
2. Pick one item; port its tests first, then the code.
3. Run `cargo test`, the harness scenarios for the touched area, and clippy.
4. Commit, then update **Status** below (what changed, what's next, blockers).
5. Hardware steps: write exact commands for the user to run on the box, or run them
   directly when the session is on the box.

## 12. Backlog (post-parity ideas)

- CA bandwidth balancing and pre-packing from request history
- Prometheus metrics endpoint
- Typed config file alongside CLI flags
- JSON status API

---

## Status

- **Current milestone:** M0
- **Last session:** — (plan created 2026-10-21)
- **Next:** set up cargo workspace + CI; decide C++ replay mechanism (`tools/replay.py`,
  `tests/test_play`)
- **Blockers:** none
