# 0003 Missing .kicad_pro aborts the run

Status: fixed (unreleased): without a project file the KiCad default net class is used

**What was bad**
A board without a `.kicad_pro` next to it failed with
`project file not found (net classes are read from it; set --pro)`. KiCad itself opens such
boards with its default net class.

**Command and version**
```
copperpath 0.1.0 (84c9889f)
copperpath route --pcb board.kicad_pcb --out out.kicad_pcb --json
```

**Minimal synthetic repro board**
Any board file copied without its project file; unit test
`rules::tests::without_project_file_default_class_and_board_min_width`.

**Expected**
Warning, then routing with the default class (clearance 0.2, track 0.25, via 0.6/0.3).

**Actual**
Exit code 1.
