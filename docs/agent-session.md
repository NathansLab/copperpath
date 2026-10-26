# An agent session on a real board

A complete loop on the PCBench board **icehat** (4 layers, 59 nets, GND and +3V3 planes, 107
open connections), as an AI agent would run it. Every command takes `--json` and prints one JSON
object; the output keeps its `.kicad_pro` / `.kicad_dru` next to it, so each next command sees
the board's net classes. Times are wall times on 4 cores.

| # | step | command | time | result |
|---|---|---|---|---|
| 1 | look | `status --pcb icehat.kicad_pcb --refill-zones --json` | 0.06 s | 107 opens; `suggested_prio`: `+5V=wide,+1V2=wide,I2S_BCLK=low_l,SPI_CLK=low_l` |
| 2 | route | `autoroute --pcb icehat.kicad_pcb --out r.kicad_pcb --drc native --time 10 --prio '…'` | 10 s | 19 opens left, DRC clean |
| 3 | check | `drc --pcb r.kicad_pcb --json` | 0.02 s | 0 violations, 19 unconnected |
| 4 | why open | `explain --pcb r.kicad_pcb --net GPIO24_CRS --json` | 0.02 s | "blocked by routed copper of I2S_LRCK, GPIO23_CDN, …: a 0.20 mm corridor exists without it (rip up and reroute)" |
| 5 | fix | `explain`'s `next`: `route --pcb r.kicad_pcb --nets 'GPIO24_CRS' --reroute-blockers --out fixed.kicad_pcb` | 2 s | GPIO24_CRS routed, its blockers I2S_LRCK and GPIO23_CDN ripped and rerouted: 19 -> 18 opens |
| 6 | (by hand) | `ripup --nets 'GPIO24_CRS,I2S_LRCK'`, then `route --nets 'GPIO24_CRS,I2S_LRCK' --time-limit 5` | 0.02 s + 3 s | GPIO24_CRS routed, I2S_LRCK now open (the corridor is shared) |
| 7 | look | `status --pcb r3.kicad_pcb --json` | 0.02 s | 19 opens, DRC clean |
| 8 | bus | `netinfo --pcb r3.kicad_pcb --match 'SPI_.*' --json` | 0.01 s | SPI skew 2.33 mm |
| 9 | match | `tune --pcb r3.kicad_pcb --match 'SPI_.*' --out r4.kicad_pcb` | 0.02 s | SPI_MISO 11.08 → 13.41 mm, SPI_MOSI 11.35 → 13.41 mm (skew 0) |
| 10 | check | `drc --pcb r4.kicad_pcb --json` | 0.02 s | 0 violations |
| 11 | see | `render --pcb r4.kicad_pcb --opens --out r4.png` | 0.04 s | image with the open connections as lines |

Notes for agents:

- **Budget**: start with `autoroute --time 10`. More time helps dense boards, not small ones
  (README, "How much time to give autoroute"). icehat: 18 opens at 10 s, 10 at 120 s.
- **Intents**: take `suggested_prio` from `status`. Supply rails get `wide` rather than `low_r`:
  twice the width cost icehat 8 extra open connections at `--time 10`.
- **Reading failures**: `explain` names the blocking nets, whether a corridor exists without
  them, and gives the command that fixes it (`next`); `ripup` + `route --nets` redoes a few nets
  by hand in about a second. Two nets that share a
  corridor trade places, so fix the net that matters more or give the pair more room (`nudge`).
- **Project files**: if a board has no `.kicad_pro` next to it, `status` says so in `next`: all
  nets then get KiCad's default class, and `drc` reports false clearance violations.
