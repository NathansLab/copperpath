# 0010 `remove_unused_layers` of a plated pad is ignored

Status: fixed (unreleased): on inner layers only the plated barrel (drill diameter) is an obstacle

**What was bad**
A mounting hole pad (8.6 mm pad, 4.3 mm hole, `(remove_unused_layers yes) (keep_end_layers yes)`)
was treated as an 8.6 mm pad on every inner layer and closed a routing channel next to it.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb
```

**Minimal synthetic repro board**
4-layer board, one such pad; unit test
`model::tests::removed_unused_layers_leave_only_the_barrel_inside`.

**Expected**
Inner layers: only the barrel (hole clearance / copper clearance to the barrel).

**Actual**
Full pad on In1..In4.

**Limitation**
A track of the pad's own net that connects on an inner layer makes KiCad flash the pad there
again; copperpath does not check that case.
