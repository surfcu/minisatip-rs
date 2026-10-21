# adapter.cpp logging coverage

C++ source: `src/adapter.cpp` (read-only oracle at `~/upstream-repo/minisatip`).
Rust counterparts: `crates/ms-sched`, `crates/ms-config`, `crates/ms-net`,
`crates/minisatip`.

`LOG` is `LOG_GENERAL` (`logging.h`), `LOGM` uses `DEFAULT_LOG LOG_ADAPTER`
(adapter.cpp:58), `DEBUGM` is the debug level of that mask. Sites without a Rust
code point are marked `n/a` with the reason.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---:|---|---|---|
| 138 | LOG | `could not resize the buffer of adapter %d from %d to %d ` | n/a: adapters own fixed-size `Vec`/`TsBuf` buffers; there is no `realloc` pass (`adapter_resize_buffers`) in the port |
| 143 | LOG | `adapter %d: buffer resized from %d to %d bytes` | n/a: same fixed-size buffer model as line 138 |
| 181 | LOG | `Keeping adapter %d open as the initialization is slow` | n/a: `adapter_timeout` is a sockets-table callback in C++; the satipc driver closes idle devices on its own timer without these lines |
| 187 | LOG | `Keeping adapter %d open as it's a CI device` | n/a: no sockets-timeout adapter close loop |
| 195 | LOG | `Keeping adapter %d open as there are active streams` | n/a: no sockets-timeout adapter close loop |
| 204 | LOG | `adapter %d is already used by a slave adapters %d` | n/a: no sockets-timeout adapter close loop |
| 210 | LOG | `Requested adapter %d close due to timeout %d sec, result %d max_rtime ` | n/a: no sockets-timeout adapter close loop |
| 226 | LOG | `closing DVR socket %d pos %d aid %d` | n/a: the port has no per-adapter DVR `sockets` object (`close_adapter_for_socket`) |
| 270 | LOG_AND_RETURN | `opening adapter %i failed because adapter is disabled` | n/a: `Scheduler::init_hw` is state-only (ms-sched/src/lib.rs:544); it never opens hardware |
| 274 | LOG_AND_RETURN | `opening adapter %i failed because adapter is already enabled` | n/a: state-only `init_hw` |
| 294 | LOG_AND_RETURN | `opening adapter %i failed because adapter does not have an op` | n/a: state-only `init_hw`; tuners are supplied by `add_adapter` |
| 305 | LOG_AND_RETURN | `opening adapter %i failed to open the adapter with code %d` | n/a: hardware open failures are logged by the backend (`linux_dvb/backend.rs`, `linux_dvb/sys.rs`) |
| 312 | LOG_AND_RETURN | `opening adapter %i failed because adapter the adapter buffer wa` | n/a: no adapter buffer allocation in the port |
| 353 | LOG | `Slow adapter %d detected` | n/a: no open-time latency measurement |
| 360 | LOG | `done opening adapter %i delivery systems: %s %s %s %s` | crates/minisatip/src/runtime.rs:1560 |
| 377 | LOG | `starting init_all_hw %d` | n/a: no global `init_all_hw`/`init_complete` pass; adapters are spawned once at startup |
| 392 | LOG | `Failed to init device %d with return value %d` | n/a: no numeric init return value; backend open errors are logged where they occur |
| 397 | LOG | `done init_hw %d` | n/a: no `init_all_hw` pass |
| 418 | LOG | `closing adapter %d  -> fe:%d dvr:%d, sock:%d, fe_sock:%d` | n/a: adapters live for the process lifetime; there is no `close_adapter` lifecycle |
| 454 | LOG | `done closing adapter %d` | n/a: no `close_adapter` lifecycle |
| 458 | LOG | `Slave adapter %d of adapter %d will be closed` | n/a: no `close_adapter` lifecycle |
| 542 | LOG | `FE %d mapped to Adapter %d, sys %s` | n/a: the FE map is built without logging; `get_free_adapter` applies it directly (ms-sched/src/lib.rs:564) |
| 571 | LOG | `Dumping adapters:` | n/a: no debug dump function |
| 574 | LOG | `%d\|f: %d sid_cnt:%d master_sid:%d master_source:%d del_sys: ` | n/a: no debug dump function |
| 600 | LOG | `Dumping pids table for adapter %d, number of unknown ` | n/a: no debug dump function |
| 605 | LOG | `pid %d, fd %d, packets %d, d/c/c2 errs %d/%d/%d, flags %d, ` | n/a: no debug dump function |
| 655 | LOG | `adapter %d used is set for adapter %d but it is disabled, ` | n/a: the port ignores `used` slots of disabled adapters instead of clearing and logging them (ms-sched/src/lib.rs:812) |
| 673 | LOGM | `%s: adapter %d master %d (pol %d, band %d, diseqc ` | crates/ms-sched/src/lib.rs:829 |
| 702 | LOG | `Mapping adapter %d src %d to src %d` | crates/ms-sched/src/lib.rs:867 |
| 728 | LOG | `get free adapter %d - a[%d] => e:%d m:%d sid_cnt:%d src:%d f:%d ` | crates/ms-sched/src/lib.rs:590 |
| 736 | LOG | `get free adapter %d msys %s requested %s` | crates/ms-sched/src/lib.rs:603 |
| 782 | LOGM | `get free adapter found slave adapter %d` | crates/ms-sched/src/lib.rs:668 |
| 788 | LOG | `no adapter found for src:%d f:%d pol:%d msys:%d` | crates/ms-sched/src/lib.rs:673 |
| 865 | LOG | `set adapter %d for sid %d m:%d s:%d` | crates/ms-sched/src/lib.rs:896 |
| 875 | DEBUGM | `skipping committing adapter %d as no pids were updated` | n/a: the port applies PID sets directly through `AdapterTuner::set_pids`; there is no `commit`/`pids_updates` callback |
| 879 | LOGM | `adapter_commit for adapter %d` | n/a: no `adapter_commit` callback |
| 895 | LOG | `%s adapter mismatch: got %d expected %d` | n/a: no `streams::adapter` mirror; `close_adapter_for_stream` owns the stream set (ms-sched/src/lib.rs:906) |
| 904 | LOG | `closed adapter %d for sid %d m:%d sid_cnt:%d` | crates/ms-sched/src/lib.rs:930 |
| 918 | LOGM | `adapter %d freed from slave adapter %d` | crates/ms-sched/src/lib.rs:949 |
| 935 | LOG | `ad %d [%d], another update_pids is in progress skipping` | n/a: `update_pids` takes `&mut self`; there is no re-entrancy flag |
| 941 | LOG | `adapter %d in error state %d` | n/a: `Adapter` has no `err` field; tuner failures are returned as `TuneError` |
| 945 | LOGM | `Updating pids for adapter %d` | crates/ms-sched/src/lib.rs:1384 |
| 968 | LOG0 | `failed to remove pid %d, reporting error` | crates/ms-sched/src/lib.rs:1403 |
| 988 | LOG | `maximum number of pids %d out of %d reached` | crates/ms-sched/src/lib.rs:1421 |
| 1004 | LOG0 | `Maximum pid filter reached, lowering the value to %d` | crates/ms-sched/src/lib.rs:1437 |
| 1007 | LOG0 | `failed to add pid %d with only %d active pids, ` | crates/ms-sched/src/lib.rs:1442 |
| 1044 | LOGM | `adapter post_tune: aid %d` | crates/ms-sched/src/lib.rs:1317 |
| 1056 | LOG | `Adding pid %d to the list of pids as not explicitly added ` | crates/ms-sched/src/lib.rs:1339 |
| 1078 | LOGM | `adapter tune: sid %d aid %d => sock: %d ` | crates/ms-sched/src/lib.rs:1066 |
| 1116 | LOG | `not tuning for sid %d (do_tune=%d, master_sid=%d)` | crates/ms-sched/src/lib.rs:1128 |
| 1204 | LOG | `deleting pids on adapter %d, sid %d, pids=%s` | n/a: the port API takes `PidSet` values, not comma-separated `pids=` strings |
| 1209 | LOG | `deleting all the pids on adapter %d, sid %d, pids=%s` | n/a: no `delpids=all` string parser; `PidSet` carries the expansion |
| 1236 | LOG | `found already existing pid %d flags %d` | crates/ms-sched/src/lib.rs:1177 |
| 1263 | LOG | `MAX_PIDS (%d) reached for adapter %d in adding PID: %d` | crates/ms-sched/src/lib.rs:1199 |
| 1280 | LOG | `adding pids to adapter %d, sid %d, pids=%s` | n/a: structured `PidSet` API, no string entry point |
| 1327 | LOGM | `new parameters: f:%d, plp/isi:%d, diseqc:%d, pol:%d, sr:%d, mtype:%d` | crates/ms-sched/src/lib.rs:728 |
| 1330 | LOGM | `old parameters: f:%d, plp/isi:%d, diseqc:%d, pol:%d, sr:%d, mtype:%d` | crates/ms-sched/src/lib.rs:737 |
| 1382 | LOG | `setting DVB parameters for adapter %d - master_sid %d sid %d old f:%d` | crates/ms-sched/src/lib.rs:983 |
| 1392 | LOG | `secondary stream requested tune, not gonna happen ad: f:%d sr:%d pol:%d plp/isi:%d src:%d mod %d -> ` | crates/ms-sched/src/lib.rs:997 |
| 1426 | LOG | `%s:%d: get_adapter returns NULL for adapter_id %d` | n/a: `get_adapter` is `Option`-based and has no logging wrapper |
| 1558 | LOGM | `describe_adapter: sid %d, aid %d => %s` | crates/ms-net/src/session.rs:1009 (`dataplane.rs:971`) |
| 1612 | LOG | `Netceiver exit failed` | n/a: no `recv_exit` hook; `NetceiverTuner` deletes receivers on `Drop` |
| 1694 | LOG | `Setting %s adapter %d slot %d freq %d pin %d` | crates/ms-config/src/parse.rs:914 |
| 1766 | LOG | `Setting diseqc adapter %d fast %d addr 0x%02x committed_no %d ` | crates/ms-config/src/parse.rs:989 |
| 1806 | LOG | `Setting source %d (src=%d) to adapter %d position %d` | crates/ms-config/src/parse.rs:1449 |
| 1857 | LOG | `Setting diseqc multi adapter %d position %d` | crates/ms-config/src/parse.rs:1116 |
| 1890 | LOG | `LNB parameters not correctly specified: %.*s` | crates/ms-config/src/parse.rs:1145 |
| 1920 | LOG | `Setting diseqc adapter %d lnb_low %d lnb_high %d lnb_switch %d` | crates/ms-config/src/parse.rs:1180 |
| 2005 | LOG | `Setting diseqc timing for adapter %d before cmd %d after cmd %d ` | crates/ms-config/src/parse.rs:1070 |
| 2055 | LOG | `Setting master adapter %d for adapter %d` | crates/ms-config/src/parse.rs:1222 |
| 2081 | LOG | `Set default timeout to %d` | crates/ms-config/src/parse.rs:1254 |
| 2105 | LOG | `Set timeout for adapter %d to %d` | crates/ms-config/src/parse.rs:1280 |
| 2126 | LOG | `Delivery system is missing, the format is ` | crates/ms-config/src/parse.rs:1293 |
| 2146 | LOG | `Setting delivery system for adapter %d to %s and %s` | crates/ms-config/src/parse.rs:1310 |
| 2235 | LOG | `Setting signal multipler for adapter %d%s ` | crates/ms-config/src/parse.rs:1407 |
| 2246 | LOG_AND_RETURN | `delsys_match: adapter is NULL, delsys %d` | n/a: `delsys_match` takes `&Adapter`; null adapters cannot reach it |
| 2249 | LOG_AND_RETURN | `delsys_match: requesting delsys is 0 for adapter handle %d` | n/a: the port uses `DeliverySystem` enums, there is no 0/undefined request at this point |
| 2284 | LOG | `get_signal%s took %jd ms for adapter %d handle %d (status: ` | n/a: no periodic `signal_thread`; signal reads are on demand (`backend.rs:135`) and untimed |

ported 39 / not applicable 39
