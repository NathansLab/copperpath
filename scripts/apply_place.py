#!/usr/bin/env python3
"""Apply a `placeroute place` result (place.json) to a copy of a board.

usage: <KiCad python> scripts/apply_place.py IN.kicad_pcb place.json OUT.kicad_pcb

Requires KiCad 10's bundled Python (module `pcbnew`). Sets position, rotation and side of each
listed footprint, saves OUT and copies the .kicad_pro/.kicad_dru next to it. IN is not modified.
"""
import json
import shutil
import sys

import pcbnew

if len(sys.argv) != 4:
    sys.exit(__doc__)
src, pj, out = sys.argv[1:4]
b = pcbnew.LoadBoard(src); fp = {f.GetReference(): f for f in b.GetFootprints()}
for c in json.load(open(pj)):
    f = fp[c['ref']]
    if (c['side'] == 'B') != (f.GetLayer() == pcbnew.B_Cu):
        f.Flip(f.GetPosition(), pcbnew.FLIP_DIRECTION_TOP_BOTTOM)
    f.SetPosition(pcbnew.VECTOR2I(int(round(c['x'] * 1e6)), int(round(c['y'] * 1e6))))
    f.SetOrientationDegrees(c['rot'])
pcbnew.SaveBoard(out, b)
for s in ('.kicad_pro', '.kicad_dru'):
    try: shutil.copy(src.replace('.kicad_pcb', s), out.replace('.kicad_pcb', s))
    except Exception: pass
print('APPLIED', len(json.load(open(pj))))
