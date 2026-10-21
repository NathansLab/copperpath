# 0018 New vias violate hole clearance to NPTH mounting holes

Status: open

**What was bad**
Routed vias are placed closer than `min_hole_clearance` to the drill of an NPTH pad
(connector mounting peg). KiCad: `hole_clearance | NPTH pad of P1 / Via [...]`, 3 errors.

**Command and version**
```
copperpath 0.1.0 (936f189)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --threads 4 --drc --json
```
Seen on the public kitspace TS100C board (USB connector with NPTH pegs).

**Minimal synthetic repro board**
TODO: NPTH pad `np_thru_hole circle (size 0.65 0.65) (drill 0.65)` without copper layers except
`*.Mask`, board `min_hole_clearance 0.25`; two pads of net A on either side forcing a via nearby.

**Expected / Actual**
No new DRC errors / 3 `hole_clearance` errors.

**Ideas**
NPTH pads with no copper layers (`layers == 0` after masking) may never become objects, so
`hole_r` is not seen by `via_map`/`via_ok_fixed`. Add a hole-only object (all layers, net 0, shape =
drill circle, clearance = hole clearance only).
