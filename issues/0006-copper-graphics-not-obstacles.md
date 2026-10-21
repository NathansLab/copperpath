# 0006 Copper graphics, copper text and footprint cut-outs are not obstacles

Status: fixed (unreleased): net-less copper graphics/text become locked obstacles; Edge.Cuts
drawn inside footprints are added to the board outline

**What was bad**
Tracks were routed through (1) `gr_text` on F.Cu, (2) copper `fp_poly` inside a footprint
(antenna/logo copper without a net) and (3) along Edge.Cuts arcs drawn inside a footprint
(slot/cut-out), giving KiCad `clearance`, `shorting_items` and `copper_edge_clearance` errors.
Net-less copper was also exempted as a "same-class pour".

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --drc --json
```

**Minimal synthetic repro board**
Unit test `board::tests::copper_graphics_text_and_footprint_cutouts` (text on F.Cu, line on B.Cu,
filled copper polygon and Edge.Cuts line in a rotated footprint). Seen on DAC2020 benchmark bm06
(18 new errors) and a public PCBench backplane with a copper title text (1 error).

**Expected**
No new DRC errors.

**Actual**
`error shorting_items | Polygon [<no net>] of U12 on Top / Track [GND]`,
`error clearance | PCB text '...' on F.Cu / Track [...]`.
