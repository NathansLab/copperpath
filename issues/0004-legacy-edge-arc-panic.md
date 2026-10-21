# 0004 Panic on Edge.Cuts arcs in the legacy centre/angle form

Status: fixed (unreleased): legacy arcs are read, missing coordinates no longer panic

**What was bad**
`gr_arc` on Edge.Cuts written as `(start centre) (end arc-start) (angle deg)` (no `mid`)
crashed the loader: `called Option::unwrap() on a None value` at `src/board.rs:560`.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --json
```

**Minimal synthetic repro board**
`(gr_arc (start 10 10) (end 12 10) (angle 90) (layer "Edge.Cuts") (width 0.1))`; unit test
`board::tests::legacy_edge_arc_is_read`.

**Expected**
The arc becomes part of the board outline.

**Actual**
Exit code 101 (panic).
