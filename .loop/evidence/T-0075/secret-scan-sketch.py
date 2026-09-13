#!/usr/bin/env python3
"""Pre-sync secret-shape scanner (sketch for ROADMAP 3.8's pre-sync scan).

Rules are SHAPE rules plus one FIELD rule. Values are redacted in output
(first 4 chars + length) so the evidence transcript itself carries no secret.
"""
import re, sys, pathlib

RULES = [
    ("provider-prefixed: openai/anthropic sk-", re.compile(r"\bsk-[A-Za-z0-9_\-]{16,}")),
    ("provider-prefixed: verboo vbk_",         re.compile(r"\bvbk_[A-Za-z0-9_]{20,}")),
    ("provider-prefixed: xai-",                re.compile(r"\bxai-[A-Za-z0-9]{16,}")),
    ("provider-prefixed: github ghp_/github_pat_", re.compile(r"\bgh[pousr]_[A-Za-z0-9]{16,}|\bgithub_pat_[A-Za-z0-9_]{20,}")),
    ("provider-prefixed: gitlab glpat-",       re.compile(r"\bglpat-[A-Za-z0-9_\-]{16,}")),
    ("provider-prefixed: AWS AKIA",            re.compile(r"\bAKIA[0-9A-Z]{16}\b")),
    ("provider-prefixed: google AIza",         re.compile(r"\bAIza[0-9A-Za-z_\-]{35}\b")),
    ("provider-prefixed: huggingface hf_",     re.compile(r"\bhf_[A-Za-z0-9]{20,}")),
    ("jwt",                                    re.compile(r"\beyJ[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{8,}\.[A-Za-z0-9_\-]{4,}")),
    ("pem private key header",                 re.compile(r"-----BEGIN [A-Z ]*PRIVATE KEY-----")),
    ("field-name heuristic: key/token/secret/password", re.compile(
        r"(?i)\b(api[_-]?key|access[_-]?token|auth[_-]?token|bearer[_-]?token|secret|password|passwd)\b\s*[:=]\s*[\"']?([A-Za-z0-9_\-\./\+]{16,})")),
]

ENV_REF = re.compile(r"^\$?\{?[A-Z][A-Z0-9_]*\}?$|^\{env:[A-Z][A-Z0-9_]*\}$|^\$\{[A-Z][A-Z0-9_]*\}$")

def redact(v):
    return f"{v[:4]}…({len(v)} chars)"

def scan(path: pathlib.Path):
    text = path.read_text(errors="replace")
    hits = []
    for lineno, line in enumerate(text.splitlines(), 1):
        for name, rx in RULES:
            for m in rx.finditer(line):
                val = m.group(m.lastindex) if m.lastindex else m.group(0)
                if ENV_REF.match(val):
                    continue  # a reference, not a literal
                hits.append((lineno, name, redact(val), line.strip()[:40], val))
    return hits

targets = [pathlib.Path(p) for p in sys.argv[1:]]
total = 0
for t in targets:
    if not t.exists():
        print(f"--- {t} : (absent)")
        continue
    hits = scan(t)
    total += len(hits)
    print(f"--- {t} : {len(hits)} hit(s)")
    for lineno, name, val, ctx, val_full in hits:
        # NOTE: the context is masked too — the field name is the useful part and
        # the value must never be echoed, not even into a transcript that CI scans.
        masked = re.sub(r"[A-Za-z0-9_\-]{16,}", lambda m: redact(m.group(0)), ctx)
        print(f"    line {lineno}: {name} -> {val}   [field context: {masked}]")
print(f"TOTAL literal-secret hits: {total}")
