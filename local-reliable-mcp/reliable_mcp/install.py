"""Stage units and source. This module never installs them."""

from __future__ import annotations

import shlex
import shutil
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
BACKEND_BIND = "127.0.0.1:49376"


def render_dropin(unit_text: str) -> str:
    command = _last_execstart(unit_text)
    rewritten = _replace_bind(command, BACKEND_BIND)
    return f"[Service]\nExecStart=\nExecStart={rewritten}\n"


def dry_run_text(unit_text: str) -> str:
    dropin = render_dropin(unit_text)
    return "não instala: plano apenas, nenhuma unit foi escrita.\n" + dropin


def stage(dest: Path, unit_text: str, source_root: Path | None = None) -> None:
    root = Path(source_root) if source_root is not None else ROOT
    dest = Path(dest)
    dest.mkdir(parents=True, mode=0o700)
    unit_dir = dest / "units" / "ai-memory.service.d"
    unit_dir.mkdir(parents=True, mode=0o700)
    (unit_dir / "backend-bind.conf").write_text(render_dropin(unit_text), encoding="utf-8")
    units = dest / "units"
    for name in ("ai-memory-ingress.service", "ai-memory-delivery.service"):
        text = (root / "templates" / name).read_text(encoding="utf-8")
        (units / name).write_text(text, encoding="utf-8")
    source = dest / "source"
    source.mkdir(mode=0o700, exist_ok=True)
    launcher = source / "reliable-mcp"
    shutil.copy(root / "reliable-mcp", launcher)
    launcher.chmod(0o755)
    package = source / "reliable_mcp"
    if package.exists():
        shutil.rmtree(package)
    shutil.copytree(
        root / "reliable_mcp",
        package,
        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"),
    )
    patches = dest / "patches"
    patches.mkdir(mode=0o700, exist_ok=True)
    shutil.copy(root / "patches" / "install-ai-memory-ready.patch", patches / "install-ai-memory-ready.patch")


def _last_execstart(unit_text: str) -> str:
    found = []
    for line in unit_text.splitlines():
        stripped = line.strip()
        if stripped.startswith("ExecStart="):
            found.append(stripped[len("ExecStart=") :])
    if not found:
        raise ValueError("unit has no ExecStart")
    return found[-1]


def _replace_bind(command: str, bind: str) -> str:
    parts = shlex.split(command)
    rewritten = []
    replaced = False
    index = 0
    while index < len(parts):
        part = parts[index]
        if part == "--bind" and index + 1 < len(parts):
            rewritten.extend(["--bind", bind])
            replaced = True
            index += 2
            continue
        if part.startswith("--bind="):
            rewritten.append("--bind=" + bind)
            replaced = True
            index += 1
            continue
        rewritten.append(part)
        index += 1
    if not replaced:
        rewritten.extend(["--bind", bind])
    return " ".join(rewritten)
