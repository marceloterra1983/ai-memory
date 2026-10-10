#!/usr/bin/env bash
set -euo pipefail
SERVER_ORIGIN="${AI_MEMORY_SERVER_URL:-http://127.0.0.1:49374}"
SERVER_MCP="${SERVER_ORIGIN%/}/mcp"
DATA_DIR="${AI_MEMORY_DATA_DIR:-$HOME/.local/share/ai-memory}"
CONFIG_TOML="${AI_MEMORY_CONFIG:-$DATA_DIR/config.toml}"
BIN="${AI_MEMORY_BIN:-$HOME/.local/bin/ai-memory}"
MANIFEST="${AI_MEMORY_MANIFEST:-$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)/manifest.json}"
UNIT="${AI_MEMORY_UNIT:-$HOME/.config/systemd/user/ai-memory.service}"
SYSTEM_BIN="${AI_MEMORY_SYSTEM_BIN:-/usr/bin/ai-memory}"
ok() { printf '  [ok] %s\n' "$*"; }
skip() { printf '  [--] %s\n' "$*"; }
warn() { printf '  [!!] %s\n' "$*"; }
die() { printf 'erro: %s\n' "$*" >&2; exit 1; }
have() { command -v "$1" >/dev/null 2>&1; }
aim() { "$BIN" --data-dir "$DATA_DIR" --config "$CONFIG_TOML" "$@"; }
wire_mcp() {
  local client="$1"; shift
  if aim install-mcp --client "$client" --apply --server-url "$SERVER_MCP" "$@"; then ok "MCP $client"
  else warn "MCP $client falhou"; return 1; fi
}
wire_hooks() {
  local agent="$1"; shift
  if aim install-hooks --agent "$agent" --apply --server-url "$SERVER_ORIGIN" "$@"; then ok "hooks $agent"
  else warn "hooks $agent falhou"; return 1; fi
}
on_path() { have "$1" || [[ -x "$HOME/.local/bin/$1" ]]; }

# --check: só leitura (sem mkdir, init, systemctl, db, lock ou escrita no artefato).
# Códigos de saída determinísticos: 0 = artefato local íntegro (o pacote do
# sistema é informativo e nunca muda o código); 1 = drift no artefato local
# (sha/versão do binário, patch, scheduler auto_improve ou unit). O pacote do
# sistema (pacman) pode divergir de versão/sha sem reprovar o check.
check() {
  python3 - "$MANIFEST" "$BIN" "$CONFIG_TOML" "$UNIT" "$SYSTEM_BIN" "$DATA_DIR" "$HOME" <<'PY'
import hashlib, json, os, subprocess, sys, tomllib
manifest, binp, config, unit, sysbin, data_dir, home = sys.argv[1:]
bad = 0
def res(ok, msg):
    global bad
    print(("  [ok] " if ok else "  [!!] ") + msg)
    bad += not ok
def sha(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for b in iter(lambda: f.read(1 << 20), b""):
            h.update(b)
    return h.hexdigest()
try:
    m = json.load(open(manifest))
except Exception as e:
    sys.exit(f"erro: manifest ilegível: {e}")
res(os.path.isabs(binp) and os.access(binp, os.X_OK), f"binário absoluto executável: {binp}")
if os.path.isfile(binp):
    res(sha(binp) == m["artifact"]["sha256"], "sha256 do binário == manifest.artifact")
    r = subprocess.run([binp, "--version"], capture_output=True, text=True)
    res(r.stdout.strip() == m["artifact"]["version_output"], f"--version == {m['artifact']['version_output']!r}")
patch = os.path.join(os.path.dirname(manifest), m["patch"]["file"])
res(os.path.isfile(patch) and sha(patch) == m["patch"]["sha256"], "patch fixbrainrank == manifest.patch")
try:
    cfg = tomllib.load(open(config, "rb"))
    res(cfg.get("auto_improve", {}).get("scheduler", {}).get("enabled") is False, f"scheduler auto_improve desligado em {config}")
    query_url = cfg.get("embedding_query_base_url")
    if query_url is not None:
        # Só gerencia o worker quando a URL é exatamente o loopback 8003 (CPU);
        # qualquer outro valor é configuração não suportada por este instalador.
        res(query_url == "http://127.0.0.1:8003/v1", "embedding_query_base_url é exatamente http://127.0.0.1:8003/v1")
        if query_url == "http://127.0.0.1:8003/v1":
            here = os.path.dirname(manifest)
            tmpl_unit = os.path.join(here, "systemd", "brain-embed-query.service")
            tmpl_drop = os.path.join(here, "systemd", "embedding-query.conf")
            inst_unit = os.path.join(home, ".config", "systemd", "user", "brain-embed-query.service")
            inst_drop = os.path.join(home, ".config", "systemd", "user", "ai-memory.service.d", "embedding-query.conf")
            res(os.path.isfile(tmpl_unit), "template brain-embed-query.service presente no repositório")
            res(os.path.isfile(tmpl_drop), "template embedding-query.conf presente no repositório")
            unit_ok = os.path.isfile(inst_unit) and os.path.isfile(tmpl_unit) \
                and open(tmpl_unit, "rb").read() == open(inst_unit, "rb").read()
            res(unit_ok, "unit brain-embed-query.service instalado == template do repositório")
            drop_ok = os.path.isfile(inst_drop) and os.path.isfile(tmpl_drop) \
                and open(tmpl_drop, "rb").read() == open(inst_drop, "rb").read()
            res(drop_ok, "dropin embedding-query.conf instalado == template do repositório")
            if os.path.isfile(inst_unit):
                u = open(inst_unit, encoding="utf-8", errors="replace").read()
                res("CUDA_VISIBLE_DEVICES=" in u, "unit roda só na CPU (CUDA_VISIBLE_DEVICES=)")
                res("--port 8003" in u, "unit expõe --port 8003")
                res("--host 127.0.0.1" in u, "unit expõe --host 127.0.0.1 (loopback)")
            if os.path.isfile(inst_drop):
                d = open(inst_drop, encoding="utf-8", errors="replace").read()
                res("Wants=brain-embed-query.service" in d, "dropin declara Wants=brain-embed-query.service")
                res("After=brain-embed-query.service" in d, "dropin declara After=brain-embed-query.service")
except Exception as e:
    res(False, f"config efetiva ilegível ({config}): {e}")
try:
    exec_line = next(l.split("=", 1)[1].strip() for l in open(unit) if l.startswith("ExecStart="))
    argv = exec_line.replace("%h", home).split()
    res(argv[0] == binp, "unit usa o binário absoluto")
    for flag, want in (("--config", config), ("--data-dir", data_dir)):
        if flag in argv:
            res(argv[argv.index(flag) + 1] == want, f"unit {flag} == efetivo")
except Exception as e:
    res(False, f"unit ilegível ({unit}): {e}")
if os.path.isfile(sysbin):
    # O pacote do sistema é gerenciado pelo pacman e independe do artefato
    # local pinado (não é fallback do servidor; sem downgrade de banco).
    # Versão/sha podem divergir legitimamente (pacman atualiza), então NÃO
    # exigimos sha fixo nem parseamos --version por regex. Verificamos apenas
    # identidade/posse via consulta estruturada do pacman e reportamos a versão.
    owner = subprocess.run(["pacman", "-Qo", sysbin], capture_output=True, text=True)
    if owner.returncode == 0 and "ai-memory" in owner.stdout:
        ver = subprocess.run(["pacman", "-Q", "ai-memory"], capture_output=True, text=True)
        pkg = ver.stdout.strip() if ver.returncode == 0 else "ai-memory"
        print(f"  [ok] {sysbin} pacote do sistema independente (pacman): {pkg}")
    else:
        # Presente mas sem posse confirmada do pacman: aviso, não é falha
        # load-bearing (o servidor usa o binário local, não o do sistema).
        print(f"  [!!] {sysbin} presente mas sem posse confirmada do pacman (não load-bearing)")
else:
    print(f"  [--] {sysbin} ausente (pacote do sistema não instalado)")
sys.exit(1 if bad else 0)
PY
}
if [[ "${1:-}" == "--check" ]]; then echo "==> ai-memory --check (somente leitura)"; check; exit $?; fi

echo "==> ai-memory no Omarchy"
[[ -x "$BIN" ]] || die "binário $BIN ausente (build pinado: build-reproduce.sh / manifest.json); sem instalação automática do pacote do sistema"
ok "binário $BIN"
mkdir -p "$DATA_DIR"
[[ -f "$CONFIG_TOML" ]] || aim init
# scheduler auto_improve nunca é religado por aqui (recorrência LLM só por decisão do dono)
python3 - "$CONFIG_TOML" <<'PY' || die "scheduler auto_improve ligado em $CONFIG_TOML"
import sys, tomllib
c = tomllib.load(open(sys.argv[1], "rb"))
sys.exit(0 if c.get("auto_improve", {}).get("scheduler", {}).get("enabled") is False else 1)
PY
systemctl --user daemon-reload
# Worker de query (só CPU, :8003): instalado só quando embedding_query_base_url é
# exatamente o loopback gerenciado; documentos permanecem em brain-embed.service :8002.
QUERY_URL="$(python3 - "$CONFIG_TOML" <<'PY'
import sys, tomllib
print(tomllib.load(open(sys.argv[1], "rb")).get("embedding_query_base_url", ""))
PY
)"
if [[ "$QUERY_URL" == "http://127.0.0.1:8003/v1" ]]; then
  HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
  mkdir -p "$HOME/.config/systemd/user/ai-memory.service.d"
  install -m 644 "$HERE/systemd/brain-embed-query.service" "$HOME/.config/systemd/user/brain-embed-query.service"
  install -m 644 "$HERE/systemd/embedding-query.conf" "$HOME/.config/systemd/user/ai-memory.service.d/embedding-query.conf"
  systemctl --user enable --now brain-embed-query.service
  ok "worker de query brain-embed-query.service (:8003, só CPU) antes do ai-memory"
elif [[ -n "$QUERY_URL" ]]; then
  warn "embedding_query_base_url não é o loopback 8003 gerenciado; worker de query não instalado"
fi
systemctl --user enable --now ai-memory.service
have loginctl && loginctl enable-linger "$USER" >/dev/null 2>&1 || true
ready=0
for _ in $(seq 1 20); do
  code="$(curl -s -o /dev/null -w '%{http_code}' --max-time 1 "$SERVER_ORIGIN/handoff" || true)"
  [[ "$code" =~ ^[1-5][0-9][0-9]$ ]] && { ready=1; break; }
  sleep 0.4
done
[[ "$ready" -eq 1 ]] || { systemctl --user status --no-pager ai-memory.service || true; die "servidor não respondeu"; }
ok "servidor em $SERVER_ORIGIN"

python3 - <<'PY'
import json, pathlib
def strip(path):
    if not path.is_file():
        return
    try:
        data = json.loads(path.read_text())
    except Exception:
        return
    mutated = False
    for key in ("mcpServers", "servers"):
        block = data.get(key)
        if not isinstance(block, dict):
            continue
        for k in [k for k in block if "claude-mem" in k.lower()]:
            block.pop(k, None); mutated = True
    if mutated:
        path.write_text(json.dumps(data, indent=2) + "\n")
        print(f"  [ok] removido claude-mem de {path}")
h = pathlib.Path.home()
strip(h / ".cursor" / "mcp.json")
strip(h / ".claude.json")
strip(h / ".claude" / "settings.json")
PY

mkdir -p "$HOME/.cursor"
wire_mcp cursor --config-file "$HOME/.cursor/mcp.json" || wire_mcp cursor
wire_hooks cursor
on_path claude && { wire_mcp claude-code; wire_hooks claude-code; } || skip "claude"
on_path codex && { wire_mcp codex; wire_hooks codex; } || skip "codex"
on_path opencode && { wire_mcp opencode; wire_hooks opencode; } || skip "opencode"
on_path agy && { wire_mcp antigravity-cli; wire_hooks antigravity-cli; } || skip "agy"
on_path grok && { wire_mcp grok; wire_hooks grok; } || skip "grok"
on_path pi && { wire_hooks pi; } || skip "pi"
on_path omp && { wire_mcp omp; wire_hooks omp; } || skip "omp"
on_path muse && { wire_mcp muse; skip "muse MCP-only"; } || skip "muse"
on_path crush && skip "crush: ai-memory run crush"
aim install-skills --scope global --agent both >/dev/null 2>&1 \
  || aim install-skills --scope global --agent agents >/dev/null 2>&1 || true
systemctl --user is-active ai-memory.service
echo "Pronto. Recarregue o MCP no Cursor."
