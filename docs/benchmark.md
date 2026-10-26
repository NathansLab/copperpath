# Benchmark history

`scripts/bench_pcbench.py` routes eight MIT-licensed boards of the PCBench set that Freerouting
benchmarks with, end to end, and checks each result with `kicad-cli pcb drc --refill-zones`:

- each board is downloaded at a pinned Freerouting commit (sha256 checked; no board data in this
  repository), upgraded with kicad-cli, keeps its own net classes and board setup minimums,
  loses all tracks and vias, keeps its zones unfilled;
- `copperpath autoroute --time BUDGET` (releases before `autoroute` existed: `route`);
- per board: unconnected items before and after, new KiCad DRC items against the input, route
  time, vias and track length against the human reference.

The `Benchmark` workflow (`.github/workflows/benchmark.yml`) runs it on every release tag and by
hand for any commit or tag, and adds one row here per run (pull request). Times depend on the
machine (GitHub runners: 4 cores); compare rows from the same machine.

"fully routed" counts the boards that end with 0 unconnected items; "unconnected" and "new DRC"
are sums over all boards.

| release | date | commit | command | fully routed | unconnected | new DRC | vias | route time s | machine |
|---|---|---|---|---|---|---|---|---|---|
| main fe329e3 | 2026-10-25 | `fe329e3e` | autoroute | 1/8 | 152 | 25 | 1131 | 406 | 4 cores Intel(R) Xeon(R) Processor @ 2.10GHz |
