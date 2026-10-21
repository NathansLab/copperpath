# 0020 Low completion and many vias on dense two-layer boards

GitHub: https://github.com/NathansLab/copperpath/issues/7
Status: open

**What was bad**
A public two-layer adapter board (PCBench gdrom adapter, 40 nets, 109 connections, two 50-pin
headers with 2.54 mm pitch plus an IDE connector) ends with 30 opens and 117 vias after 1018 s
(4 threads). The human reference completes all with 27 vias.

**Command and version**
```
copperpath 0.1.0 (936f189)
copperpath route --pcb gdrom.unrouted.kicad_pcb --out out.kicad_pcb --threads 4 --drc --json
```
Reasons printed: mostly `job window exhausted` and `no path`.

**Minimal synthetic repro board**
TODO: two 2x20 headers 30 mm apart on two layers, nets connecting pin i to pin (40-i)
(crossing bus), default class 0.25/0.2.

**Expected**
Complete routing (a crossing bus on two layers is solvable with < 1 via per net).

**Ideas**
- Escalate the job window automatically for nets that fail with `job window exhausted`.
- Via cost increase when the board has only two layers; layer-direction preference (H on F.Cu,
  V on B.Cu) for bus-like nets.
- Rip-up order: route long bus nets first.
