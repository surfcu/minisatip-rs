# dvb.cpp logging coverage

C++ source: `src/dvb.cpp` (read-only oracle at `~/upstream-repo/minisatip`).
Rust counterparts: `crates/ms-tuner/src/linux_dvb`, `crates/ms-net`,
`crates/ms-sched`, `crates/minisatip`.

`LOGM` uses `DEFAULT_LOG LOG_DVB` (dvb.cpp:53), `LOG` is `LOG_GENERAL`.
Sites without a Rust code point are marked `n/a` with the reason.

| C++ line | macro | text (first 60 chars) | Rust file:line or "n/a: <reason>" |
|---:|---|---|---|
| 205 | LOG | `a` (inside the unused `INVALID_URL` macro) | n/a: the `INVALID_URL` helper macro has no port |
| 279 | LOG_AND_RETURN | `no ? found in URL` | crates/ms-net/src/satip.rs:35 |
| 286 | LOG | `detect_dvb_parameters (S)-> %.*s` | crates/ms-net/src/satip.rs:47 |
| 373 | LOG | `detect_dvb_parameters: PIDs changed: old = %s, new = %s` | crates/ms-net/src/satip.rs:124 |
| 381 | LOG | `detect_dvb_parameters (E) -> src=%d, fe=%d, freq=%d, fec=%d, sr=%d, ` | crates/ms-net/src/satip.rs:135 |
| 395 | LOG | `copy_dvb_param start -> src=%d, fe=%d, freq=%d, fec=%d, sr=%d, ` | crates/ms-sched/src/lib.rs:87 |
| 463 | LOG | `copy_dvb_parameters -> src=%d, fe=%d, freq=%d, fec=%d sr=%d, pol=%d, ` | crates/ms-sched/src/lib.rs:108 |
| 498 | LOG | `get_db_snr_map -> DVB-C modulation SNR scale not tested!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:218 |
| 530 | LOG | `get_db_snr_map -> DVB-S modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:231 |
| 536 | LOG | `get_db_snr_map -> DVB-S modulation SNR scale unkown!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:236 |
| 574 | LOG | `get_db_snr_map -> DVB-S2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:251 |
| 610 | LOG | `get_db_snr_map -> DVB-S2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:265 |
| 616 | LOG | `get_db_snr_map -> DVB-S2 modulation SNR scale unkown!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:270 |
| 644 | LOG | `get_db_snr_map -> DVB-T modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:282 |
| 670 | LOG | `get_db_snr_map -> DVB-T modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:293 |
| 696 | LOG | `get_db_snr_map -> DVB-T modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:304 |
| 702 | LOG | `get_db_snr_map -> DVB-T modulation SNR scale unkown!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:309 |
| 733 | LOG | `get_db_snr_map -> DVB-T2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:322 |
| 762 | LOG | `get_db_snr_map -> DVB-T2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:334 |
| 791 | LOG | `get_db_snr_map -> DVB-T2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:346 |
| 820 | LOG | `get_db_snr_map -> DVB-T2 modulation SNR scale not ` | crates/ms-tuner/src/linux_dvb/transponder.rs:358 |
| 826 | LOG | `get_db_snr_map -> DVB-T2 modulation SNR scale unkown!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:363 |
| 831 | LOG | `get_db_snr_map -> Modulation SNR scale unkown!!` | crates/ms-tuner/src/linux_dvb/transponder.rs:368 |
| 835 | LOGM | `get_db_snr_map -> src=%d, fe=%d, msys=%d, mtype=%d, fec=%d, db_map=%f` | crates/ms-tuner/src/linux_dvb/transponder.rs:374 |
| 854 | LOG | `setProcData %s -> %d` | n/a: `/proc/stb` helpers are not ported |
| 860 | LOG | `setProcData open failed, %s: %m` | n/a: `/proc/stb` helpers are not ported |
| 871 | LOG | `DMX_SET_SOURCE: Failed opening %s` | n/a: `DvbDevice::set_demux_source` exists but the open path never calls it (device.rs:184) |
| 873 | LOG | `DMX_SET_SOURCE failed for adapter %d - %d: %s` | n/a: same as line 871 |
| 876 | LOG | `Set DMX_SET_SOURCE for adapter %d to %d` | n/a: same as line 871 |
| 898 | LOG | `trying to open [%d] adapter %d and frontend %d` | crates/ms-tuner/src/linux_dvb/backend.rs:55 |
| 911 | LOG | `Could not open %s in RW mode (fe: %d, dvr: %d) error %d: %s` | crates/ms-tuner/src/linux_dvb/sys.rs:240 and :255 (frontend/dvr branches) |
| 925 | LOG | `opened DVB adapter %d fe:%d dvr:%d` | crates/ms-tuner/src/linux_dvb/backend.rs:64 |
| 927 | LOG | `couldn't set DVR buffer size error %d: %s` | crates/ms-tuner/src/linux_dvb/backend.rs:91 |
| 929 | LOG | `DVR buffer set to %d bytes` | crates/ms-tuner/src/linux_dvb/backend.rs:96 |
| 937 | LOG | `FBC master adapter is recommended to be adapter 0 or 1, ` | n/a: FBC multi-frontend support is not ported |
| 955 | LOG | `send_diseqc: FE_DISEQC_SEND_MASTER_CMD %s failed for fd %d: ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:70 |
| 999 | LOGM | `send_diseqc fd %d, pos = %d (c %d u %d), pol = %d, hiband = %d` | crates/ms-tuner/src/linux_dvb/diseqc.rs:150 |
| 1005 | LOG | `send_diseqc: FE_SET_TONE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:161 |
| 1008 | LOG | `send_diseqc: FE_SET_VOLTAGE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:165 |
| 1025 | LOG | `send_diseqc: FE_DISEQC_SEND_BURST failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:180 |
| 1032 | LOG | `send_diseqc: FE_SET_TONE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:190 |
| 1059 | LOGM | `send_unicable fd %d, freq %d, ufreq %d, pos = %d, pol = %d, hiband = ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:300 |
| 1067 | LOG | `send_unicable: pre voltage  SEC_VOLTAGE_13 failed for fd %d: ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:261 |
| 1071 | LOG | `send_unicable: FE_SET_TONE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:269 |
| 1076 | LOG | `send_unicable: FE_SET_VOLTAGE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:277 |
| 1080 | LOG | `send_unicable: FE_SET_VOLTAGE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:282 |
| 1105 | LOGM | `send_jess fd %d, freq %d, ufreq %d, pos = %d, pol = %d, hiband = %d, ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:331 |
| 1115 | LOG | `send_jess: pre voltage  SEC_VOLTAGE_13 failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:261 |
| 1118 | LOG | `send_jess: FE_SET_TONE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:269 |
| 1124 | LOG | `send_jess: FE_SET_VOLTAGE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:277 |
| 1129 | LOG | `send_jess: FE_SET_VOLTAGE failed for fd %d: %s` | crates/ms-tuner/src/linux_dvb/diseqc.rs:282 |
| 1151 | LOG | `Adapter %d master source %d for specified but adapter not ` | n/a: the caller passes an optional `MasterView`; there is no master-adapter lookup here |
| 1203 | LOG | `Slave adapter %d wants to change the paramters of the ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:384 |
| 1216 | LOGM | `Skip sending diseqc commands since the switch position ` | crates/ms-tuner/src/linux_dvb/diseqc.rs:404 |
| 1221 | LOGM | `Slave adapter %d not sending switch updates using master %d` | crates/ms-tuner/src/linux_dvb/diseqc.rs:411 |
| 1296 | LOG | `dvb_tune: DTV_CLEAR failed %d %s, continuing` | crates/ms-tuner/src/linux_dvb/state.rs:159 |
| 1310 | LOG_AND_RETURN | `Frequency %d is not within range ` | crates/ms-tuner/src/linux_dvb/state.rs:174 |
| 1341 | LOG | `tuning to %d(%d) pol: %s (%d) sr:%d fec:%s delsys:%s mod:%s ` | crates/ms-tuner/src/linux_dvb/state.rs:203 |
| 1361 | LOG_AND_RETURN | `Frequency %d is not within range ` | crates/ms-tuner/src/linux_dvb/state.rs:220 |
| 1397 | LOG | `tuning to %d delsys: %s bw:%d inversion:%s mod:%s fec:%s guard:%s ` | crates/ms-tuner/src/linux_dvb/state.rs:252 |
| 1422 | LOG_AND_RETURN | `Frequency %d is not within range ` | crates/ms-tuner/src/linux_dvb/state.rs:266 |
| 1444 | LOG | `tuning to %d sr:%d specinv:%s delsys:%s mod:%s ts clear = %jd` | crates/ms-tuner/src/linux_dvb/state.rs:279 |
| 1461 | LOG_AND_RETURN | `Frequency %d is not within range ` | crates/ms-tuner/src/linux_dvb/state.rs:290 |
| 1466 | LOG | `tuning to %d delsys:%s mod:%s specinv:%s ts clear = %jd` | crates/ms-tuner/src/linux_dvb/state.rs:295 |
| 1482 | LOG_AND_RETURN | `Frequency %d is not within range ` | crates/ms-tuner/src/linux_dvb/state.rs:305 |
| 1493 | LOG | `tuning to %d delsys: %s bw:%d inversion:%s , ts clear = %jd` | crates/ms-tuner/src/linux_dvb/state.rs:315 |
| 1504 | LOG | `tuning to unknown delsys: %s freq %d ts clear = %jd` | crates/ms-tuner/src/linux_dvb/state.rs:325 |
| 1527 | LOG | `dvb_tune: set property failed %d %s` | crates/ms-tuner/src/linux_dvb/state.rs:356 |
| 1531 | LOG | `dvb_tune: trying dvbapi version 3` | n/a: only the DVB-API v5 property path is implemented |
| 1533 | LOG | `dvbapi v3 ioctl failed, fd %d, errno %d (%s)` | n/a: only the DVB-API v5 property path is implemented |
| 1558 | LOG_AND_RETURN | `pid %d > 8192 for /dev/dvb/adapter%d/demux%d` | crates/ms-tuner/src/linux_dvb/backend.rs:159 |
| 1562 | LOG | `Could not open demux device /dev/dvb/adapter%d/demux%d: %s ` | crates/ms-tuner/src/linux_dvb/backend.rs:207 |
| 1578 | LOG0 | `failed setting filter on %s pid %d, errno %d (%s), enabled pids ` | crates/ms-tuner/src/linux_dvb/backend.rs:218 |
| 1585 | LOG | `AD %d [dvr %d %d], setting filter on PID %d for fd %d [%jd ms]` | crates/ms-tuner/src/linux_dvb/backend.rs:230 |
| 1598 | LOG_AND_RETURN | `DMX_STOP on an invalid handle %d, pid %d` | n/a: `DemuxFd` is typed; invalid handles cannot be passed |
| 1602 | LOG0 | `DMX_STOP failed on PID %d FD %d: error %d %s` | crates/ms-tuner/src/linux_dvb/backend.rs:180 |
| 1605 | LOG | `clearing filter on PID %d FD %d [%jd ms]` | crates/ms-tuner/src/linux_dvb/backend.rs:188 |
| 1610 | LOG | `%s: Could not find pid %d on adapter %d` | n/a: `set_pids` works on the validated `PidSet`; there is no pid-table lookup by fd |
| 1633 | LOG0 | `failed setting filter on fd %d, adapter %d, pid %d, errno %d ` | n/a: shared demux fd with `DMX_ADD_PID` is not ported (one demux fd per PID) |
| 1647 | LOG_AND_RETURN | `pid %d > 8192 for adapter %d` | n/a: same as line 1633 |
| 1654 | LOG | `AD %d [dvr %d %d], setting filter on PID %d for fd %d, active ` | n/a: same as line 1633 |
| 1663 | LOG0 | `failed to add pid %d to fd %d maximum pids %d: errno %d, %s` | n/a: same as line 1633 |
| 1669 | LOG | `AD %d [demux %d %d], recovered stale demux counter, setting ` | n/a: same as line 1633 |
| 1678 | LOG | `AD %d [demux %d %d], setting filter on PID %d for fd %d [%jd ms]` | n/a: same as line 1633 |
| 1687 | LOG_AND_RETURN | `DMX_STOP on an invalid handle %d, pid %d` | n/a: same as line 1633 |
| 1690 | LOG_AND_RETURN | `pid %d > 8192 for adapter %d` | n/a: same as line 1633 |
| 1695 | LOG0 | `failed to remove pid %d to fd %d: errno %d, %s` | n/a: same as line 1633 |
| 1704 | LOG | `DMX_STOP failed on PID %d FD %d: error %d %s` | n/a: same as line 1633 |
| 1706 | LOG | `stopped filters on fd %d` | n/a: same as line 1633 |
| 1709 | LOG | `clearing demux filter on PID %d FD %d, active_pids %d [%jd ms]` | n/a: same as line 1633 |
| 1725 | LOG | `%s: read %d, errno %d` (commented out in C++) | n/a: commented out in C++ as well |
| 1727 | LOG_AND_RETURN | `socket %d error: EOVERFLOW` | n/a: `dvb_psi_read` and its per-adapter socket are not ported; PSI is parsed by `ms-psi` |
| 1748 | LOGM | `dvb_psi_read: ...` | n/a: `dvb_psi_read` is not ported |
| 1763 | DEBUGM | `%s: socket %d, master %d, buf %p len %d, read %d` | n/a: `dvb_psi_read` is not ported |
| 1780 | LOG_AND_RETURN | `pid %d > 8192 for /dev/dvb/adapter%d/demux%d` | n/a: the PSI-filter demux socket path is not ported |
| 1784 | LOG | `Could not open demux device /dev/dvb/adapter%d/demux%d: %s ` | n/a: same as line 1780 |
| 1797 | LOG0 | `%s: failed setting filter on %s pid %d, errno %d (%s), enabled ` | n/a: same as line 1780 |
| 1805 | LOG | `AD %d [demux %d %d], setting PSI filter on PID %d for fd %d [%jd ms]` | n/a: same as line 1780 |
| 1810 | LOG | `Failed to add socket for filter pid %d, adapter %d` | n/a: same as line 1780 |
| 1820 | LOG0 | `%s: could not find pid %d` | n/a: same as line 1780 |
| 1871 | LOG | `unable to query frontend, perhaps DVB-API < 5.5 ?` | crates/ms-tuner/src/linux_dvb/enumerate.rs:132 |
| 1876 | LOG | `FE_GET_INFO failed for adapter %d, fd %d: %s ` | crates/ms-tuner/src/linux_dvb/enumerate.rs:142 |
| 1881 | LOG | `Detected adapter %d handle %d DVB Card Name: %s` | crates/ms-tuner/src/linux_dvb/enumerate.rs:151 |
| 1922 | LOG | `no available delivery system for adapter %d` | crates/ms-tuner/src/linux_dvb/enumerate.rs:162 |
| 1934 | LOG | `Detected delivery system for adapter %d: %s [%d]` | crates/ms-tuner/src/linux_dvb/enumerate.rs:174 |
| 1945 | LOG | `FE_GET_INFO failed for adapter %d, fd %d: %s ` | crates/ms-tuner/src/linux_dvb/signal.rs:316 |
| 2030 | LOGM | `adapt_signal adapter %d: strength %d (raw: %lld),snr %d (raw: %lld), dB %d (raw: %lld)` | crates/ms-tuner/src/linux_dvb/signal.rs:190 |
| 2111 | LOG | `ad %d ioctl fd %d FE_READ_STATUS failed, error %d (%s)` | crates/ms-tuner/src/linux_dvb/signal.rs:216 |
| 2131 | LOGM | `ad %d ioctl fd %d, FE_READ_BER failed, error %d (%s)` | crates/ms-tuner/src/linux_dvb/signal.rs:348 |
| 2136 | LOGM | `ad %d ioctl fd %d FE_READ_SIGNAL_STRENGTH failed, error %d ` | crates/ms-tuner/src/linux_dvb/signal.rs:362 |
| 2142 | LOGM | `ad %d ioctl fd %d FE_READ_SNR failed, error %d (%s)` | crates/ms-tuner/src/linux_dvb/signal.rs:376 |
| 2153 | LOG | `ad %d tuner '%s' uses SNR calibration %d` | crates/ms-tuner/src/linux_dvb/signal.rs:390 |
| 2159 | LOGM | `get_signal adapter %d: SNR calibrated %d -> %d` | crates/ms-tuner/src/linux_dvb/signal.rs:400 |
| 2169 | LOG | `ad %d reports no signal strength while locked, ` | crates/ms-tuner/src/linux_dvb/signal.rs:414 |
| 2180 | LOGM | `get_signal adapter %d: status %d, strength %d, snr %d, BER: %d` | crates/ms-tuner/src/linux_dvb/signal.rs:425 |
| 2208 | LOGM | `get_signal_new: unable to query frontend %d: %s` | crates/ms-tuner/src/linux_dvb/signal.rs:242 |
| 2238 | LOGM | `get_signal_new adapter %d: status %d, strength %jd %s -> %jd, snr ` | crates/ms-tuner/src/linux_dvb/signal.rs:291 |
| 2301 | LOG | `ad %d has no lock, resending the switch commands (retry %d)` | crates/ms-tuner/src/linux_dvb/state.rs:510 |
| 2331 | LOGM | `Signal metrics are not retrieved as both multipliers are 0` | crates/ms-tuner/src/linux_dvb/state.rs:422 |
| 2380 | LOG | `DVB device detection disabled` | crates/minisatip/src/runtime.rs:1503 |
| 2402 | LOG | `%s device found, this is a DDCI device` | crates/ms-tuner/src/linux_dvb/enumerate.rs:91 |
| 2412 | LOGM | `%s: adding %d %d to the list of devices` | crates/ms-tuner/src/linux_dvb/enumerate.rs:108 |

ported 88 / not applicable 34
