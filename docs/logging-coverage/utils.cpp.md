# `src/utils.cpp` (`#define DEFAULT_LOG LOG_UTILS`)

Ported **16** / not applicable **24** (40 active log sites).

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
| --- | --- | --- | --- |
| 130 | LOG_AND_RETURN | `check_strs: v is empty` | n/a: the Rust `log_level` always has the `LOG_LEVELS` table; no null array |
| 136 | LOG_AND_RETURN | `check_strs: s is empty` | `crates/ms-config/src/parse.rs:1527` |
| 179 | LOG_AND_RETURN | `parse_float: s is empty, mul=%d` | `crates/ms-net/src/satip.rs:351` |
| 181 | LOG_AND_RETURN | `parse_float: s not a number: %.*s, mul=%d` | `crates/ms-net/src/satip.rs:355` |
| 208 | LOG | `Could not set signal SIGBUS` | n/a: Rust installs only SIGINT/SIGTERM (`sys.rs`) |
| 211 | LOG | `Could not set signal SIGSEGV` | n/a: Rust installs only SIGINT/SIGTERM |
| 214 | LOG | `Could not set signal SIGABRT` | n/a: Rust installs only SIGINT/SIGTERM |
| 217 | LOG | `Could not set signal SIGFPE` | n/a: Rust installs only SIGINT/SIGTERM |
| 220 | LOG | `Could not set signal SIGILL` | n/a: Rust installs only SIGINT/SIGTERM |
| 224 | LOG | `Could not set signal SIGINT` | `crates/minisatip/src/sys.rs:29` |
| 228 | LOG | `Could not set signal SIGTERM` | `crates/minisatip/src/sys.rs:33` |
| 231 | LOG | `Could not ignore signal SIGHUP` | n/a: SIGHUP is not ignored by the Rust port |
| 234 | LOG | `Could not ignore signal SIGPIPE` | n/a: SIGPIPE is not ignored by the Rust port |
| 241 | LOG0 | `Stack trace:\n%s` | n/a: no stacktrace port |
| 243 | LOG | `C++ compiler does not support C++23 standard and stacktrace support is missing` | n/a: no stacktrace port |
| 294 | LOG | `Found %s running with pid %d, killing....` | `crates/minisatip/src/sys.rs:201` |
| 300 | LOG | `running %s in background and logging to %s` | `crates/minisatip/src/sys.rs:101` |
| 385 | LOG | `processing_file %p len %d:` | `crates/ms-net/src/api.rs:294` (`%p` normalised to `0x0`) |
| 419 | DEBUGM | `%s` | `crates/ms-net/src/api.rs:326` |
| 440 | LOG_AND_RETURN | `Could not open file %s` | `crates/ms-net/src/web.rs:821` |
| 443 | LOG | `readfile: %s is not a file` | `crates/ms-net/src/web.rs:816` |
| 451 | LOG_AND_RETURN | `mmap failed for file %s` | n/a: Rust reads with `fs::read`, there is no `mmap` |
| 454 | LOG | `opened %s fd %d at %p - %d bytes` | n/a: Rust reads with `fs::read`, there is no `open`/`mmap` |
| 501 | LOG | `Failed to create thread: %s, error %d %s` | n/a: `start_new_thread` is not ported (threads use `std::thread`) |
| 513 | LOG | `pthread_setschedparam failed with error %d` | n/a: thread priority setting is not ported |
| 524 | LOG | `ERROR: Maximum thread limit (100) reached, cannot add thread %l` | n/a: the C++ join-thread registry is not ported |
| 529 | LOG | `%s: pthread %lx` | n/a: the C++ join-thread registry is not ported |
| 537 | LOGM | `Joining thread %lx` | n/a: the C++ join-thread registry is not ported |
| 539 | LOG | `Join thread failed for %lx with %d %s` | n/a: the C++ join-thread registry is not ported |
| 578 | LOG | `%s: ZERO LENGTH` | n/a: in `ms-log` (`_hexdump` port) |
| 582 | LOG | `%s: NEGATIVE LENGTH: %i\n` | n/a: in `ms-log` (`_hexdump` port) |
| 619 | LOG | `\n%s` | n/a: in `ms-log` (`_hexdump` port) |
| 621 | LOG | `%s:\n%s` | n/a: in `ms-log` (`_hexdump` port) |
| 673 | hexdump | `crc_32 ` | n/a: in `ms-log` (`crc_32` port) |
| 689 | LOG | `%s: pid %04d (%04X) CC=%X CRC=%08X %s pos: %d packet %d : [%02X ` | n/a: in `ms-log` (`dump_packets` port) |
| 706 | LOG_AND_RETURN | `Not enough space to copy pid %d, len %d from %d, srclen %d` | `crates/ms-ca-drivers/src/ddci/psi.rs:14` |
| 725 | LOG | `pid %d, left -> %d, len %d, cc %d` | `crates/ms-ca-drivers/src/ddci/psi.rs:37` |
| 726 | hexdump | `packet -> ` | `crates/ms-ca-drivers/src/ddci/psi.rs:38` |
| 792 | LOG | `cannot open %s` | `crates/ms-tuner/src/satipc/util.rs:190` |
| 797 | LOG | `cannot read from %s` | `crates/ms-tuner/src/satipc/util.rs:187` |

Commented, not active: line 190 (`LOG("atof returned ...")`), line 406 (`LOG("%s", outp)`), line 535 (`LOG("starting %s", ...)`).
