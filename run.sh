#!/bin/sh
# Build Mallow if sources changed (no-op otherwise), then start it.
# Usage: ./run.sh [url ...]
set -e
cd "$(dirname "$0")"
cargo build --release
exec ./target/release/mallow "$@"
