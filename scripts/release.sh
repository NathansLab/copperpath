#!/usr/bin/env bash
# Build, test and publish a copperpath binary into a release folder.
#
# usage: COPPERPATH_RELEASE_DIR=/path/to/release scripts/release.sh [--no-latest]
#
# Copies target/release/copperpath to $COPPERPATH_RELEASE_DIR/copperpath-<shorthash> and points the
# symlink copperpath-latest there. --no-latest (or COPPERPATH_NO_LATEST=1) keeps the symlink as it
# is, e.g. when the new build has not been compared with the current one yet. Refuses to release a
# dirty working tree unless COPPERPATH_ALLOW_DIRTY=1.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${COPPERPATH_RELEASE_DIR:?set COPPERPATH_RELEASE_DIR to the release folder}"
latest=1
[[ "${1:-}" == "--no-latest" || "${COPPERPATH_NO_LATEST:-0}" == "1" ]] && latest=0

cd "$repo"
hash="$(git rev-parse --short=8 HEAD)"
if [[ -n "$(git status --porcelain)" && "${COPPERPATH_ALLOW_DIRTY:-0}" != "1" ]]; then
    echo "error: working tree has uncommitted changes (commit first or set COPPERPATH_ALLOW_DIRTY=1)" >&2
    exit 1
fi

cargo build --release
cargo test --release

mkdir -p "$dest"
bin="$dest/copperpath-$hash"
cp target/release/copperpath "$bin"
chmod 755 "$bin"
"$bin" --version
if [[ $latest == 1 ]]; then
    ln -sfn "copperpath-$hash" "$dest/copperpath-latest"
    echo "released $bin (copperpath-latest -> copperpath-$hash)"
else
    echo "released $bin (copperpath-latest unchanged)"
fi
