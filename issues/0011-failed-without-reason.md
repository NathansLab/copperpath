# 0011 FAILED gives no reason; fixed node cap too small for long connections

Status: fixed (unreleased): every failed net reports why its last search stopped; the node cap is
at least half the nodes of the net's job window

**What was bad**
`FAILED <net> (1 components left)` after 8 M nodes, every time, without a reason. A 174 mm
connection needed 5 M nodes in one search (default cap 4 M). Another connection failed because its
only path ran outside the job window (`--job-margin`, default 6 mm); the log did not say so.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --region ... --out out.kicad_pcb
```

**Minimal synthetic repro board**
Two pads 170 mm apart on a long narrow board, or a detour around a wall that leaves the pin bounding
box by more than 6 mm.

**Expected**
`node cap hit (raise --max-expand)`, `job window exhausted (raise --job-margin)` or
`no path (target unreachable)` in the log and in the JSON warnings; a cap that scales with the
window.

**Actual**
No reason; fixed cap.
