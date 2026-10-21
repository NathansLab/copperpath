# Third-party licenses

placeroute is licensed under MIT OR Apache-2.0. Its Rust dependencies (from `Cargo.lock`) are
listed below; all are compatible with both licenses. Release binaries statically link these crates.
Keep their notices when you redistribute binaries.

| Crate | Version | License |
|---|---|---|
| adler2 | 2.0.1 | 0BSD OR MIT OR Apache-2.0 |
| aho-corasick | 1.1.5 | Unlicense OR MIT |
| bitflags | 1.3.2 | MIT OR Apache-2.0 |
| cfg-if | 1.0.5 | MIT OR Apache-2.0 |
| crc32fast | 1.5.2 | MIT OR Apache-2.0 |
| crossbeam-deque | 0.8.8 | MIT OR Apache-2.0 |
| crossbeam-epoch | 0.9.21 | MIT OR Apache-2.0 |
| crossbeam-utils | 0.8.23 | MIT OR Apache-2.0 |
| either | 1.19.0 | MIT OR Apache-2.0 |
| fdeflate | 0.3.7 | MIT OR Apache-2.0 |
| flate2 | 1.1.10 | MIT OR Apache-2.0 |
| itoa | 1.0.18 | MIT OR Apache-2.0 |
| memchr | 2.8.3 | Unlicense OR MIT |
| miniz_oxide | 0.8.9, 0.9.1 | MIT OR Zlib OR Apache-2.0 |
| png | 0.17.16 | MIT OR Apache-2.0 |
| proc-macro2 | 1.0.107 | MIT OR Apache-2.0 (build time only) |
| quote | 1.0.47 | MIT OR Apache-2.0 (build time only) |
| rayon | 1.12.0 | MIT OR Apache-2.0 |
| rayon-core | 1.13.0 | MIT OR Apache-2.0 |
| regex | 1.13.1 | MIT OR Apache-2.0 |
| regex-automata | 0.4.18 | MIT OR Apache-2.0 |
| regex-syntax | 0.8.11 | MIT OR Apache-2.0 |
| serde, serde_core | 1.0.229 | MIT OR Apache-2.0 |
| serde_derive | 1.0.229 | MIT OR Apache-2.0 (build time only) |
| serde_json | 1.0.154 | MIT OR Apache-2.0 |
| simd-adler32 | 0.3.10 | MIT |
| syn | 3.0.9 | MIT OR Apache-2.0 (build time only) |
| unicode-ident | 1.0.27 | (MIT OR Apache-2.0) AND Unicode-3.0 (build time only) |
| zlib-rs | 0.6.8 | Zlib |
| zmij | 1.0.23 | MIT |

Regenerate the list after dependency updates, e.g. with
`cargo metadata --format-version 1 | jq -r '.packages[] | "\(.name) \(.version) \(.license)"'`.

## KiCad footprints in the demo board

`examples/demo/demo.kicad_pcb` embeds footprints from the KiCad standard footprint library
(https://gitlab.com/kicad/libraries/kicad-footprints). These are licensed under
CC-BY-SA 4.0 with the KiCad library exception, which allows their use in designs without
the design falling under the share-alike terms. See https://www.kicad.org/libraries/license/.

## Tools used by the scripts (not distributed)

- KiCad 10 (`kicad-cli`, bundled Python with `pcbnew`): GPL-3.0-or-later, called as an external program.
- matplotlib (`scripts/plot_route.py`): matplotlib license (PSF-style).
