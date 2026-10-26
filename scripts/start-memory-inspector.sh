#!/bin/sh
set -eu

project_dir=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
# Build incrementally on every launch: the HTML and archive are embedded assets.
cargo build --manifest-path "$project_dir/Cargo.toml" --target-dir "$project_dir/target" -p hippo-inspector --release
exec "$project_dir/target/release/hippo-inspector" "$@"
