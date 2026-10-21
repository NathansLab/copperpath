#!/usr/bin/env bash
# Build, test and publish a placeroute binary into a release folder.
#
# usage: PLACEROUTE_RELEASE_DIR=/path/to/release scripts/release.sh [--no-latest]
#
# Copies target/release/placeroute to $PLACEROUTE_RELEASE_DIR/placeroute-<shorthash> and points the
# symlink placeroute-latest there. --no-latest (or PLACEROUTE_NO_LATEST=1) keeps the symlink as it
# is, e.g. when the new build has not been compared with the current one yet. Refuses to release a
# dirty working tree unless PLACEROUTE_ALLOW_DIRTY=1.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
dest="${PLACEROUTE_RELEASE_DIR:?set PLACEROUTE_RELEASE_DIR to the release folder}"
latest=1
[[ "${1:-}" == "--no-latest" || "${PLACEROUTE_NO_LATEST:-0}" == "1" ]] && latest=0

cd "$repo"
hash="$(git rev-parse --short=8 HEAD)"
if [[ -n "$(git status --porcelain)" && "${PLACEROUTE_ALLOW_DIRTY:-0}" != "1" ]]; then
    echo "error: working tree has uncommitted changes (commit first or set PLACEROUTE_ALLOW_DIRTY=1)" >&2
    exit 1
fi

cargo build --release
cargo test --release

mkdir -p "$dest"
bin="$dest/placeroute-$hash"
cp target/release/placeroute "$bin"
chmod 755 "$bin"
"$bin" --version
if [[ $latest == 1 ]]; then
    ln -sfn "placeroute-$hash" "$dest/placeroute-latest"
    echo "released $bin (placeroute-latest -> placeroute-$hash)"
else
    echo "released $bin (placeroute-latest unchanged)"
fi
