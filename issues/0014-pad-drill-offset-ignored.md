# 0014 Pad drill offset is ignored

Status: fixed (unreleased): the copper shape is shifted by `(drill d (offset x y))`

**What was bad**
Oval THT pads with `(drill 1 (offset 0 0.25))` (box header footprint) were modelled centred on the
hole; their copper really sits 0.25 mm off. Tracks passed too close: 27 `clearance` errors.

**Minimal synthetic repro board**
Unit test `board::tests::drill_offset_shifts_the_pad_copper`. Seen on a public PCBench adapter board.

**Expected / Actual**
0 new DRC errors / 27 `clearance` errors (pad to track 0.08-0.18 mm instead of 0.254 mm).
