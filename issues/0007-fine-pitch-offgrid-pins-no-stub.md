# 0007 Fine-pitch pins off the 0.05 mm grid get no legal stub

GitHub: https://github.com/NathansLab/copperpath/issues/1
Status: open

**What was bad**
On a 48-pin package with 0.4 mm pitch and 0.2 mm wide pads (default class: 0.2 mm clearance,
0.2 mm neck-down) 43 of 48 pins got no legal stub, so 33 connections stayed open. The pad centres
sit 0.0164 mm off the 0.05 mm routing grid; a 0.2 mm track between 0.2 mm pads at 0.4 mm pitch
fits only exactly on the pad axis, never at the nearest cell centre.

**Command and version**
```
copperpath 0.1.0 (84c9889f) and unreleased fixes
copperpath route --pcb bm5.unrouted.kicad_pcb --out out.kicad_pcb --threads 4 --drc --json
```

**Minimal synthetic repro board**
TODO: synthetic QFN-48 (0.4 mm pitch, 0.8 x 0.2 mm pads) placed at an offset of 0.0164 mm from the
grid, one track per pin to a header. Found on the public DAC2020 benchmark bm05 (Freerouting
fixtures, `Issue508-DAC2020`).

**Expected**
All pins reachable (the human reference routes them with 0.2 mm tracks).

**Actual**
`[aes] pins 154 without legal stub 43`, `opens: 33`.

**Ideas**
Exact pad-axis stubs that end on the first legal cell, or a per-net grid offset so that the
dominant pin row lies on cell centres.
