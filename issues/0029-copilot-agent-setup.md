# 0029 Prepare the Copilot coding agent and document the bot workflow

GitHub: https://github.com/NathansLab/copperpath/issues/16
Status: open (infrastructure)

1. `.github/copilot-instructions.md`
   - Architecture overview: board → model → droute / groute / place → clean → write.
   - Style: comments and docs in English; match the surrounding code.
   - Every fix comes with a unit test, plus a synthetic repro board when needed.
   - fmt, clippy `-D warnings` and tests must pass.
   - No third-party or proprietary board data, net names or part numbers in the repo; synthetic
     repros only.
2. `.github/workflows/copilot-setup-steps.yml`: Rust stable toolchain, rust-cache, KiCad CLI
   (optional), `cargo build --release`.
3. CONTRIBUTING: explain the labels.
   - `agent:copilot`: assigned to Copilot first.
   - `takeover`: the workbench agent takes over.
   - `no-auto`: humans only.
   - `werkbank`: worked by the workbench agent.
   - The workbench also takes over when no PR exists after 90 min, the PR is still a draft after
     4 h, or its checks are red.
