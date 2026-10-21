# 0005 Neck-down ignores the board setup minimum track width

Status: fixed (unreleased): the neck-down width is never below `min_track_width`

**What was bad**
Near pads the router narrows tracks to 0.2 mm (fixed value). Boards with a larger minimum track
width in Board Setup (e.g. 0.25 mm) got KiCad `track_width` errors.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --drc --json
```

**Minimal synthetic repro board**
Any board with `"min_track_width": 0.25` in the project rules; unit test
`rules::tests::without_project_file_default_class_and_board_min_width`.
Seen on two public PCBench boards (USB-serial adapter, AVR ISP level shifter).

**Expected**
No new DRC errors.

**Actual**
`error track_width | Track width (board setup constraints min width 0.2500 mm; actual 0.2000 mm)`,
25 and 7 new errors.
