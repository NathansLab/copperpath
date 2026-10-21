# 0013 Vias smaller than the Board Setup minimums

Status: fixed (unreleased): via diameter/drill are raised to `min_via_diameter`,
`min_through_hole_diameter` and drill + 2 x `min_via_annular_width`

**What was bad**
Net class via 0.4 / 0.25 mm (annular 0.075 mm) on a board with a 0.1 mm minimum annular width:
46 new `annular_width` errors.

**Minimal synthetic repro board**
Unit test `rules::tests::without_project_file_default_class_and_board_min_width`
(class via 0.6 / 0.3, annular minimum 0.2 -> 0.7 / 0.3). Seen on a public PCBench iCE40 board.

**Expected / Actual**
0 new DRC errors / 46 `annular_width` errors.
