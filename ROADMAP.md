# Roadmap

**Goal:** route a complete, already-placed KiCad board in seconds to minutes, from net
priorities and the board's own rules, including power/HV nets and pours. Generic (no board-specific
code), efficient and parallel (rayon), and easy to drive from scripts, AI agents and KiCad.

Status (main, October 2026): `autoroute` routes whole boards in one call (planes, fanout,
signals, pours, thermal fixes, DRC gate); 2 of 8 PCBench boards end fully routed, the others
with 1-11 % of their connections open (README, benchmark). A native copper DRC with KiCad's
semantics runs in milliseconds.

## Done (formerly milestones)

- **Per-net strategies**: routing intents per net or class (`low_r`, `wide`, `low_l`, `short`,
  `few_vias`, `quiet`, `first`, `outer`, `z:OHM`, `pair`), from `--prio`, the project file or
  the net class name; `status` suggests them from net names.
- **Wide tracks**: width multipliers, impedance widths from the stackup, DRU width rules.
- **Pours and planes**: `plane` creates zones, a native filler (thermal reliefs, islands) feeds
  routing and DRC; `fanout` joins pads to planes; starved thermals get stub tracks (`thermals`).
- **Via stitching**: `stitch` for pours onto their planes.
- **Incremental work**: existing copper is kept; `ripup` + `route --nets` redo single nets.
- **JSON for everything**: `--json` on every command, `--delta-json` for the KiCad IPC API,
  `--progress-json` events.
- **Differential pairs and length matching**: `pair` intent and `--pairs`, `tune` meanders.

## Next

1. **Global-route corridors.** Bound the detailed search of long nets: board-long 2-pin nets
   and nets in congestion still spend most of a dense board's budget (prog_rig, sizif128).
2. **Completion on dense two-layer boards.** sizif128 needs about a minute for 22 opens; rip-up
   strategies beyond `--reroute-blockers` (via-aware, two-net swaps).
3. **Pairs and tuning in `autoroute`.** Pair intents across vias (keep the pair together through
   layer changes), intra-pair skew tuning, bus groups from the schematic.
4. **Stripline impedance.** `z:` on inner layers (today: microstrip on the outer layers).
5. **KiCad entry point.** IPC-API plugin: route a selection or region from the running editor,
   show metrics and hints.
6. **Placement loop.** Use `hints` and `check` results to drive `place`, then route; keep it
   agent-friendly (small JSON, deterministic seeds).

## Non-goals (for now)

- Blind/buried/micro vias.
- Replacing KiCad's DRC for sign-off: `kicad-cli pcb drc` stays the reference; the native DRC is
  the fast check while iterating.
