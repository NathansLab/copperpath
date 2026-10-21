# 0028 Fuzzing for the S-expression parser and board loader

GitHub: https://github.com/NathansLab/copperpath/issues/15
Status: open (infrastructure)

- `fuzz/` with cargo-fuzz targets:
  - `sexpr::parse` on arbitrary bytes;
  - `board::parse_board` on mutated small boards (seed corpus: `examples/demo`, the test minis).
- Weekly workflow (nightly toolchain), 10 min per target.
- A crash uploads the reproducer as an artifact and opens an issue.
- Acceptance: the targets build, a 60 s local run is clean, the workflow runs green once.
