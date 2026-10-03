#!/usr/bin/env python3
"""Check that every corpus PSD renders identically through the edit graph.

Runs `lumenply graph FILE --check` (lower to a graph, render it and the
layer tree, compare) over the PSD corpus fetched by psd_corpus.py.

With --roundtrip each file also goes through a graph project (.lumen
format 3, ADR 0026): `graph FILE --check --save T.lumen --hints` saves the
graph with render hints for its output and loads it back in-process, then
a second process runs `graph T.lumen --check`, which renders the loaded
file with and without its hints. Both renders must be bit-identical to the
first process's graph render (the one --check compared with the layer
tree), by their printed render digests.

    cargo build --release -p lumenply-cli
    scripts/psd_corpus.py fetch                # once
    scripts/graph_corpus_check.py [FILTER...] [-j 6] [--roundtrip]
"""
import argparse, os, re, shutil, subprocess, sys, tempfile
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


def run(args):
    try:
        p = subprocess.run([BIN] + args, capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        return "timeout", ""
    return p.returncode, (p.stdout + p.stderr).strip()


def digest(out):
    m = re.search(r"render digest ([0-9a-f]{64})", out)
    return m.group(1) if m else None


def one(i, f, tmp):
    """(file, exit code or reason, max difference, last output line, v3 size)."""
    if tmp is None:
        code, out = run(["graph", f, "--check"])
        m = re.search(r"max difference ([0-9.e+-]+)", out)
        return f, code, float(m.group(1)) if m else None, out.splitlines()[-1] if out else "", None
    lumen = os.path.join(tmp, f"{i}.lumen")
    try:
        code, out = run(["graph", f, "--check", "--save", lumen, "--hints"])
        m = re.search(r"max difference ([0-9.e+-]+)", out)
        diff = float(m.group(1)) if m else None
        tail = out.splitlines()[-1] if out else ""
        if code != 0:
            return f, code, diff, tail, None
        size = os.path.getsize(lumen)
        code2, out2 = run(["graph", lumen, "--check"])
        if code2 != 0:
            return f, f"reload {code2}", diff, (out2.splitlines() or [""])[-1], size
        a, b = digest(out), digest(out2)
        if a is None or a != b:
            return f, "digest", diff, f"{a} != {b}", size
        return f, 0, diff, tail, size
    finally:
        for p in (lumen, lumen + ".tmp"):
            if os.path.exists(p):
                os.remove(p)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("filters", nargs="*")
    ap.add_argument("-j", "--jobs", type=int, default=6)
    ap.add_argument("--roundtrip", action="store_true", help="also save each graph as a .lumen v3 and reload it")
    a = ap.parse_args()
    todo = files(a.filters)
    if not todo:
        sys.exit(f"no PSDs under {CORPUS}; run scripts/psd_corpus.py fetch first")
    tmp = tempfile.mkdtemp(prefix="lumenply-graph-corpus-") if a.roundtrip else None
    try:
        with ThreadPoolExecutor(a.jobs) as ex:
            res = list(ex.map(lambda job: one(job[0], job[1], tmp), enumerate(todo)))
    finally:
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)
    bad = [r for r in res if r[1] != 0]
    what = "identical, saved and reloaded bit-identically" if a.roundtrip else "identical"
    print(f"{len(res)} files: {len(res) - len(bad)} {what}, {len(bad)} not")
    if a.roundtrip:
        psd = sum(os.path.getsize(r[0]) for r in res if r[4] is not None)
        v3 = sum(r[4] for r in res if r[4] is not None)
        print(f"PSDs {psd / 1e6:.1f} MB, graph projects with output hints {v3 / 1e6:.1f} MB")
    for f, code, diff, tail, _ in sorted(bad, key=lambda r: -(r[2] or 0)):
        print(f"  {code} {diff} {os.path.relpath(f, CORPUS)} | {tail[:160]}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
