# 0021 Feature request: --time-limit, SIGTERM and stall handling write the best result so far

GitHub: https://github.com/NathansLab/copperpath/issues/8
Status: open (feature request)

**Problem**
On dense boards the negotiation can plateau (a few hundred conflict cells, 3-17 nets in conflict)
for many iterations of 1-2 min each. A run killed by a watchdog writes nothing, and the log does not
say where the congestion is.

**Proposal**
1. `--time-limit S`: when the next iteration would exceed the limit, stop negotiating and run the
   existing resolve step (rip the most conflicted nets, reroute hard) on the BEST iteration so far
   (fewest conflict cells), then write the board as usual.
2. SIGTERM / SIGINT: same as the time limit (finish the current net, resolve, write). No new
   dependency needed: a `static AtomicBool` set from a `libc::signal` handler, or `signal-hook`.
3. Stall detection: configurable window (`--stall-iters N`, default 3 instead of the fixed 6).
4. Congestion report in `--json`: `"conflicts": [{"nets": ["A","B"], "cells": 42,
   "at": [x, y], "layer": "In1.Cu"}]` from the last negotiation iteration (top 20 hotspots,
   clustered), and the same as `[droute] hotspot ...` log lines.

**Acceptance**
- Unit/integration test: a synthetic board with an unsolvable channel (3 nets through a gap that
  fits 2) and `--time-limit 5` writes an output with 1 failed net and a `conflicts` entry naming
  the gap location.
- `kill -TERM` during iteration 2 of the same board still writes the output file.

**Notes**
`Router::run` in `src/droute.rs` holds `routed`, `cells`, `vias`, `left` per job; snapshot them when
`total` reaches a new minimum. Coordinate with the maintainer: the iteration loop is being changed
to route non-overlapping nets in parallel waves (same function).
