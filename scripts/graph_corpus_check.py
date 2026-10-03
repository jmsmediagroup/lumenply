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

With --gpu each file is also rendered by the GPU executor (ADR 0027) and
compared with the CPU evaluator (tolerance 1e-4; skipped where no GPU
adapter exists), and the tiles that fell back to the CPU are tallied.

    cargo build --release -p lumenply-cli
    scripts/psd_corpus.py fetch                # once
    scripts/graph_corpus_check.py [FILTER...] [-j 6] [--roundtrip] [--gpu]
"""
import argparse, os, re, shutil, subprocess, sys, tempfile
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


def run(args):
    try:
        p = subprocess.run([BIN] + args, capture_output=True, text=True, timeout=600)
    except subprocess.TimeoutExpired:
        return "timeout", ""
    return p.returncode, (p.stdout + p.stderr).strip()


def digest(out):
    m = re.search(r"render digest ([0-9a-f]{64})", out)
    return m.group(1) if m else None


def gpu_result(out):
    """The GPU comparison's difference and CPU-fallback tiles by op."""
    g = re.search(r"gpu max difference ([0-9.e+-]+)", out)
    fallback = Counter()
    fb = re.search(r"CPU-fallback tiles \(([^)]*)\)", out)
    if fb and fb.group(1) != "none":
        for part in fb.group(1).split(", "):
            n, op = part.split(" ", 1)
            fallback[op] += int(n)
    return (float(g.group(1)) if g else None), fallback


def one(i, f, tmp, gpu):
    """file, exit code or reason, max difference, last output line, v3 size,
    GPU difference, GPU fallback tiles, whether the GPU was skipped."""
    r = dict(file=f, code=0, diff=None, tail="", size=None, gpu=None, fallback=Counter(), skipped=False)
    lumen = os.path.join(tmp, f"{i}.lumen") if tmp else None
    try:
        args = ["graph", f, "--check"] + (["--save", lumen, "--hints"] if lumen else []) + (["--gpu"] if gpu else [])
        code, out = run(args)
        m = re.search(r"(?<!gpu )max difference ([0-9.e+-]+)", out)
        r.update(code=code, diff=float(m.group(1)) if m else None, tail=out.splitlines()[-1] if out else "")
        r["gpu"], r["fallback"] = gpu_result(out)
        r["skipped"] = "no GPU adapter" in out
        if code != 0 or not lumen:
            return r
        r["size"] = os.path.getsize(lumen)
        code2, out2 = run(["graph", lumen, "--check"])
        if code2 != 0:
            r.update(code=f"reload {code2}", tail=(out2.splitlines() or [""])[-1])
            return r
        a, b = digest(out), digest(out2)
        if a is None or a != b:
            r.update(code="digest", tail=f"{a} != {b}")
        return r
    finally:
        if lumen:
            for p in (lumen, lumen + ".tmp"):
                if os.path.exists(p):
                    os.remove(p)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("filters", nargs="*")
    ap.add_argument("-j", "--jobs", type=int, default=6)
    ap.add_argument("--roundtrip", action="store_true", help="also save each graph as a .lumen v3 and reload it")
    ap.add_argument("--gpu", action="store_true", help="also compare the GPU executor with the CPU")
    a = ap.parse_args()
    todo = files(a.filters)
    if not todo:
        sys.exit(f"no PSDs under {CORPUS}; run scripts/psd_corpus.py fetch first")
    tmp = tempfile.mkdtemp(prefix="lumenply-graph-corpus-") if a.roundtrip else None
    try:
        with ThreadPoolExecutor(a.jobs) as ex:
            res = list(ex.map(lambda job: one(job[0], job[1], tmp, a.gpu), enumerate(todo)))
    finally:
        if tmp:
            shutil.rmtree(tmp, ignore_errors=True)
    bad = [r for r in res if r["code"] != 0]
    what = "identical, saved and reloaded bit-identically" if a.roundtrip else "identical"
    print(f"{len(res)} files: {len(res) - len(bad)} {what}, {len(bad)} not")
    if a.roundtrip:
        psd = sum(os.path.getsize(r["file"]) for r in res if r["size"] is not None)
        v3 = sum(r["size"] for r in res if r["size"] is not None)
        print(f"PSDs {psd / 1e6:.1f} MB, graph projects with output hints {v3 / 1e6:.1f} MB")
    if a.gpu:
        ran = [r for r in res if r["gpu"] is not None]
        skipped = sum(1 for r in res if r["skipped"])
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
