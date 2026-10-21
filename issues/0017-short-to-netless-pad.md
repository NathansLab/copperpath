# 0017 Track shorts a net-less SMD pad

GitHub: https://github.com/NathansLab/copperpath/issues/4
Status: open

**What was bad**
An 0402 resistor has pad 1 without a net and pad 2 on net A. The routed track of net A runs over
pad 1 (KiCad: `shorting_items` + `solder_mask_bridge`, Pad 1 [<no net>]). Net-less pads should be
obstacles for every net.

**Command and version**
```
copperpath 0.1.0 (936f189)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --threads 4 --drc --json
```
Seen on the public kitspace TS100C board (KiCad 6+ copy, rotated footprint `(at x y -90)`, pads
`(at -0.485 0 270)` / `(at 0.485 0 270)`).

**Minimal synthetic repro board**
TODO: 0402 footprint rotated -90 deg, pad 1 no net, pad 2 net A; a second pad of net A placed so
that the straight path to pad 2 crosses pad 1.

**Expected / Actual**
No new DRC errors / 1 `shorting_items`, 1 `solder_mask_bridge`.

**Ideas**
Check the pad-entry/stub code path and the final cleanup: the negotiation maps treat net-less pads
as obstacles (`Model::clr` returns >= 0 for net 0), so the short is probably introduced after
routing (stub, neck-down or merge step) or by the same-footprint exemption.
