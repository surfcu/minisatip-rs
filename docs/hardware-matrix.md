# Hardware validation matrix

Copied from `PLAN.md` §8 with a notes column. Rows that are not validated on real
hardware must be called out in release notes.

| Feature | Validated on | How | Notes |
|---|---|---|---|
| Linux DVB, 8 tuners | user's box | real | FTA through tvheadend works (2026-10-22); all 8 adapters still to be exercised. |
| DDCI, 1 CAM | user's box | real | Irdeto CAM on a DuoFlex CI (adapter 8), `-c 8:**2`: 3 channels through tvheadend without glitches, rejoin without partner glitches (2026-10-22). |
| Multi-CAM | — | fake CAM only | `ms-fakecam` provides N instances from M1 onwards. |
| DVBAPI / oscam | — | fake oscam only | No oscam on the box; diffed against a fake oscam socket only. |
| Unicable/JESS | — | tests + external testers | Needs external hardware to validate. |
| CI+ | — | external testers | Feature-gated and experimental until validated. |
| SAT>IP client | TBD | real server if available | Backend lands in M8. |
| Netceiver, hw descrambler, Enigma2 | — | tests only | Unvalidated; must be flagged in release notes. Netceiver is parked (PLAN §3) and not part of the binary. |
