# 0012 DRU `intersectsCourtyard` relaxations are not applied

Status: open

**What was bad**
A later DRU rule that relaxes a large outer-layer clearance inside a part's courtyard
(`A.intersectsCourtyard('U1')`) is ignored, so the stricter rule keeps all of that part's pins
without a legal stub (15 of 32 pins on one board).

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb
```

**Minimal synthetic repro board**
TODO: SOIC-16 with net class HV on pins 1-8 and LV on pins 9-16; rule 1: HV-LV 8 mm on outer
layers; rule 2 (later): HV-LV 0.5 mm when `A.intersectsCourtyard('U1')`.

**Expected**
KiCad semantics: the last matching rule wins, including position-dependent conditions.

**Actual**
`intersectsCourtyard` evaluates to "unknown", the rule never applies.

**Design notes**
Clearances are a net-pair matrix today. Needed: per-rule courtyard polygons with an override
value, used (1) when stamping obstacles into the track/via maps (cells inside the courtyard get the
relaxed distance), (2) in the exact checker and (3) in the cleanup checks. KiCad applies the rule to
the whole item if it intersects the courtyard; a conservative first step is to relax only stubs
and cells inside the courtyard.
