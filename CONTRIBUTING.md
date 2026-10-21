# Contributing

Bug reports and pull requests are welcome.

- **Never attach proprietary board data.** Repro boards must be synthetic: generate them
  (see `scripts/make_demo.py`) or strip a board down to a few made-up parts and nets. No real
  net names, part numbers, company or project names.
- Use the issue template "Bad routing result" (`.github/ISSUE_TEMPLATE/bad_result.md`, also in
  `issues/TEMPLATE.md` for local notes). Include `placeroute --version`, the full command and the
  `--json` output.
- Before a pull request: `cargo fmt`, `cargo clippy --all-targets --release` (0 warnings),
  `cargo test --release`. Router changes: route the demo and report opens, vias, length and the
  kicad-cli DRC delta before and after (`placeroute route ... --drc --json`).
- Contributions are dual licensed under MIT OR Apache-2.0, as the project.
