#!/usr/bin/env bash
# Integration checks use the installed binary, real tag and temporary copies only.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
python3 -B - "$REPO_ROOT" <<'PY'
from datetime import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys
import tempfile

repo = Path(sys.argv[1])
script = repo / "local-runtime-resource/record-manifest.sh"
resource = Path.home() / ".agents/tools/ai-memory"
tag = "v2.5.3-local.2"
fields = (
    "version", "source.commit", "source.tree", "source.release_tag",
    "source.source_archive_sha256", "source.source_archive_path",
    "deps.cargo_lock_sha256", "artifact.sha256", "artifact.size",
    "artifact.version_output",
)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def field(data, name, value=None):
    parts = name.split(".")
    for part in parts[:-1]:
        data = data[part]
    if value is not None:
        data[parts[-1]] = value
    return data[parts[-1]]


def check_next_steps():
    # Execute only the printing section: no release gates, commit or tag.
    tail = (repo / "bin/release").read_text().split(
        "# ── next steps ─", 1
    )[1].split("\n", 1)[1]
    for version in ("2.5.3-local.2", "2.5.3"):
        result = subprocess.run(
            ["bash", "-euo", "pipefail", "-c", tail], check=True,
            env={**os.environ, "VERSION": version, "TAG": "v" + version},
            capture_output=True, text=True,
        )
        if version.endswith("-local.2"):
            for step in ("cargo build --locked --offline --release -p ai-memory-cli",
                         "install -m 755 target/release/ai-memory ~/.local/bin/ai-memory",
                         "systemctl --user restart ai-memory.service",
                         "local-runtime-resource/record-manifest.sh v" + version,
                         "git -C ~/.agents add tools/ai-memory/manifest.json",
                         'git -C ~/.agents commit -m "chore(ai-memory): record v' + version + '"'):
                assert step in result.stdout, result.stdout
            assert "git push" not in result.stdout, result.stdout
        else:
            assert "git push && git push origin v2.5.3" in result.stdout
            assert "./bin/deploy" in result.stdout
            assert "record-manifest" not in result.stdout


with tempfile.TemporaryDirectory(prefix="ai-memory-manifest-") as temporary:
    root = Path(temporary)
    manifest = root / "manifest.json"
    shutil.copy2(resource / "manifest.json", manifest)
    # --check resolves these read-only inputs relative to the manifest copy.
    shutil.copytree(resource / "patches", root / "patches")
    shutil.copytree(resource / "systemd", root / "systemd")
    reference = json.loads(manifest.read_text())
    assert reference["source"]["commit"] == "79c0ed300d7c96c16ef944c1f8785c94672b3a19"
    stale = json.loads(manifest.read_text())
    for name in fields:
        field(stale, name, 0 if name == "artifact.size" else "stale")
    original = (json.dumps(stale, ensure_ascii=False, indent=2) + "\n").encode()
    manifest.write_bytes(original)
    manifest.chmod(0o640)
    backup_root = root / "release backups"
    env = {**os.environ, "AI_MEMORY_MANIFEST": str(manifest),
           "AI_MEMORY_RELEASE_BACKUP_DIR": str(backup_root)}

    def run(*args, extra_env=None):
        result = subprocess.run(
            ["bash", str(script), *args], cwd=root,
            env={**env, **(extra_env or {})}, capture_output=True, text=True,
        )
        return result

    # Removing dry-run's early return must expose writes to the stale copy.
    before = manifest.stat()
    result = run("--dry-run", tag)
    assert result.returncode == 0, result.stdout + result.stderr
    for name in fields:
        assert name in result.stdout, result.stdout
    after = manifest.stat()
    assert manifest.read_bytes() == original
    assert (before.st_ino, before.st_mtime_ns) == (after.st_ino, after.st_mtime_ns)
    assert not backup_root.exists(), "dry-run created a backup directory"
    print("PASS 1/3: dry-run rc=0; diff dos 10 campos; nenhuma escrita", flush=True)

    # Wrong tag/lockfile/artifact hashes or extra field edits must fail equality.
    result = run(tag)
    assert result.returncode == 0, result.stdout + result.stderr
    assert "==> ai-memory --check (somente leitura)" in result.stdout
    written = json.loads(manifest.read_text())
    archive = backup_root / "release-79c0ed30/source-79c0ed30.tar"
    expected = json.loads(json.dumps(reference))
    expected["source"]["source_archive_path"] = str(archive)
    assert written == expected, "manifest differs from the audited reference"
    assert manifest.read_text() == json.dumps(expected, ensure_ascii=False, indent=2) + "\n"
    assert manifest.stat().st_mode & 0o777 == 0o640
    assert sha(archive) == reference["source"]["source_archive_sha256"]
    saved_manifest = archive.parent / ("manifest-antes-" + datetime.now().strftime("%Y%m%d") + ".json")
    assert saved_manifest.read_bytes() == original
    archive_before = archive.stat()
    result = run(tag)
    assert result.returncode == 0, result.stdout + result.stderr
    archive_after = archive.stat()
    assert (archive_before.st_ino, archive_before.st_mtime_ns) == (archive_after.st_ino, archive_after.st_mtime_ns)
    assert saved_manifest.read_bytes() == original, "overwrote the first backup"
    print("PASS 2/3: atualização == manifesto atual (exceto path temporário); --check rc=0; tar reaproveitado e backup preservado", flush=True)

    # Failures must refuse before changing the manifest or clobbering a tar.
    unchanged = manifest.read_bytes()
    for invalid, diagnostic in (("v0.0.0-local.999999999", "tag inexistente"),
                                ("v2.5.2", "tag local inválida"),
                                ("v2.5.3-local.1", "versão instalada divergente")):
        negative_backup = root / ("refused-" + invalid)
        result = run(invalid, extra_env={"AI_MEMORY_RELEASE_BACKUP_DIR": str(negative_backup)})
        assert result.returncode != 0, result.stdout + result.stderr
        assert diagnostic in result.stderr, result.stderr
        assert manifest.read_bytes() == unchanged
        assert not negative_backup.exists()
    valid_archive = archive.with_suffix(".valid")
    archive.rename(valid_archive)
    archive.write_bytes(b"different archive\n")
    result = run(tag)
    assert result.returncode != 0, result.stdout + result.stderr
    assert "tar existente com hash diferente" in result.stderr, result.stderr
    assert archive.read_bytes() == b"different archive\n"
    assert manifest.read_bytes() == unchanged
    assert saved_manifest.read_bytes() == original
    archive.unlink()
    valid_archive.rename(archive)
    # The real checker fails on a nonexistent unit; its rc must reach callers.
    result = run(tag, extra_env={"AI_MEMORY_UNIT": str(root / "missing.service")})
    assert result.returncode == 1, result.stdout + result.stderr
    assert "unit ilegível" in result.stdout, result.stdout
    assert manifest.read_bytes() == unchanged
    check_next_steps()
    print("PASS 3/3: tag inexistente/formato inválido/versão divergente e tar conflitante recusados sem alterar a cópia; rc=1 do --check propagado; next steps local/upstream corretos", flush=True)
PY
