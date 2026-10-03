#!/usr/bin/env python3
"""Check that every corpus PSD renders identically through the edit graph.

Runs `lumenply graph FILE --check` (lower to a graph, render it and the
layer tree, compare) over the PSD corpus fetched by psd_corpus.py.

    cargo build --release -p lumenply-cli
    scripts/psd_corpus.py fetch                # once
    scripts/graph_corpus_check.py [FILTER...] [-j 6]
"""
import argparse, os, re, subprocess, sys
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "target", "psd-corpus")
BIN = os.path.join(ROOT, "target", "release", "lumenply")


def files(filters):
    found = []
    for d, _, names in os.walk(CORPUS, followlinks=True):
        if os.sep + "out" in d:
            continue
        found += [os.path.join(d, n) for n in names if n.lower().endswith((".psd", ".psb"))]
    return sorted(f for f in found if not filters or any(s in f for s in filters))


def one(f):
    try:
        p = subprocess.run([BIN, "graph", f, "--check"], capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        return f, "timeout", None, ""
    out = (p.stdout + p.stderr).strip()
    m = re.search(r"max difference ([0-9.e+-]+)", out)
    return f, p.returncode, float(m.group(1)) if m else None, out.splitlines()[-1] if out else ""


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("filters", nargs="*")
    ap.add_argument("-j", "--jobs", type=int, default=6)
    a = ap.parse_args()
    todo = files(a.filters)
    if not todo:
        sys.exit(f"no PSDs under {CORPUS}; run scripts/psd_corpus.py fetch first")
    with ThreadPoolExecutor(a.jobs) as ex:
        res = list(ex.map(one, todo))
    bad = [r for r in res if r[1] != 0]
    print(f"{len(res)} files: {len(res) - len(bad)} identical, {len(bad)} not")
    for f, code, diff, tail in sorted(bad, key=lambda r: -(r[2] or 0)):
        print(f"  {code} {diff} {os.path.relpath(f, CORPUS)} | {tail[:160]}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
