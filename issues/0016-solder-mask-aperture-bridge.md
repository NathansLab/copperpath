# 0016 Tracks run through solder mask apertures of other nets (solder_mask_bridge)

GitHub: https://github.com/NathansLab/copperpath/issues/3
Status: open

**What was bad**
A solder jumper footprint has a mask opening drawn as a polygon on F.Mask that is larger than its
pads. Foreign tracks routed through that opening are exposed and KiCad reports
`solder_mask_bridge` (4 errors on DAC2020 benchmark bm02).

**Command and version**
```
copperpath 0.1.0 (5dcd97ac + unreleased)
copperpath route --pcb bm2.unrouted.kicad_pcb --out out.kicad_pcb --drc --json
```

**Minimal synthetic repro board**
TODO: footprint with two 0.5 x 1 mm SMD pads (nets A, B) and an `fp_poly` on F.Mask of 2 x 2 mm
around them; a track of net C whose straight path crosses the polygon.

**Expected**
No new DRC errors: tracks of nets not present in the aperture stay out of it (same for pad mask
expansions on F.Mask/B.Mask).

**Actual**
`error solder_mask_bridge | Front solder mask aperture bridges items with different nets`.

**Ideas**
Treat footprint mask graphics (and pad mask apertures) as obstacles on the matching outer copper
layer for nets other than the nets of the pads inside the aperture (clearance 0 to the aperture).
