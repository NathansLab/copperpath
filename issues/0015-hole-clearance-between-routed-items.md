# 0015 Hole clearance not kept between newly routed vias and tracks

Status: fixed (unreleased): routed vias keep `min_hole_clearance` to routed tracks of other nets
(negotiation maps and exact checks)

**What was bad**
With a net class clearance (0.13 mm) smaller than the Board Setup hole clearance (0.25 mm), a new
track passed 0.21 mm from the drill of a new via of another net. The hole clearance was only
checked against existing (fixed) holes.

**Minimal synthetic repro board**
Two nets of a class with 0.13 mm clearance, `min_hole_clearance` 0.25 mm, both forced to cross
with a via. Seen on a public PCBench iCE40 board (3 `hole_clearance` errors).

**Expected / Actual**
0 new DRC errors / 3 `hole_clearance` errors.
