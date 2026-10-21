# Contributing

Bug reports and pull requests are welcome.

- **Never attach proprietary board data.** Repro boards must be synthetic: generate them
  (see `scripts/make_demo.py`) or strip a board down to a few made-up parts and nets. No real
  net names, part numbers, company or project names.
- Use the issue templates "Bad routing result" or "Feature request". GitHub issues are the only
  tracker. Include `copperpath --version`, the full command and the `--json` output.
- Before a pull request: `cargo fmt`, `cargo clippy --all-targets --release` (0 warnings),
  `cargo test --release`. Router changes: route the demo and report opens, vias, length and the
  kicad-cli DRC delta before and after (`copperpath route ... --drc --json`).
- Contributions are dual licensed under MIT OR Apache-2.0, as the project.

## Workflow and labels

- `main` is protected: every change goes through a pull request.
- Issues are worked on in priority order: `prio:1` (correctness) to `prio:4`.
- Automated agents:
  - `agent:copilot`: assigned to the Copilot coding agent.
  - `werkbank`: picked up by the maintainer's build machine (branch `werkbank/issue-N`).
  - `takeover`: hand an issue from Copilot to werkbank.
  - `no-auto`: keep an issue away from all agents.
- `epic` issues are split into sub-issues and delivered milestone by milestone.
