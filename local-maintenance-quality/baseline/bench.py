#!/usr/bin/env python3
"""Scoped synthetic adaptation of /home/marce/.agents/tools/brain/bench.py.

Recall@3, MRR@10, latency, response size and a scoped FTS5 baseline. Reads only
an explicitly identified synthetic laboratory DB; no ask/LLM or global defaults.
"""

import argparse
import hashlib
import json
import math
import sqlite3
import statistics
import sys
import time
import unicodedata
from datetime import datetime, timezone
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from lab import mcp_call, open_lab

HERE = Path(__file__).resolve().parent
ESCOPOS = {"regente": ["regente"], "meusagentes": ["meusagentes"]}


def norm(value):
    return unicodedata.normalize("NFKD", value or "").encode("ascii", "ignore").decode().lower()


def wilson(success, count):
    if not count:
        return None
    z = 1.96
    fraction = success / count
    denominator = 1 + z * z / count
    center = (fraction + z * z / (2 * count)) / denominator
    margin = z * math.sqrt(fraction * (1 - fraction) / count + z * z / (4 * count * count)) / denominator
    return [round(center - margin, 4), round(center + margin, 4)]


def corpos_da_pagina(connection, hit, workspace, projetos):
    # Path alone cannot distinguish a sibling workspace or an historical head.
    project = hit.get("project") or hit.get("project_name")
    alvo = [project] if project else projetos
    if any(p not in projetos for p in alvo):
        return []
    marks = ",".join("?" * len(alvo))
    query = ("SELECT p.body FROM pages p JOIN projects pr ON pr.id=p.project_id "
             "JOIN workspaces ws ON ws.id=p.workspace_id "
             f"WHERE p.path=? AND p.is_latest=1 AND ws.name=? AND pr.name IN ({marks}) "
             "AND (p.expires_at IS NULL OR p.expires_at > ?)")
    return [r[0] or "" for r in connection.execute(query, (hit["path"], workspace, *alvo, int(time.time() * 1_000_000)))]


def buscar_fts(connection, query, workspace, projetos):
    terms = " OR ".join('"' + term.replace('"', '""') + '"' for term in query.split())
    marks = ",".join("?" * len(projetos))
    sql = ("SELECT pr.name, p.path FROM pages_fts JOIN pages p ON p.rowid=pages_fts.rowid "
           "JOIN projects pr ON pr.id=p.project_id JOIN workspaces ws ON ws.id=p.workspace_id "
           f"WHERE pages_fts MATCH ? AND p.is_latest=1 AND ws.name=? AND pr.name IN ({marks}) "
           "AND (p.expires_at IS NULL OR p.expires_at > ?) ORDER BY pages_fts.rank LIMIT 10")
    return [{"project": project, "path": path} for project, path in connection.execute(
        sql, (terms, workspace, *projetos, int(time.time() * 1_000_000)))]


def medir(connection, server_url, workspace, perguntas, metodo):
    latency, sizes, ranks, correct, covered, categories = [], [], [], [], [], {}
    errors = []
    for question in perguntas:
        projects = ESCOPOS[question["project"]]
        args = {"query": question["q"], "limit": 10, "workspace": workspace, "project": question["project"]}
        begin = time.perf_counter()
        try:
            if metodo == "fts":
                hits = buscar_fts(connection, question["q"], workspace, projects)
                size = 0
            else:
                result = mcp_call(server_url, "memory_query", args)
                hits = result.get("hits", [])
                size = len(json.dumps(result))
                # This corpus deliberately has no global scope: global hits must
                # not count as a scoped success or silently widen the denominator.
                if result.get("global_hits") or result.get("global_scope_hits"):
                    raise ValueError("unexpected global results in isolated corpus")
            latency.append((time.perf_counter() - begin) * 1000)
            sizes.append(size)
        except (ValueError, KeyError, sqlite3.Error) as error:
            errors.append({"id": question["id"], "kind": type(error).__name__})
            hits = []
        terms = [norm(term) for term in question["must"]]
        position = next((i for i, hit in enumerate(hits)
                         if hit["path"] == question["expected_path"] and any(
                             all(term in norm(body) for term in terms)
                             for body in corpos_da_pagina(connection, hit, workspace, projects))), None)
        bodies = corpos_da_pagina(connection, {"path": question["expected_path"]}, workspace, projects)
        covered.append(any(all(term in norm(body) for term in terms) for body in bodies))
        hit = position is not None and position < 3
        correct.append(hit)
        ranks.append(1 / (position + 1) if position is not None else 0)
        category = categories.setdefault(question["cat"], [0, 0])
        category[0] += hit
        category[1] += 1
    count = len(correct)
    ordered = sorted(latency) or [0]
    return {"recall@3": round(sum(correct) / count, 3), "recall@3_n": count,
            "recall@3_k": sum(correct), "recall@3_ic95": wilson(sum(correct), count),
            "mrr@10": round(statistics.mean(ranks), 3), "cobertas": sum(covered),
            "por_categoria": {key: f"{value[0]}/{value[1]}" for key, value in categories.items()},
            "p50_ms": round(statistics.median(ordered), 1),
            "p95_ms": round(ordered[max(0, math.ceil(.95 * len(ordered)) - 1)], 1),
            "tokens_estimados_med": round(statistics.mean(sizes) / 4) if sizes else None,
            "erros": errors}


def run(lab_dir, server_url, db, workspace, log):
    output = Path(log).resolve()
    root = Path(lab_dir).resolve()
    if root not in output.parents:
        raise ValueError("benchmark log must stay inside the explicit laboratory")
    connection = open_lab(root, server_url, db, workspace)
    try:
        fixture_bytes = (HERE / "fixtures.json").read_bytes()
        questions = json.loads(fixture_bytes)["queries"]
        line = {"quando": datetime.now(timezone.utc).isoformat(timespec="seconds"),
                "conjunto": hashlib.sha256(fixture_bytes).hexdigest(),
                "workspace": workspace, "perguntas": len(questions),
                "limites": "synthetic scoped retrieval only; no LLM, embeddings or answer synthesis"}
        line["projeto"] = medir(connection, server_url, workspace, questions, "uso-real")
        line["linha_de_base"] = medir(connection, server_url, workspace, questions, "fts")
        with output.open("a") as target:
            target.write(json.dumps(line, ensure_ascii=False) + "\n")
        return line
    finally:
        connection.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for flag in ("lab-dir", "server-url", "db", "workspace", "log"):
        parser.add_argument("--" + flag, required=True)
    args = parser.parse_args()
    try:
        line = run(args.lab_dir, args.server_url, args.db, args.workspace, args.log)
    except (ValueError, OSError, sqlite3.Error, KeyError, json.JSONDecodeError) as error:
        parser.error(f"laboratory benchmark refused ({type(error).__name__})")
    print(json.dumps(line, ensure_ascii=False, indent=2))
    if line["projeto"]["erros"] or line["projeto"]["cobertas"] != line["perguntas"]:
        raise SystemExit(1)


if __name__ == "__main__":
    main()
