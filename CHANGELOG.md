# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added
- `fanout`: one track + via per open island of a plane net onto a plane fill (grid search from an
  SMD pad to the nearest legal via spot within `--reach`), neck-down inside the pad's courtyard
  from a DRU `track_width` rule with `intersectsCourtyard`. About 1 s for 42 islands on a 6-layer
  board.
- DRU conditions `intersectsCourtyard` / `intersectsFrontCourtyard` / `intersectsBackCourtyard`
  are evaluated when the item position is known (`Rules::track_width_at`).
- `Model::fill_zone`: zone of every fill polygon.
- `stitch`: stitching vias for the fill islands of a plane net (one per island without a via,
  optional grid), legal against the DRU/net-class model incl. outer-layer creepage, pads, holes,
  edge and no-via rule areas, on a plane fill of the same net. Writes the vias to a copy
  (`--base` for another board text). About 0.5 s for 97 islands on a 6-layer board (a Shapely
  prototype took 16 s).
- `Checker::via_block_fixed` reports what blocks a via; `Checker::min_clr` floors the clearance.
- Added copper gets uuids seeded from the board text (no duplicates when a command runs on the
  output of another).
- Saved zone fills (`filled_polygon`) count as connected copper of their net, locked or not:
  `opens` and the router's unrouted-component count see the real islands of a plane net instead
  of one component per pad. `opens --json` lists the pads of every island (`--max-islands`);
  `--nets` also counts plane nets. The router may start or end a connection on a fill inside the
  net's search window. Connectivity uses a bounding-box sweep and an edge index per fill
  (all nets of a 6-layer board with 217 fills in about 1 s; per-net opens equal KiCad's DRC).

### Fixed
- Router: a DRU `track_width` rule scoped by `intersectsCourtyard('REF')` sets the neck-down width
  inside that courtyard (power net with a wide class on a fine-pitch part gets its stubs); the
  neck-down of such a net then applies inside those courtyards only (#2, width part).
- A pin whose own pad already sits closer to another net than the rule (LV pins of an isolator
  next to its HV pins) gets its stub: the track inside the own pad, up to where its centre leaves
  the pad plus the half width, is the pad's own copper and is not checked (#53).
- A plated pad with `remove_unused_layers` (outer pad + inner barrel) is one piece of copper for
  connectivity; before, copper touching only its outer pad left it as a separate island.

### Fixed
- A connection may start or end on any existing track or via of a component, not only at its
  pads: a pad boxed in by other copper (no stub, no room for a via) still joins through its own
  via or a dangling track end; via ends are pulled to the via centre.
- Panel files with several closed Edge.Cuts outlines: each item is checked against the outline
  it lies in (even-odd: an outline inside another is a cut-out), instead of only the first
  outline; outlines drawn from lines chain into several loops. New option `--outline N` keeps
  only outline N (#68).
- Tracks and vias of other nets stay out of solder mask apertures: F.Mask/B.Mask graphics of
  footprints and the board (e.g. over a solder jumper) and pad mask expansions only admit the
  nets of the pads and locked zones inside them; no more `solder_mask_bridge` (#3).
- `class_layers`: the most specific matching pattern wins; before, `"*"` won because JSON keys
  are sorted. The log prints the effective layers per class (#38).
- A multi-pin net whose start pin cannot be joined keeps the rest of its routing (the tree is
  regrown from the next components) instead of dropping the whole net (#39).
- KiCad 5 board files are refused with a clear message instead of routing an empty board (#0002).
- A missing `.kicad_pro` no longer aborts; KiCad's default net class is used (#0003).
- Edge.Cuts arcs in the legacy centre/angle form are read; no panic on incomplete graphics (#0004).
- The neck-down width respects the Board Setup minimum track width (#0005).
- Copper graphics and copper text without a net are obstacles, Edge.Cuts drawn inside footprints
  are part of the board outline (#0006).
- Unlocked tracks and vias of other nets are obstacles (they stay in the output); `--ignore-unlocked`
  restores the old behaviour (#0008).
- Plated pads with `remove_unused_layers` block only with their barrel on inner layers (#0010).
- Failed nets report why the search stopped (node cap, job window, no path); the node cap grows
  with the net's search window (#0011).
- Vias respect the Board Setup minimum diameter, drill and annular width (#0013).
- Pad drill offsets shift the pad copper (#0014).
- New vias and tracks of different nets keep the minimum hole clearance (#0015).
- Multi-line copper texts are obstacles over their full height.
- Fine-pitch pins whose pad axes sit off the 0.05 mm grid get exit stubs again: copper may touch
  the clearance exactly (1 nm float guard instead of a 5 µm margin), and the routing grid is
  shifted onto the axes of the tightest pin rows (#1; synthetic QFN-48 at 0.4 mm pitch, 0.0164 mm
  off-grid: 48 of 48 pins without a stub -> 0, all 48 connections routed).
- Faster negotiation: A* skips stale heap entries and caches its heuristic per cell (same
  result, demo 5.5 s -> 2.8 s); nets with non-interacting windows route in parallel; the
  negotiation stops after 3 iterations without progress (`--stall-iters`) instead of 6
  (public benchmark boards: 114 s -> 79 s and 162 s -> 74 s).
- A* uses a 4-ary heap of packed u64 keys and window-local indices without divisions; the
  routed boards are byte-identical (public boards: 85 -> 58 s, 68 -> 37 s, 51 -> 33 s).
- Negotiated searches read one precomputed cost per cell (built once per net and iteration)
  instead of seven maps per neighbour; byte-identical, public board bikedar 57 -> 44 s CPU.
- The two fallback levels (no slide, no cleanup) run at the same time instead of one after
  the other; the chosen result is the same.

### Added
- `--time-limit S`, SIGTERM and Ctrl+C write the best result so far; `--json` gains `stopped`
  and `hotspots`, the log prints the conflict hotspots; the fallback ladder is skipped when
  out of time (#8).
- `route --cancel-file PATH` and `--cancel-on-stdin-eof`: cancel paths that work on Windows,
  Linux and macOS and write the best result so far like `--time-limit`; Ctrl+C / Ctrl+Break
  do the same on Windows. `--json` reports `stopped: "cancel"` (#19).
- `--yield-pours` / config `yield_pours`: locked pours that yield like unlocked ones (#0009).
- `kicad-plugin/`: KiCad 9/10 IPC API plugin (MVP). A **Route with copperpath** button routes the
  open board and applies the `--delta-json` change set as one commit (one Ctrl+Z undoes it, no
  file reload); binary from the plugin setting, the bundled `bin/` or `PATH`; errors in a dialog
  with the last log lines. Tested against a mocked kipy API in CI (#21).

## [0.1.0] - 2026-10-21

First public version (research prototype).

### Added
- `route`: grid router (0.05 mm, up to 6 copper layers, 45° directions, through vias) with
  negotiated congestion, exact rule checks from KiCad net classes and custom rules (`.kicad_dru`).
- Route aesthetics: straight pad exits, 45°-only tracks, exact cleanup (pull-tight, jog merge,
  stub/junction/via sliding, mitering) and a fallback ladder (full, no-slide, no-clean).
- Final octilinear polish (small jogs instead of off-angle segments) and removal of tiny
  T-junction legs (avoids KiCad `track_dangling` warnings).
- `check`: routability report (via sites per net class, global-route congestion, hotspots, PNG maps).
- `place`: parallel simulated-annealing placement inside zones from a JSON file.
- `opens`, `hints`, `pads`, `netclass` helper commands.
- `--json` on every command (one result object on stdout, non-zero exit codes on errors),
  `--drc` on `route` (kicad-cli DRC delta), `--version` with git hash.
- Optional board configuration (`--cfg`, see `examples/config.json`).
- Synthetic demo board (`examples/demo/`, generated by `scripts/make_demo.py`), unit and
  integration tests, CI.
