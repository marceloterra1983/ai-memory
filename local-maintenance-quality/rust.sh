#!/usr/bin/env bash
# Pinned toolchain, isolated Cargo cache; no user configuration or credentials.
set -euo pipefail
quality_root=$(cd "$(dirname "$0")/.." && pwd)
quality_git=$(git -C "$quality_root" rev-parse --git-common-dir)
case "$quality_git" in
  /*) ;;
  *) quality_git="$quality_root/$quality_git" ;;
esac
quality_git=$(realpath "$quality_git")
mkdir -p "$quality_root/target/quality-cargo"
exec docker run --rm \
  --mount "type=bind,src=$quality_root,dst=$quality_root" \
  --mount "type=bind,src=$quality_git,dst=$quality_git,readonly" \
  -e CARGO_HOME="$quality_root/target/quality-cargo" \
  -e RUSTUP_TOOLCHAIN=1.95.0 \
  -e CARGO_TARGET_DIR="$quality_root/target" \
  -w "$quality_root" ai-memory-quality-rust:1.95 "$(id -u)" "$(id -g)" "$@"
