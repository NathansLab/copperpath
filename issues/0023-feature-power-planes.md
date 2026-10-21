# 0023 Feature request: power planning (pours, plane splits, stitching, power-aware routing)

GitHub: https://github.com/NathansLab/copperpath/issues/10
Status: open (feature request). Related: #6 (wide power tracks).

Seen on public benchmark boards: humans route power with wide tracks or pours and put far fewer
vias on power nets; copperpath routes power like signals at class width.

## Proposals
1. **Power net detection** by net class, name pattern (`GND`, `+*V*`, `VBAT*`, `VCC*`) or pad
   count; config `power_nets`.
2. **Plane assignment**
   - Option to dedicate an inner layer to one net (GND) or to split it between nets
     (`planes: [{"layer":"In2.Cu","nets":["+3V3","+5V"]}]`).
   - Generate the zones (KiCad zone with priority and clearance from the rules) instead of
     routing those nets.
   - Split planes: Voronoi/region partition from the pad positions of each net, with a minimum
     neck width and clearance between splits.
3. **Pad-to-plane connections**
   - Every power pin gets a short fanout + via to its plane (the "via next to pad" rule), with a via
     count per pin depending on the pin current (config) and thermal relief settings respected.
4. **Stitching and return paths**
   - GND stitching vias along outer pours and board edges (grid pitch configurable).
   - Signal vias that change reference layer get a nearby GND via (soft rule).
5. **Current-aware width** (see #6): width per net from current and temperature rise (IPC-2152
   tables), necks only at pads, with a minimum neck width rule.
6. **Pour-aware routing**
   - Route signals so that pours stay connected: soft cost for cutting an existing/planned plane
     (islands, narrow necks); report a pour-fragmentation metric.

## Acceptance (first step)
Synthetic 4-layer board: `--planes In1.Cu=GND` produces a GND zone on In1, all GND pads connect
through fanout vias, 0 new DRC errors, and KiCad's zone fill connects every GND pad.
