# 0008 Unlocked copper of other nets is not an obstacle (real clearance violations)

Status: fixed (unreleased): `route` treats all existing copper as obstacle/connectivity;
`--ignore-unlocked` restores the old behaviour

**What was bad**
Unlocked tracks and vias already in the board (e.g. written by another tool or an earlier
routing step) were ignored during routing but kept in the output. New tracks ran through them:
clearance errors, shorts and crossings in KiCad DRC.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --region ... --out out.kicad_pcb --drc --json
```

**Minimal synthetic repro board**
One unlocked track of net HV across the straight path between two pads of net LV; unit test
`model::tests::unlocked_copper_of_other_nets_is_an_obstacle`.

**Expected**
All copper of other nets blocks, locked or not.

**Actual**
Dozens of new `clearance` and `shorting_items` errors on a mixed HV/LV board.
