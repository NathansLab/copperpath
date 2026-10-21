# 0001 Tiny junction leg gives a KiCad track_dangling warning

Status: fixed in 0.1.0 (tiny T-junction legs are removed before the exact verification)

**What was bad**
`route` on the demo left a 0.07 mm diagonal segment between a junction in the middle of a track
and a neighbouring segment. KiCad DRC: 1 new `track_dangling` warning, 0 errors.

**Command and version**
```
placeroute route --pcb examples/demo/demo.kicad_pcb --out out/demo_routed.kicad_pcb --drc --json
```

**Minimal synthetic repro board**
`examples/demo/demo.kicad_pcb` (net +5V near 143.1, 116.1 on F.Cu).

**Expected**
No new DRC violations.

**Actual**
`warning track_dangling | Track [+5V] on F.Cu, length 0.0707 mm`

**Metrics JSON**
before the fix: `"drc": {"new_errors": 0, "new_violations": 1}`; after: `"new_violations": 0`.
