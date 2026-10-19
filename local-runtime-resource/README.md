# Local fork release manifest

After cutting a `vX.Y.Z-local.N` release with `bin/release`, build and install
the tagged version, restart the user service, then record the installed artifact:

```bash
cargo build --locked --offline --release -p ai-memory-cli
install -m 755 target/release/ai-memory ~/.local/bin/ai-memory
systemctl --user restart ai-memory.service
local-runtime-resource/record-manifest.sh vX.Y.Z-local.N
git -C ~/.agents add tools/ai-memory/manifest.json
git -C ~/.agents commit -m "chore(ai-memory): record vX.Y.Z-local.N"
```

`record-manifest.sh` requires Bash and Python 3.11+ (standard library only,
matching the installer's `tomllib` requirement). It reads the tag from this
checkout, validates `~/.local/bin/ai-memory --version`, and records the tagged
commit, tree and Cargo.lock hash, source tar hash/path, and installed binary
hash, size and version. Other manifest values and key order are preserved,
using the existing JSON indentation.

`AI_MEMORY_MANIFEST` overrides `/home/marce/.agents/tools/ai-memory/manifest.json`.
`AI_MEMORY_RELEASE_BACKUP_DIR` overrides the backup base `~/backups/ai-memory`;
archives go under `release-<sha8>/source-<sha8>.tar`. An existing tar is reused
only when its hash matches. `manifest-antes-YYYYMMDD.json` in that directory
preserves the first manifest saved for that release that day.

Use `record-manifest.sh --dry-run <tag>` to print the field diff without
creating archives, backups or manifest writes, or invoking the installer.
A normal run updates the manifest atomically, then runs
`~/.agents/tools/ai-memory/install-ai-memory.sh --check` against that manifest
and returns its exit code. If verification fails, the updated manifest and
its prior backup remain available for diagnosis. The recorder itself builds
nothing and restarts no services.

Run `local-runtime-resource/tests/test-record-manifest.sh` for integration
checks against the real `v2.5.3-local.2` tag, installed binary and audited
manifest. The test copies the manifest, patches and systemd templates into
a temporary directory and uses a temporary backup base. It also executes
only the final printing section of `bin/release` for local/upstream versions.
