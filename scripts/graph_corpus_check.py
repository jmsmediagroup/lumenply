#!/usr/bin/env python3
"""Check that every corpus PSD renders identically through the edit graph.

Runs `lumenply graph FILE --check` (lower to a graph, render it and the
layer tree, compare) over the PSD corpus fetched by psd_corpus.py. With
--gpu, each file is also rendered by the GPU executor and compared with
the CPU evaluator (tolerance 1e-4; skipped where no GPU adapter exists).

    cargo build --release -p lumenply-cli
    scripts/psd_corpus.py fetch                # once
    scripts/graph_corpus_check.py [FILTER...] [-j 6] [--gpu]
"""
import argparse, os, re, subprocess, sys
from collections import Counter
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


def one(f, gpu):
    cmd = [BIN, "graph", f, "--check"] + (["--gpu"] if gpu else [])
    try:
        p = subprocess.run(cmd, capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        return dict(file=f, code="timeout", diff=None, gpu=None, fallback={}, tail="")
    out = (p.stdout + p.stderr).strip()
    m = re.search(r"(?<!gpu )max difference ([0-9.e+-]+)", out)
    g = re.search(r"gpu max difference ([0-9.e+-]+)", out)
    fb = re.search(r"CPU-fallback tiles \(([^)]*)\)", out)
    fallback = Counter()
    if fb and fb.group(1) != "none":
        for part in fb.group(1).split(", "):
            n, op = part.split(" ", 1)
            fallback[op] += int(n)
    return dict(
        file=f,
        code=p.returncode,
        diff=float(m.group(1)) if m else None,
        gpu=float(g.group(1)) if g else None,
        fallback=fallback,
        skipped="no GPU adapter" in out,
        tail=out.splitlines()[-1] if out else "",
    )


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("filters", nargs="*")
    ap.add_argument("-j", "--jobs", type=int, default=6)
    ap.add_argument("--gpu", action="store_true", help="also compare the GPU executor with the CPU")
    a = ap.parse_args()
    todo = files(a.filters)
    if not todo:
        sys.exit(f"no PSDs under {CORPUS}; run scripts/psd_corpus.py fetch first")
    with ThreadPoolExecutor(a.jobs) as ex:
        res = list(ex.map(lambda f: one(f, a.gpu), todo))
    bad = [r for r in res if r["code"] != 0]
    print(f"{len(res)} files: {len(res) - len(bad)} identical, {len(bad)} not")
    if a.gpu:
        ran = [r for r in res if r["gpu"] is not None]
        skipped = sum(1 for r in res if r.get("skipped"))
        worst = max((r["gpu"] for r in ran), default=0.0)
        fallback = sum((r["fallback"] for r in ran), Counter())
        some = sum(1 for r in ran if r["fallback"])
        print(
            f"gpu: {len(ran)} files compared, worst difference {worst:.2e}, {skipped} skipped (no adapter); "
            f"{some} files used the CPU fallback for some tiles ("
            + (", ".join(f"{n} {op}" for op, n in fallback.most_common()) or "none")
            + ")"
        )
    for r in sorted(bad, key=lambda r: -max(r["diff"] or 0, r["gpu"] or 0)):
        print(f"  {r['code']} {r['diff']} gpu {r['gpu']} {os.path.relpath(r['file'], CORPUS)} | {r['tail'][:160]}")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
