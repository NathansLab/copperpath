# 0019 Feature request: wider tracks for power nets (current-aware width)

GitHub: https://github.com/NathansLab/copperpath/issues/6
Status: open (feature request)

**Problem**
copperpath routes power nets (GND, +3V3, +5V, VBAT, ...) with the net-class width, which on most
public benchmark boards is the default 0.2-0.25 mm. Human references route them 0.4-1 mm wide.

**Proposal**
- Config key `"power_width": {"pattern": ["GND", "+*V*", "VBAT*"], "width": 0.5}` and/or CLI
  `--power-width 0.5 --power-nets "GND,+*"`.
- Route those nets first at the wide width, neck down only at pads (existing neck-down logic).
- Optional: width from `(current, temp rise)` via IPC-2152 when a net-class property gives a current.

**Acceptance**
On a board with default class 0.25 mm, `--power-width 0.5` yields >= 90 % of power-net length at
0.5 mm, 0 new DRC errors.
