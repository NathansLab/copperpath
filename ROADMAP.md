# Roadmap

**Goal:** route a complete, already-placed KiCad board in seconds to minutes, from net
priorities and the board's own rules, including power/HV nets and pours. Generic (no board-specific
code), efficient and parallel (rayon), and easy to drive from scripts, AI agents and KiCad.

Status 0.1.0: signal nets in a region route well (0 opens on the demo and on a 46-net test
region); power/HV pours are not routed, very long multi-pin nets are slow.

## Milestones

1. **Per-net-class strategies.** Order, cost weights, via budget, layer sets and neck-down per
   class (configurable, sensible defaults: short/critical nets first, power last or as pours).
2. **Wide power tracks.** Width from the DRU/net class (or a current), multi-cell-wide search,
   parallel tracks and via arrays for high current; DRU length and impedance-ish width rules.
3. **Pours and planes.** Generate copper zones for plane nets (own outline, clearances from the
   rules, thermal reliefs), keep them consistent with routed tracks; leave fill to KiCad.
4. **Via stitching.** GND/plane stitching and shielding via rows with the via-site maps that `check`
   already computes.
5. **Incremental / ECO mode.** Keep existing copper, reroute only changed or open nets; stable
   results for small edits.
6. **Global-route corridors.** Use the global router to bound detailed searches for long
   multi-pin nets (today 10 M+ nodes per search on large nets).
7. **JSON API.** Request/response files (and a stdin/stdout mode) for every command: board path,
   nets with priorities, rule overrides; result with geometry diff, metrics and warnings.
8. **KiCad entry point.** IPC-API plugin / action button: route selection or region from the
   running editor, show metrics and hints.
9. **Placement loop.** Use `hints` and `check` results to drive `place`, then route; keep it
   agent-friendly (small JSON, deterministic seeds).

## Non-goals (for now)

- Differential pairs and length tuning beyond simple max-length rules.
- Blind/buried/micro vias.
- Replacing KiCad's DRC: KiCad's `kicad-cli pcb drc` stays the reference check.
