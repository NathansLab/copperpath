# 0002 KiCad 5 board file is read as an empty board

Status: fixed (unreleased): boards older than format 20211014 are refused with a clear error

**What was bad**
`route` on a KiCad 5 file (`(version 20171130)`, `module` instead of `footprint`) loaded
0 footprints, routed 0 nets and reported `ok: true`.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb unrouted.kicad_pcb --out out.kicad_pcb --json
```

**Minimal synthetic repro board**
`(kicad_pcb (version 20171130) (module R1))`; unit test `board::tests::rejects_kicad5_boards`.
Seen on every board of the public PCBench set (Freerouting benchmark fixtures), which are stored
in KiCad 4/5 format.

**Expected**
An error that says to open and save the board in KiCad 6 or newer.

**Actual**
`[load] 0 footprints, 0 tracks, 0 vias, 0 zones, 1 nets`, exit code 0.
