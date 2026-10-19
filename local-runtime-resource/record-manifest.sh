#!/usr/bin/env bash
# Record an already installed local release; never build or restart anything.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec python3 -B - "$REPO_ROOT" "$@" <<'PY'
import argparse
from datetime import datetime
import difflib
import hashlib
import json
import os
from pathlib import Path
import re
import stat
import subprocess
import sys
import tempfile

repo = Path(sys.argv[1])


def git(*args):
    return subprocess.check_output(["git", "-C", str(repo), *args], stderr=subprocess.PIPE)


def sha(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def archive_sha(ref, output=None):
    digest = hashlib.sha256()
    with subprocess.Popen(["git", "-C", str(repo), "archive", "--format=tar", ref],
                          stdout=subprocess.PIPE) as process:
        with process.stdout as stream:
            for block in iter(lambda: stream.read(1024 * 1024), b""):
                digest.update(block)
                if output is not None:
                    output.write(block)
        if process.wait() != 0:
            raise ValueError("git archive falhou")
    return digest.hexdigest()


def sync_dir(path):
    fd = os.open(path, os.O_RDONLY | os.O_DIRECTORY)
    try:
        os.fsync(fd)
    finally:
        os.close(fd)


def write_manifest(path, content, mode):
    fd, temporary = tempfile.mkstemp(prefix=".manifest-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            os.fchmod(stream.fileno(), mode)
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        sync_dir(path.parent)
    finally:
        Path(temporary).unlink(missing_ok=True)


def save_archive(ref, path):
    path.parent.mkdir(parents=True, exist_ok=True)
    fd, temporary = tempfile.mkstemp(prefix=".source-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            digest = archive_sha(ref, stream)
            stream.flush()
            os.fsync(stream.fileno())
        # Linking publishes the complete tar without ever replacing an existing one.
        try:
            os.link(temporary, path)
        except FileExistsError:
            if sha(path) != digest:
                raise ValueError(f"tar existente com hash diferente: {path}") from None
        return digest
    finally:
        Path(temporary).unlink(missing_ok=True)


def save_previous(path, content):
    fd, temporary = tempfile.mkstemp(prefix=".manifest-antes-", dir=path.parent)
    try:
        with os.fdopen(fd, "wb") as stream:
            stream.write(content)
            stream.flush()
            os.fsync(stream.fileno())
        try:
            os.link(temporary, path)
        except FileExistsError:
            pass  # Preserve the first pre-update copy on retries the same day.
    finally:
        Path(temporary).unlink(missing_ok=True)
    sync_dir(path.parent)


def main():
    parser = argparse.ArgumentParser(description="Registra uma release local já instalada no manifesto.")
    parser.add_argument("--dry-run", action="store_true", help="imprime somente o diff dos campos")
    parser.add_argument("tag")
    args = parser.parse_args(sys.argv[2:])
    if not re.fullmatch(r"v.+-local\.[0-9]+", args.tag):
        raise ValueError("tag local inválida: esperado v<versão>-local.N")
    ref = "refs/tags/" + args.tag
    try:
        commit = git("rev-parse", "--verify", ref + "^{commit}").decode().strip()
    except subprocess.CalledProcessError:
        raise ValueError(f"tag inexistente ou sem commit: {args.tag}") from None
    binary = Path.home() / ".local/bin/ai-memory"
    version = args.tag[1:]
    version_output = subprocess.check_output([str(binary), "--version"], text=True).strip()
    if version_output != "ai-memory " + version:
        raise ValueError(f"versão instalada divergente: {version_output!r}; esperado ai-memory {version}")

    manifest = Path(os.environ.get("AI_MEMORY_MANIFEST", "/home/marce/.agents/tools/ai-memory/manifest.json")).expanduser().resolve()
    backup_root = Path(os.environ.get("AI_MEMORY_RELEASE_BACKUP_DIR", "~/backups/ai-memory")).expanduser().resolve()
    archive = backup_root / f"release-{commit[:8]}" / f"source-{commit[:8]}.tar"
    checker = Path.home() / ".agents/tools/ai-memory/install-ai-memory.sh"
    if not args.dry_run and not os.access(checker, os.X_OK):
        raise ValueError(f"instalador não executável: {checker}")
    original = manifest.read_bytes()
    data = json.loads(original)
    updates = {
        "version": version,
        "source.commit": commit,
        "source.tree": git("rev-parse", ref + "^{tree}").decode().strip(),
        "source.release_tag": args.tag,
        "source.source_archive_sha256": None,
        "source.source_archive_path": str(archive),
        "deps.cargo_lock_sha256": hashlib.sha256(git("show", ref + ":Cargo.lock")).hexdigest(),
        "artifact.sha256": sha(binary),
        "artifact.size": binary.stat().st_size,
        "artifact.version_output": version_output,
    }
    previous = {}
    for name in updates:
        target = data
        for key in name.split("."):
            target = target[key]
        previous[name] = target
    updates["source.source_archive_sha256"] = (
        archive_sha(ref) if args.dry_run else save_archive(ref, archive)
    )
    if args.dry_run:
        before = json.dumps(previous, ensure_ascii=False, indent=2) + "\n"
        after = json.dumps(updates, ensure_ascii=False, indent=2) + "\n"
        print("".join(difflib.unified_diff(before.splitlines(True), after.splitlines(True),
                                         fromfile=str(manifest), tofile=args.tag)), end="")
        return
    for name, value in updates.items():
        parts = name.split(".")
        target = data
        for key in parts[:-1]:
            target = target[key]
        target[parts[-1]] = value
    text = original.decode("utf-8")
    indentation = re.search(r'^([ \t]+)"', text, re.MULTILINE)
    indent = indentation[1] if indentation else 2
    rewritten = json.dumps(data, ensure_ascii=False, indent=indent)
    if text.endswith("\n"):
        rewritten += "\n"
    saved = archive.parent / ("manifest-antes-" + datetime.now().strftime("%Y%m%d") + ".json")
    save_previous(saved, original)
    write_manifest(manifest, rewritten.encode("utf-8"), stat.S_IMODE(manifest.stat().st_mode))
    print(f"Manifesto registrado: {manifest}\nFonte: {archive}\nBackup anterior: {saved}", flush=True)
    env = {**os.environ, "AI_MEMORY_MANIFEST": str(manifest), "AI_MEMORY_BIN": str(binary)}
    os.execve(checker, [str(checker), "--check"], env)


try:
    main()
except (OSError, ValueError, KeyError, TypeError, subprocess.CalledProcessError) as error:
    sys.exit(f"erro: {error}")
PY
