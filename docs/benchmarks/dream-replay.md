# Paired literal Dream replay

Run the bundled fixture from the repository root. The command uses the local
Cargo cache and writes `receipt.json` and `receipt.md` to the given directory.

```sh
replay_out=$(mktemp -d)
cargo run --locked --offline -p ai-memory-eval -- dream-replay --out "$replay_out"
cargo test --locked --offline -p ai-memory-eval dream_replay
```

The eval embeds the versioned fixture in the binary. It seeds a temporary
wiki and SQLite database through the normal wiki writer, with three synthetic
authors and three unpinned episodic pages. The existing synthetic bag-of-words
embedder generates the fixture vectors from the bodies. With the seed writer
closed, the eval sets the page timestamps to `2025-05-01T00:00:00Z`. It then
copies that wiki and takes a SQLite snapshot for the candidate. Both arms
start with the same files, page IDs, timestamps, and vectors.

The candidate calls `run_dream_pass` first in dry run and then in literal apply
mode, with the default decay and clustering settings. It uses the normal
eligibility, clustering, literal proof, checkpoint and wiki/store CAS paths.
The harness creates its own scratch paths and runs in-process without loading
installation config, contacting a server or model, or starting a scheduler.

## What the receipts show

- The baseline retains its heads, wiki bytes, version history and checkpoints.
  The dry run leaves the candidate in the same state and exposes source paths,
  version IDs, hashes and a digest that must match the subsequent apply.
- The apply lands one survivor with both complete bodies and leaves a linked
  stub. The unrelated cold page retains its original head. Original versions
  and their indexed authors remain in the supersession chains, and the
  survivor records both sources as reconsolidation evidence.
- Each task uses scoped FTS with the same top-three limit. The scorer reads
  the complete body from each returned version. A requirement needs its exact
  phrase and the original source. After apply, a merged source needs the
  survivor's provenance entry for the corresponding original version ID.
  The fixtures cover dates bound to measurements, a changed decision,
  identifiers, commands, a path, a negation and its exception.
- Context volume is the number of returned bodies and their total UTF-8 bytes.
  Estimated tokens are the sum of Unicode scalar counts divided by four and
  rounded up per query. A less selective result can expand the context. The
  estimate describes retrieved bodies and excludes model costs and the native
  Dream report's draft input estimate.
- The eval resolves both original files in the reported premerge checkpoint
  and verifies their bytes. It restores each page through
  `Wiki::restore_page_from_checkpoint`, checks the original bytes and
  attribution on disk, verifies the original and applied versions in the
  chain, and reruns the same tasks against the restored index.
- Two controls reuse the scorer with copies of the observed candidate context.
  Removing one literal must leave one requirement unsupported. Removing its
  source record while keeping the literal must leave that source's two
  requirements unsupported. The other source must still support its evidence.

The command exits with an error if a check fails. The fixture digest, selected
paths, evidence coverage and context volumes are reproducible. Version IDs,
generated metadata and Git commit IDs are native run-local values. The scratch
repositories are removed at the end. The receipts record the checkpoint IDs
and the original files' verified hashes from that execution.
