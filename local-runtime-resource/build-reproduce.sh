#!/usr/bin/env bash
# DRAFT — isolated operational rebuild for ai-memory 2.5.2+fixbrainrank.
#
# Rebuilds the fixbrainrank variant from the manifest's `source.local_checkout`
# (canonical ~/Projects/ai-memory) at the manifest's fixed `source.commit`, in a
# debian:12 + rustup 1.95 container. Never touches the binary in service.
#
# Safety and correctness:
#   * REFUSES a pre-existing output directory — never `rm -rf` an arbitrary path.
#   * The functional test exercises the fixbrainrank guard end to end with the
#     correct lib-harness path (`--lib integration::… -- --exact`), NOT the stale
#     `--test suite` command from the old manifest.
#   * A controlled negative mutates exactly ONE SQL condition (the project-name
#     exclusion), proves the test fails (exit 101, 0 passed / 1 failed), restores
#     the file byte-for-byte, then re-runs the positive. A compile error or zero
#     passing tests is refused — it is not a RED signal.
#
# Usage:
#   build-reproduce.sh [OUT=/tmp/ai-memory-build-repro]
#   MANIFEST=/path/to/manifest.json build-reproduce.sh [OUT]
#   AI_MEMORY_SRC=/path/to/checkout   build-reproduce.sh [OUT]   # override source
#   AI_MEMORY_BUILD_IMAGE=IMG         build-reproduce.sh [OUT]   # override image
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
# The manifest is the read-only source of truth for source, commit and variant.
MANIFEST="${MANIFEST:-/home/marce/.agents/tools/ai-memory/manifest.json}"
OUT="${1:-/tmp/ai-memory-build-repro}"
# The prebuilt ai-memory-integrate-lab:debian12-rust1.95 image can be supplied
# here to skip the in-container rustup install; the default is the self-contained
# debian:12 image the original resource used.
IMAGE="${AI_MEMORY_BUILD_IMAGE:-debian:12}"

# --- read the manifest (expand `~` in local_checkout to the canonical path) ---
SRC_REPO="$(python3 -c 'import json,sys,os; print(os.path.expanduser(json.load(open(sys.argv[1]))["source"]["local_checkout"]))' "$MANIFEST")"
SRC_REPO="${AI_MEMORY_SRC:-$SRC_REPO}"
COMMIT="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1]))["source"]["commit"])' "$MANIFEST")"

# --- refuse a pre-existing output dir (never rm -rf an arbitrary path) ------
if [ -e "$OUT" ]; then
    echo "refusing pre-existing output directory; move or remove it yourself: $OUT" >&2
    exit 2
fi
mkdir -p "$OUT/target"

# --- isolate the source at the fixed commit --------------------------------
git clone -q --no-hardlinks --no-checkout "$SRC_REPO" "$OUT/src"
git -C "$OUT/src" checkout -q "$COMMIT"

# --- build + verify inside the pinned toolchain container ------------------
docker run --rm \
    -v "$OUT/src:/src" \
    -v "$OUT/target:/target" \
    "$IMAGE" bash -eo pipefail -c '
  set -euo pipefail
  apt-get update -qq && apt-get install -y -qq curl ca-certificates build-essential pkg-config cmake clang libclang-dev git >/dev/null
  curl -sSf https://sh.rustup.rs | sh -s -- -y --profile minimal --default-toolchain 1.95 >/dev/null
  git config --global --add safe.directory /src
  . /root/.cargo/env
  cd /src
  export CARGO_TARGET_DIR=/target
  rustc --version; cargo --version

  cargo build --release --locked -p ai-memory-cli 2>&1 | tail -2

  FILTER="integration::retrieval_entity_project_name::entity_stream_does_not_vote_for_the_scoped_project_name"
  run_test() { cargo test --locked -p ai-memory-store --lib "$FILTER" -- --exact; }

  echo "== positive (guard in place) =="
  pos_log="$(mktemp)"
  if ! run_test >"$pos_log" 2>&1; then
      echo "ERROR: positive functional test failed" >&2
      tail -20 "$pos_log" >&2
      exit 3
  fi
  grep -E "^test |test result" "$pos_log"
  grep -q "1 passed; 0 failed" "$pos_log" \
      || { echo "ERROR: expected exactly 1 passing test, got zero or more" >&2; exit 4; }

  echo "== negative (exclusion removed) =="
  # Mutate exactly ONE condition: the project-name exclusion. Replacing
  # `e.name <> ?` with `? IS NOT NULL` keeps the single bind (positional) and
  # removes the exclusion, so the scoped project name itself votes and the test
  # must fail. Never swap the whole reader.rs for the upstream base — new APIs
  # are missing there and a compile error is NOT a red test.
  python3 -c "import pathlib; p=pathlib.Path(\"crates/ai-memory-store/src/reader.rs\"); s=p.read_text(); n=\"e.name <> ?\"; assert s.count(n)==1, n; p.write_text(s.replace(n, \"? IS NOT NULL\"))"
  restore() { git checkout -q -- crates/ai-memory-store/src/reader.rs; }
  trap restore EXIT
  neg_log="$(mktemp)"
  set +e
  run_test >"$neg_log" 2>&1
  neg_exit=$?
  set -e
  restore
  trap - EXIT
  if [ "$neg_exit" -eq 0 ]; then
      echo "ERROR: test passed without the exclusion patch" >&2
      exit 5
  fi
  if [ "$neg_exit" -ne 101 ]; then
      echo "ERROR: negative exited $neg_exit, expected 101 (not a test failure)" >&2
      tail -20 "$neg_log" >&2
      exit 6
  fi
  grep -E "^test |test result" "$neg_log"
  grep -q "FAILED. 0 passed; 1 failed" "$neg_log" \
      || { echo "ERROR: negative was not 0 passed / 1 failed (compile error is not RED)" >&2; exit 7; }
  echo "ok: test fails without the exclusion patch (exit $neg_exit)"

  echo "== positive (restored) =="
  run_test 2>&1 | grep -E "^test |test result"
'

sha256sum "$OUT/target/release/ai-memory"
