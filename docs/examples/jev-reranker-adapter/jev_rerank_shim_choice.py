#!/usr/bin/env python3
"""OpenAI-compat -> Jev `/v1/systemone` reranker adapter for ai-memory.

ai-memory's LLM reranker (`AI_MEMORY_RERANKER=llm`) sends one structured chat
request per `memory_query`: a fixed system prompt plus a user JSON payload
`{"query", "candidates":[{"candidate","title","text"}]}`, and expects the
first balanced JSON object in the reply content to be
`{"scores":[{"candidate","relevance"}]}` (1-based indices, relevance in
[0,1]; a timeout, error, or invalid score set preserves the server's own
order).

This adapter recognises exactly that request (by the system-prompt prefix),
compares candidates with one Jev `choice` question and returns each
candidate's choice probability as relevance in plain chat content. Everything
else — consolidation, lint, bootstrap — is reverse-proxied unchanged to the
configured upstream, so only reranking rides the Jev endpoint.

Why: a hosted reranker model that answers in tens of seconds dwarfs the rest
of a `memory_query`. The measured choice and historical rubric runs, including
their quality and latency limits, are documented in docs/jev-reranker-adapter.md.

Env:
  JEV_URL     Jev systemone endpoint      (default http://127.0.0.1:18095/v1/systemone)
  JEV_MODEL   model name sent to Jev      (default jev-latest)
  UPSTREAM    OpenAI-compat upstream base (default http://127.0.0.1:8000)
  LISTEN      host:port to bind           (default 127.0.0.1:18097)

Stdlib only. No secrets are stored: Authorization headers are forwarded
verbatim to the upstream.
"""
import json
import math
import os
import sys
import time
import urllib.error
import urllib.request
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

JEV_URL = os.environ.get("JEV_URL", "http://127.0.0.1:18095/v1/systemone").rstrip("/")
JEV_MODEL = os.environ.get("JEV_MODEL", "jev-latest")
UPSTREAM = os.environ.get("UPSTREAM", "http://127.0.0.1:8000").rstrip("/")
LISTEN = os.environ.get("LISTEN", "127.0.0.1:18097")

# Fixed prefix of the reranker's system prompt; see
# crates/ai-memory-llm/src/reranker.rs in the ai-memory tree.
RERANK_SYSTEM_PREFIX = "You are a retrieval reranker for a software project's memory wiki."


def log(msg):
    sys.stderr.write(f"[jev-rerank] {time.strftime('%H:%M:%S')} {msg}\n")
    sys.stderr.flush()


def jev_rerank(payload):
    """Translate a reranker chat request into ONE batched Jev choice call.

    Choice-contrastive variant of the rubric adapter: a single `choice`
    question over all candidates, ranked by probability. On the same
    golden set and backend the rubric adapter answers from, comparative
    ranking beat per-candidate rubric scoring by double-digit hit@1 on
    both backends measured (see the comparison table in the docs), and it
    is the only shape in which replay-trained small models hold their
    quality. Relevance is the raw choice probability: monotonic in the
    probability order, which is all the reranker sorts on.
    """
    user = next((m.get("content", "") for m in payload.get("messages", [])
                 if m.get("role") == "user"), "")
    data = json.loads(user)
    query, cands = data["query"], data["candidates"]
    lines = [f"User query: {query}", "", "Candidate documents:"]
    criteria = {}
    for c in cands:
        n = c["candidate"]
        desc = (c.get("title") or f"candidate {n}")[:120]
        lines.append(f"{n}. {desc} — {c['text'][:160]}")
        criteria[f"c{n}"] = desc
    # Pool v3 on the 35B backend: this wording beat "most relevant" 0.828 -> 0.879
    # hit@1 (5 fixed, 0 broken). Relevance stays the raw choice probability.
    questions = {"best": {"type": "choice",
                          "instructions": (
                              "Which candidate directly answers the user query? "
                              "Choose the specific page that contains the answer. "
                              "Do not choose an index, catalog, or summary page if a more specific page answers the query."
                          ),
                          "criteria": criteria}}
    body = {"model": JEV_MODEL, "state": "\n".join(lines), "questions": questions}
    t0 = time.perf_counter()
    req = urllib.request.Request(JEV_URL, data=json.dumps(body).encode(),
                                 headers={"Content-Type": "application/json"})
    with urllib.request.urlopen(req, timeout=25) as resp:
        out = json.loads(resp.read())
    dt = time.perf_counter() - t0
    probs = ((out.get("answers") or {}).get("best") or {}).get("probabilities") or {}
    scores = []
    for c in cands:
        n = c["candidate"]
        p = probs.get(f"c{n}")
        if (isinstance(p, bool) or not isinstance(p, (int, float))
                or not math.isfinite(p) or not 0.0 <= p <= 1.0):
            raise ValueError("incomplete or invalid choice probabilities")
        scores.append({"candidate": n, "relevance": round(p, 4)})
    log(f"jev-choice {len(cands)} candidates in {dt:.3f}s")
    return {"scores": scores}


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, fmt, *args):
        pass

    def _reply(self, status, body=b"", content_type="application/json"):
        self.send_response(status)
        if content_type:
            self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        if body:
            self.wfile.write(body)

    def _proxy(self, raw):
        """Reverse-proxy this request unchanged to the upstream."""
        req = urllib.request.Request(UPSTREAM + self.path, data=raw, method=self.command)
        for k, v in self.headers.items():
            if k.lower() in ("host", "content-length", "connection", "accept-encoding"):
                continue
            req.add_header(k, v)
        try:
            with urllib.request.urlopen(req, timeout=900) as resp:
                body = resp.read()
                self.send_response(resp.status)
                for k, v in resp.headers.items():
                    if k.lower() in ("transfer-encoding", "connection", "content-length"):
                        continue
                    self.send_header(k, v)
                self._reply_finish(body)
        except urllib.error.HTTPError as e:
            self._reply(e.code, e.read())
        except Exception as e:  # transport failure upstream
            log(f"forward transport failed: {type(e).__name__}")
            self._reply(502, json.dumps({"error": "upstream transport failure"}).encode())

    def _reply_finish(self, body):
        # headers besides Content-Length were already sent by the caller
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def do_POST(self):
        length = int(self.headers.get("Content-Length") or 0)
        raw = self.rfile.read(length) if length else b""
        if not self.path.rstrip("/").endswith("/chat/completions"):
            self._proxy(raw)
            return
        try:
            payload = json.loads(raw or b"{}")
        except json.JSONDecodeError:
            self._reply(400)
            return
        system = next((m.get("content", "") for m in payload.get("messages", [])
                       if m.get("role") == "system"), "")
        if not system.startswith(RERANK_SYSTEM_PREFIX):
            self._proxy(raw)  # consolidation / lint / bootstrap traffic
            return
        try:
            result = jev_rerank(payload)
            out = {
                "id": "jev-rerank",
                "object": "chat.completion",
                "model": payload.get("model", JEV_MODEL),
                "choices": [{
                    "index": 0,
                    "message": {"role": "assistant", "content": json.dumps(result)},
                    "finish_reason": "stop",
                }],
                "usage": {"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0},
            }
            self._reply(200, json.dumps(out).encode())
        except Exception as e:
            # The server treats any failure as "keep the original order".
            log(f"jev rerank failed: {type(e).__name__}")
            self._reply(500)

    def do_GET(self):
        self._proxy(None)


def main():
    host, port = LISTEN.rsplit(":", 1)
    srv = ThreadingHTTPServer((host, int(port)), Handler)
    log(f"listening {LISTEN} jev={JEV_URL} upstream={UPSTREAM}")
    try:
        srv.serve_forever()
    except KeyboardInterrupt:
        pass


if __name__ == "__main__":
    main()
