---
name: Bad routing result
about: The router or placer produced a wrong, ugly or slow result
title: "[bad result] "
labels: bad-result
---

**What was bad**
(opens, DRC error, ugly geometry, too slow, crash; one sentence)

**Command and version**
```
copperpath --version
copperpath route --pcb ... (full command line)
```

**Minimal synthetic repro board**
Attach a small made-up board (`.kicad_pcb` + `.kicad_pro` + `.kicad_dru`) that shows the problem.
Never attach proprietary board data (see CONTRIBUTING.md).

**Expected**

**Actual**
(screenshot or coordinates, e.g. "net X at 143.1, 116.1 on F.Cu")

**Metrics JSON**
```json
(output of the command with --json, or the <out>_route.json file)
```
