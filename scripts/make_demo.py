#!/usr/bin/env python3
"""Generate the synthetic demo board in examples/demo/ with KiCad's pcbnew module.

usage: <KiCad python> scripts/make_demo.py [OUT_DIR]      (default: examples/demo)

Requires KiCad 10 (its bundled Python with the `pcbnew` module) and the KiCad standard footprint
library. The library is found through $KICAD10_FOOTPRINT_DIR, else in the usual install locations.
The script only creates and saves the board (no zone fill, no DRC); the project file
(net classes) and the custom rules (.kicad_dru) are written as plain text. Run DRC with kicad-cli.

The board is a small, made-up 4-layer layout: a controller, a driver, a regulator, two small
MOSFETs, passives, two pin headers, four mounting holes and one rule area (no tracks, no vias).
"""
import json
import os
import sys
from pathlib import Path

import pcbnew

OUT = Path(sys.argv[1] if len(sys.argv) > 1 else Path(__file__).resolve().parent.parent / 'examples/demo')
NAME = 'demo'


def footprint_dir():
    cands = [os.environ.get('KICAD10_FOOTPRINT_DIR', ''),
             '/Applications/KiCad/KiCad.app/Contents/SharedSupport/footprints',
             '/usr/share/kicad/footprints', '/usr/local/share/kicad/footprints',
             r'C:\Program Files\KiCad\10.0\share\kicad\footprints']
    for c in cands:
        if c and Path(c, 'Resistor_SMD.pretty').is_dir():
            return Path(c)
    sys.exit('KiCad footprint library not found: set KICAD10_FOOTPRINT_DIR')


LIB = footprint_dir()
O = (100.0, 100.0)  # board origin (mm)
W, H = 70.0, 50.0   # board size (mm)

# ref: (library, footprint, value, x, y, rotation, sheet, {pad: net})
PARTS = {
    'J1': ('Connector_PinHeader_2.54mm', 'PinHeader_1x06_P2.54mm_Vertical', 'Header 1x06', 5, 12, 0, 'IO',
           {'1': 'VIN', '2': 'GND', '3': 'SDA', '4': 'SCL', '5': 'EN', '6': 'FAULT'}),
    'J2': ('Connector_PinHeader_2.54mm', 'PinHeader_1x04_P2.54mm_Vertical', 'Header 1x04', 65, 17, 0, 'IO',
           {'1': 'OUT_A', '2': 'OUT_B', '3': 'GND', '4': 'SENSE'}),
    'U1': ('Package_SO', 'SOIC-14_3.9x8.7mm_P1.27mm', 'Controller', 25, 20, 0, 'Control',
           {'1': '+3V3', '2': 'SDA', '3': 'SCL', '4': 'EN', '5': 'FAULT', '6': 'PWM_A', '7': 'GND',
            '8': 'PWM_B', '9': 'ADC1', '10': 'ADC2', '11': 'LED', '12': 'NRST', '13': 'CLK', '14': 'VREF'}),
    'U2': ('Package_SO', 'SOIC-8_3.9x4.9mm_P1.27mm', 'Driver', 44, 18, 0, 'Driver',
           {'1': '+5V', '2': 'PWM_A', '3': 'PWM_B', '4': 'GND', '5': 'DRV_B', '6': 'SW', '7': 'DRV_A', '8': '+5V'}),
    'U3': ('Package_SO', 'SOIC-8_3.9x4.9mm_P1.27mm', 'Regulator', 18, 38, 90, 'Supply',
           {'1': 'VIN', '2': 'GND', '3': '+5V', '4': '+3V3', '5': 'CLK', '6': 'LED', '7': 'SW', '8': 'GND'}),
    'Q1': ('Package_TO_SOT_SMD', 'SOT-23', 'NMOS', 56, 12, 0, 'Driver', {'1': 'DRV_A_R', '2': 'GND', '3': 'OUT_A'}),
    'Q2': ('Package_TO_SOT_SMD', 'SOT-23', 'NMOS', 56, 25, 0, 'Driver', {'1': 'DRV_B_R', '2': 'GND', '3': 'OUT_B'}),
    'R1': ('Resistor_SMD', 'R_0603_1608Metric', '10R', 51, 12, 0, 'Driver', {'1': 'DRV_A', '2': 'DRV_A_R'}),
    'R2': ('Resistor_SMD', 'R_0603_1608Metric', '10R', 51, 25, 0, 'Driver', {'1': 'DRV_B', '2': 'DRV_B_R'}),
    'R3': ('Resistor_SMD', 'R_0603_1608Metric', '10k', 40, 30, 90, 'Control', {'1': 'SENSE', '2': 'ADC1'}),
    'R4': ('Resistor_SMD', 'R_0603_1608Metric', '1k', 37, 30, 90, 'Control', {'1': 'ADC1', '2': 'GND'}),
    'R5': ('Resistor_SMD', 'R_0603_1608Metric', '10k', 31, 30, 90, 'Control', {'1': 'VREF', '2': 'ADC2'}),
    'R6': ('Resistor_SMD', 'R_0603_1608Metric', '10k', 28, 30, 90, 'Control', {'1': 'ADC2', '2': 'GND'}),
    'R7': ('Resistor_SMD', 'R_0603_1608Metric', '10k', 25, 10, 0, 'Control', {'1': '+3V3', '2': 'NRST'}),
    'R8': ('Resistor_SMD', 'R_0603_1608Metric', '10k', 13, 22, 90, 'IO', {'1': 'EN', '2': '+3V3'}),
    'C1': ('Capacitor_SMD', 'C_0805_2012Metric', '10u', 12, 38, 90, 'Supply', {'1': 'VIN', '2': 'GND'}),
    'C2': ('Capacitor_SMD', 'C_0805_2012Metric', '10u', 24, 38, 90, 'Supply', {'1': '+5V', '2': 'GND'}),
    'C3': ('Capacitor_SMD', 'C_0805_2012Metric', '1u', 29, 38, 90, 'Supply', {'1': '+3V3', '2': 'GND'}),
    'C4': ('Capacitor_SMD', 'C_0805_2012Metric', '100n', 19, 20, 90, 'Control', {'1': '+3V3', '2': 'GND'}),
    'C5': ('Capacitor_SMD', 'C_0805_2012Metric', '100n', 44, 11, 0, 'Driver', {'1': '+5V', '2': 'GND'}),
    'C6': ('Capacitor_SMD', 'C_0805_2012Metric', '100n', 34, 22, 90, 'Control', {'1': 'VREF', '2': 'GND'}),
    'MH1': ('MountingHole', 'MountingHole_3.2mm_M3', 'M3', 4, 4, 0, '', {}),
    'MH2': ('MountingHole', 'MountingHole_3.2mm_M3', 'M3', 66, 4, 0, '', {}),
    'MH3': ('MountingHole', 'MountingHole_3.2mm_M3', 'M3', 4, 46, 0, '', {}),
    'MH4': ('MountingHole', 'MountingHole_3.2mm_M3', 'M3', 66, 46, 0, '', {}),
}
RULE_AREA = [(42, 36), (54, 36), (54, 46), (42, 46)]  # no tracks, no vias (board-relative mm)

PRO = {
    'board': {'design_settings': {
        'defaults': {'board_outline_line_width': 0.1, 'copper_line_width': 0.2},
        'rules': {'min_clearance': 0.15, 'min_copper_edge_clearance': 0.5, 'min_hole_clearance': 0.25,
                  'min_hole_to_hole': 0.25, 'min_track_width': 0.15, 'min_via_annular_width': 0.1,
                  'min_via_diameter': 0.5, 'min_through_hole_diameter': 0.3},
        'track_widths': [], 'via_dimensions': []}},
    'meta': {'filename': f'{NAME}.kicad_pro', 'version': 3},
    'net_settings': {
        'classes': [
            {'name': 'Default', 'clearance': 0.2, 'track_width': 0.25, 'via_diameter': 0.6, 'via_drill': 0.3,
             'microvia_diameter': 0.3, 'microvia_drill': 0.1, 'diff_pair_gap': 0.25, 'diff_pair_width': 0.2,
             'line_style': 0, 'pcb_color': 'rgba(0, 0, 0, 0.000)', 'schematic_color': 'rgba(0, 0, 0, 0.000)',
             'wire_width': 6, 'bus_width': 12, 'priority': 2147483647},
            {'name': 'Power', 'clearance': 0.2, 'track_width': 0.4, 'via_diameter': 0.8, 'via_drill': 0.4,
             'microvia_diameter': 0.3, 'microvia_drill': 0.1, 'diff_pair_gap': 0.25, 'diff_pair_width': 0.2,
             'line_style': 0, 'pcb_color': 'rgba(0, 0, 0, 0.000)', 'schematic_color': 'rgba(0, 0, 0, 0.000)',
             'wire_width': 6, 'bus_width': 12, 'priority': 0},
        ],
        'meta': {'version': 4},
        'netclass_assignments': None,
        'netclass_patterns': [{'netclass': 'Power', 'pattern': 'GND'}, {'netclass': 'Power', 'pattern': 'VIN'},
                              {'netclass': 'Power', 'pattern': '+5V'}],
    },
}

DRU = """(version 1)
# Custom rules of the demo board (read by KiCad DRC and by placeroute).

# power nets keep more distance to everything else on the outer layers
(rule "power_outer_clearance"
  (layer outer)
  (condition "A.NetClass == 'Power' && B.NetClass != 'Power'")
  (constraint clearance (min 0.3mm)))

# power tracks: 0.4 mm, neck-down to 0.25 mm at fine-pitch pins
(rule "power_track_width"
  (condition "A.NetClass == 'Power'")
  (constraint track_width (min 0.25mm) (opt 0.4mm) (max 2mm)))

# copper keeps 0.5 mm from the mounting-hole drills
(rule "mounting_hole_clearance"
  (condition "A.memberOfFootprint('MH*')")
  (constraint hole_clearance (min 0.5mm)))
"""


def mm(x, y):
    return pcbnew.VECTOR2I(pcbnew.FromMM(O[0] + x), pcbnew.FromMM(O[1] + y))


def main():
    OUT.mkdir(parents=True, exist_ok=True)
    b = pcbnew.BOARD()
    b.SetCopperLayerCount(4)
    nets = {}
    for _, _, _, _, _, _, _, pads in PARTS.values():
        for n in pads.values():
            if n not in nets:
                ni = pcbnew.NETINFO_ITEM(b, n)
                b.Add(ni)
                nets[n] = ni
    for ref, (lib, name, val, x, y, rot, sheet, pads) in PARTS.items():
        fp = pcbnew.FootprintLoad(str(LIB / f'{lib}.pretty'), name)
        if fp is None:
            sys.exit(f'footprint {lib}:{name} not found')
        fp.SetReference(ref)
        fp.SetValue(val)
        fp.SetFPID(pcbnew.LIB_ID(lib, name))
        b.Add(fp)
        fp.SetPosition(mm(x, y))
        fp.SetOrientationDegrees(rot)
        if sheet:
            fp.SetSheetname(sheet)
        else:
            fp.Reference().SetVisible(False)  # mounting holes: no reference text near the edge
        for pd in fp.Pads():
            n = pads.get(pd.GetNumber())
            if n:
                pd.SetNet(nets[n])
    edge = pcbnew.PCB_SHAPE(b)
    edge.SetShape(pcbnew.SHAPE_T_RECT)
    edge.SetStart(mm(0, 0))
    edge.SetEnd(mm(W, H))
    edge.SetLayer(pcbnew.Edge_Cuts)
    edge.SetWidth(pcbnew.FromMM(0.1))
    b.Add(edge)
    z = pcbnew.ZONE(b)
    z.SetIsRuleArea(True)
    z.SetDoNotAllowTracks(True)
    z.SetDoNotAllowVias(True)
    z.SetDoNotAllowZoneFills(True)
    z.SetDoNotAllowPads(False)
    z.SetDoNotAllowFootprints(False)
    z.SetLayerSet(pcbnew.LSET.AllCuMask())
    z.SetZoneName('no_routing')
    ol = z.Outline()
    ol.NewOutline()
    for x, y in RULE_AREA:
        p = mm(x, y)
        ol.Append(p.x, p.y)
    b.Add(z)
    pcb = OUT / f'{NAME}.kicad_pcb'
    pcbnew.SaveBoard(str(pcb), b)
    (OUT / f'{NAME}.kicad_pro').write_text(json.dumps(PRO, indent=2) + '\n')
    (OUT / f'{NAME}.kicad_dru').write_text(DRU)
    print(f'wrote {pcb}: {len(PARTS)} footprints, {len(nets)} nets')


main()
