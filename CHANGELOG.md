# Changelog

All notable changes to this project are documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); versions follow [SemVer](https://semver.org/).

## [Unreleased]

### Added
- Routing intent `z:OHM`: the track width for a single-ended impedance on the outer layers,
  computed from the board stackup (microstrip; FR4 defaults without one); `netinfo` reports
  `z0_ohm` per net. Intent `outer`: the net's tracks stay on the outer layers.
- `thermals`: a short stub track for every starved thermal pad (fewer than 2 spokes), and an
  `autoroute` stage that runs it after the pours (`--no-thermals`); sizif128: kicad-cli
  `starved_thermal` 17 -> 7.
- Native DRC: `starved_thermal` counts spokes as kicad-cli 10 (per fill polygon, island spokes
  ignored while the pad has another connection, tracks only when they end in the fill).
- Net class names carry routing intents: `Power [low_r]` or a class named `quiet` applies that
  profile to its nets (after `--prio`, config and project block).
- README benchmark section: PCBench results table, renders of the routed boards and a DRC time
  chart (`scripts/readme_figures.py`); first row in `docs/benchmark.md`.
- `ripup --nets PATS [--region] [--locked]`: remove the copper of nets (agents redo single nets
  with `route --nets`).
- Routing intent `quiet` / `iso:X`: soft extra spacing to other nets' copper (fixed and routed),
  e.g. for sensitive analog nets; `netinfo` reports `coupled_mm` and `gap_min_mm` per net
  (demo: CLK coupled length 11.2 -> 0.1 mm with `CLK=quiet`).
- `route --signal-layers F.Cu,B.Cu`: tracks of signal nets only on these layers (vias pass all).
- `fanout --plane-only`: only the plane zones' fills join pads.
- `drc --refill-zones` reports `starved_thermal` (a thermal-relief pad with one spoke; none is
  unconnected, as in kicad-cli). The native filler keeps a spoke whose tip meets a spoke of a
  neighbouring pad of the net (pads closer than twice the relief gap, as KiCad joins them).
- `status`: one-call board overview for agents (stack, classes, zones, plane candidates, opens,
  native DRC, suggested next commands) in milliseconds.
- `Benchmark` workflow: the PCBench benchmark on every release tag and by hand for any commit or
  tag; results attached to the release and added to `docs/benchmark.md` by pull request.
- CI runs the KiCad plugin's Python tests.
- Routing intents per net class (`class:PATTERN` keys) and from the project file
  (`.kicad_pro` block `"copperpath": {"net_prio": ...}`, next to the net classes).
- `route --res auto`: 0.1 mm grid when the rules and pad pitches allow it (2.6-4x faster on
  coarse two-layer boards, same result), else 0.05 mm.
- `route --stall-gain F` (stop negotiating when iterations gain less than F) and `--no-fallback`
  (no extra full routes for nets the exact check dropped).
- Routing intents per net (`--prio NET=PROFILE`, config `net_prio`): `low_r`, `low_l`, `short`,
  `few_vias`, `wide`, `first` and raw multipliers scale the via, bend and layer-direction costs
  and the track width per net and route marked nets first.
- `netinfo`: length, vias, widths and estimated DC resistance and partial inductance per net.
- `--drc native` for `route`, `nudge` and `autoroute`: the DRC gate with the native copper DRC
  (milliseconds, the input's rules for both boards) instead of two kicad-cli runs.
- `drc`: native copper DRC with kicad-cli's rule names and JSON layout (clearance, shorts,
  crossings, hole and edge clearance, track width, unconnected items) in milliseconds; same
  counts as kicad-cli 10 on the PCBench benchmark boards.
- Native zone filler: thermal reliefs (spokes along the pad axes, 45 degrees for round pads; a
  spoke that does not reach the fill or hits foreign copper is dropped, so a pad with no spoke
  stays unconnected like in KiCad), the zone's own clearance and minimum width, pad and
  footprint `zone_connect` overrides (#86).
- `autoroute`: placed board to routed board in one command within a time budget (`--time`):
  plane zones (`--planes In1.Cu=GND`), plane fanout, negotiated signal routing with rip-up of
  blockers, stitching, quality gate (opens with native fills, optional KiCad DRC against the
  input; the latest stage without new errors is written). Per stage timing and opens in `--json`.
- `scripts/make_stress.py`: synthetic 6-layer stress board generator for benchmarks.
- `explain` and `--explain` / `--explain-img DIR` (route, fanout, stitch, plane report): a
  one-line reason per failure (`congestion`, `routed-copper` with rip-up candidates, `too-narrow`
  with the widest corridor, bottleneck and blockers, `enclosed`) and a ROI image of it.
- `nudge` joint moves: group shifts of all scanned parts, and `--push R [--push-range 1]` pushes
  colliding small passives (2-pad SMD) near the move envelopes out of the way within one
  candidate. `--reach D` routes open connections from the parts up to D mm away,
  `--rip-classes` rips and reroutes blocking nets of these classes, `--exclude-nets`.
- `nudge --json` explains failures: `infeasible_reasons` (rule, part, blocking item, position),
  per failed net the router's reason and the pins of its components; `--explain-img DIR` renders
  ROI images of them. Warning when copper zones have no saved fill.
- `nudge`: local placement optimisation by real routing. Movable parts (`--parts`, `--spec` JSON
  with per part offset ranges, grid step, rotations and keep-in, or `--auto` = unlocked 2-pad SMD
  parts near open connections) are searched on a discrete grid (coarse scan + pattern search,
  parallel candidates); each candidate passes the placement hard rules of `place` and is routed
  for real in the region after ripping the unlocked copper of the affected nets. Score: opens,
  then length + via/move/rotation penalties. Writes moved footprints + new copper (`--dry-run`
  only reports), `--json` per part move, per net result, candidates and timing.
- `Router::fixed_maps` / `Router::new_with`: blocker maps of everything that stays are built once
  and only the moving footprints are stamped per candidate (about 10x faster router setup).
- `sites::track_stamp` / `sites::via_stamp`: stamp objects into existing blocker maps.
- `droute::QUIET` silences the router's progress lines.
- `plane` (planes, issues #29, #30, #35): `plane list` detects plane/pour nets and power-like
  candidates; `plane create` writes a single-net zone on a layer over the free area (other-net
  zones, keep-outs) with an HV cut-back (`--hv REGEX --creepage D`, pour keep-outs for HV copper
  inside), idempotent on re-runs; `plane adjust` grows zones into free area, cuts them back from
  HV copper and merges them; `plane report` fills natively and reports islands, isolated islands,
  islands without via, unconnected pads, lost area, narrowest necks with positions and stitching
  coverage (`--png` renders it); `plane fill` writes the native fill as `filled_polygon`.
  Native filler matches KiCad's fill area within 0.2 % on the demo board.
- `--native-fill` for `fanout`, `stitch`, `opens` and `render`: zones are filled natively first,
  no kicad-cli refill needed.
- `movefp::move_footprints`: footprint move primitive for the board writer. Translates and rotates
  footprints (about their origin) in the `.kicad_pcb` text: the footprint `at`, the absolute pad
  and field angles and the board-coordinate outlines of rule areas inside the footprint change,
  everything else stays byte for byte.
- `render`: PNG of a board region for AI agents (native rasteriser, about 0.05 s for 1200 px):
  copper layers colour-coded and semi-transparent, zone fills, pads with `REF.PIN`/net labels
  without overlaps, vias, courtyards, rule areas, HV clearance halos, ratsnest of the open
  connections, DRC markers from a kicad-cli JSON report, mm grid, scale bar; `--per-layer`
  small multiples; `--json` lists the open connections in the region.
- `route --reroute-blockers [--max-ripped 3] [--max-targets 10]`: for every net left open, the
  unlocked copper of other nets on its cheapest corridor (search with that copper passable at a
  high cost) is ripped and the net is rerouted together with the ripped nets; kept only if no
  net gets worse, rolled back otherwise. `--json` reports each try (`reroute`).
- `fanout` and `stitch` report the nearest blockers of every failed island in `--json`
  (net, kind, footprint, layer, distance, required clearance, locked, position): what an agent would
  have to move (`model::nearest_blockers`).
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

### Changed
- `autoroute`: plane-net pads that the plane fill and the fanout leave open (e.g. dense
  through-hole pins the plane cannot reach) are routed in the pour stage with tracks on the
  signal layers; prog_rig: 110 -> 25 opens at the same budget.
- Router: nets with job windows above 4 M nodes get a reachability flood first; components the
  search cannot reach are skipped instead of expanding the whole window (prog_rig +5V in the
  pour stage: 64 -> 5 s). The node cap for large windows is at most 3x `--max-expand`.
- Router: the hard reroute after the negotiation runs in parallel waves (same result).
- `route --reroute-blockers` stops at `--time-limit`.
- Router: the A* buffers are kept per thread and only the nodes a search touched are reset, so
  a search costs the nodes it expands instead of its whole window. Same result; the first
  negotiation iteration on the PCBench sizif128 board (2801 x 1701 cells, a 56-pin GND) takes
  26 s instead of 52 s, the demo 4.4 s instead of 5.5 s.
- `--drc`: the kicad-cli report of the input board is cached (per process and in the temp dir,
  keyed by board, project, rules and the kicad-cli binary) and the routed board is checked in
  parallel, so the autoroute quality gate and repeated runs check an unchanged input once.

### Fixed
- Routed copper: a track end on the middle of another track of the net (T-junction) splits
  that track there; kicad-cli counted such ends as `track_dangling` (icehat, rf_siggen).
- `scripts/bench_pcbench.py`: when kicad-cli times out on the input board (a123_bcm zone
  refill), both boards are checked with the native DRC instead of a second 600 s wait.
- `autoroute`: the signal routing leaves a fifth of the budget to the pour nets, and the pour
  stage runs even when the signals overran the budget (PCBench sizif128 at 120 s: the GND pour
  stage was skipped, 32 -> 16 unconnected).
- Dangling copper (`track_dangling` / `via_dangling` in KiCad): collinear overlapping segments of
  a net are merged before writing (a pad exit stub lying on a run had a free end in the middle
  of the other track); `--reroute-blockers` rips all unlocked copper of a ripped net, not only
  the pieces on the corridor, and never rips plane nets (their fanout vias stayed half removed).
  PCBench: new dangling items after autoroute 6 -> 1 (rf_siggen), 5 -> 1 (icehat).
- `route`: a segment that fails the exact clearance check no longer drops its whole net; only
  the failing segments and what dangles from them are removed (PCBench rf_siggen: two pour nets
  kept 53 of 82 segments instead of none).
- `autoroute`: the plane fanout ignores outer pours of the net (`fanout --plane-only`), so every
  pad reaches the plane itself; signal tracks stay off inner plane layers (`--signal-layers`).
  PCBench, unconnected items after autoroute (kicad-cli): rf_siggen 30 -> 14, icehat 16 -> 11,
  rxadc_14 11 -> 14, fan_controller 0 -> 1, LVDS2TMDS 0.
- `autoroute` / `fanout` / `stitch`: unnamed zones (boards from KiCad 5) are selected by the key
  `NET@LAYER` (e.g. `GND@In1.Cu`). `autoroute` skipped the plane fanout on such boards, so no pad
  reached its inner plane (PCBench rxadc_14: 29 -> 11 unconnected, 1 new KiCad DRC item).
- The board setup minimum clearance defaults to 0 like in KiCad (was 0.2 mm), so net classes
  below 0.2 mm are honoured: the router was too strict and `drc` flagged legal pad pairs (28 on
  PCBench rxadc_14, where kicad-cli reports none).
- Board setup minimums missing from the project (via diameter, annular ring, hole) now take
  KiCad's defaults (0.5 / 0.1 / 0.3 mm) instead of 0; the router placed 0.46 mm vias that
  kicad-cli flagged (`via_diameter`, `drill_out_of_range`).
- `opens`: pads with the same number in one footprint (split exposed pads) are one terminal, as
  in KiCad, instead of separate islands.
- `autoroute`: a net with outer pours only is no longer a plane net. It had no inner plane to fan
  out to and stayed open (two-layer boards: all GND/supply pads unconnected). Such pour nets are
  now routed after the signals, on the natively filled pours, only where the fill leaves pads
  apart (new stage `pours`, up to 3 fill/route rounds).
- `route`: the cleanup fallback ladder (two more full routes) only runs when a failed net may have
  been broken by the cleanup; pure search failures (no path, node cap, window) skip it (#54).
  Route stage of the 800-part stress board: 217 s to 70 s, same opens.
- Native fill keeps islands without copper of the net when it replaces saved fills
  (`--native-fill`): a new plane without vias was dropped completely, so `fanout` found nothing
  to land on.
- Router: routed copper that fails the final exact clearance check is no longer written; the net
  is dropped and reported open (before, two shorts could reach the file with `aes.exact_fail`
  as the only hint; seen on the demo board with GND as a plane net).
- `fanout --plane REGEX`: when the pattern also matches outer-layer pours of the net (F/B
  pours named like the plane), only the inner-layer fills count as the plane. A pad in an
  isolated outer pour fragment without a via is now an open island (before, every pad touching
  any matching fill counted as connected, e.g. 6 instead of 18 open islands).
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
- `route --out` on an already routed board handed out the earlier run's UUIDs again (fixed
  seed); the seed now comes from the input board (#22).
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
- `route --delta-json FILE` (or `-` for stdout): added and removed items in integer nanometres
  for the KiCad IPC API, with or instead of `--out`; `--pcb -` reads the board from stdin.
  `copperpath::delta::apply` applies a delta to board text (#20).
- KiCad plugin: **Route selected nets** (`--nets` from the selected tracks, vias, pads and
  footprints), **Route region** (`--region` from a selected rectangle or zone), **Rip up selected
  nets** and **Rip up and reroute** (rip-up and route as two commits). Route deltas that remove
  copper or touch nets outside the selection are refused; `--ignore-unlocked` is refused in the
  plugin settings. README documents that `route` never changes existing copper ("route the
  rest") and what `--ignore-unlocked` does (#22).
- `route --progress-json`: NDJSON progress events on stderr (`phase`, one `iter` per negotiation
  iteration with conflict cells, nets left and an ETA, a final versioned `done`); without the
  flag the output is unchanged (#18).
- KiCad plugin: progress dialog (phase, iteration, conflict cells, nets left, progress bar, ETA)
  while copperpath runs; the editor stays usable. **Stop and apply best result** cancels the
  router and applies its best result as one commit, **Cancel and discard** throws it away. If the
  board changed during the run, the plugin asks before applying. The board goes to copperpath
  as a temporary file so stdin stays open: the router stops when the plugin ends (#23).

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
