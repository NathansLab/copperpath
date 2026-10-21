# 0026 CI: OS matrix, release binaries, CodeQL, Dependabot

GitHub: https://github.com/NathansLab/copperpath/issues/13
Status: open (infrastructure)

Use GitHub's free capacity for public repositories. Keep PR CI under 10 min.

## Scope
1. `ci.yml`
   - Matrix ubuntu-latest / windows-latest / macos-latest (arm64).
   - Steps: `cargo fmt --check`, `clippy --all-targets -D warnings`, `build --release`, `test --release`.
   - `Swatinem/rust-cache`; `concurrency: {group: ci-${{ github.ref }}, cancel-in-progress: true}`.
2. `release.yml`
   - On tag `v*`: build Linux x86_64, Windows x86_64 and macOS arm64 binaries and attach them to a
     GitHub Release (own matrix or cargo-dist), with SHA256 sums.
3. Security and upkeep
   - CodeQL workflow for Rust.
   - `.github/dependabot.yml`: cargo + github-actions, weekly.
   - README section "Security": secret scanning / push protection are enabled in the repository
     settings (by the owner).

## Acceptance
All jobs green on a PR. A test tag (`v0.0.0-test`, deleted afterwards) produces 3 binaries.
