#!/usr/bin/env bash
set -euo pipefail

version="${1:-1.0.0}"
target="${2:-x86_64-unknown-linux-gnu}"
root="$(cd "$(dirname "$0")" && pwd)"
stage="$root/target/package"
archive="$root/ladavian.dockercopilot-helper-$version-linux-amd64.tar.gz"

cargo build --release --target "$target" --manifest-path "$root/Cargo.toml"
mkdir -p "$stage"
cp "$root/plugin.json" "$stage/plugin.json"
cp "$root/target/$target/release/dockercopilot-helper" "$stage/plugin"
chmod 755 "$stage/plugin"
tar -C "$stage" -czf "$archive" plugin plugin.json
sha256sum "$archive"
