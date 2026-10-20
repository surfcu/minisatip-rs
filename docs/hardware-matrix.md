# Hardware validation matrix

Copied from `PLAN.md` §8 with a notes column. Rows that are not validated on real
hardware must be called out in release notes.

| Feature | Validated on | How | Notes |
|---|---|---|---|
| Linux DVB, 8 tuners | user's box | real | Main M4/M9 target; all 8 adapters must be exercised on the box. |
| DDCI, 1 CAM | user's box | real | Real CAM required; fake CAM covers sequencing only. |
| Multi-CAM | — | fake CAM only | `ms-fakecam` provides N instances from M1 onwards. |
| DVBAPI / oscam | user's box | real | Also diffed against a fake oscam socket. |
| Unicable/JESS | — | tests + external testers | Needs external hardware to validate. |
| CI+ | — | external testers | Feature-gated and experimental until validated. |
| SAT>IP client | TBD | real server if available | Backend lands in M8. |
| Netceiver, hw descrambler, Enigma2 | — | tests only | Unvalidated; must be flagged in release notes. |
