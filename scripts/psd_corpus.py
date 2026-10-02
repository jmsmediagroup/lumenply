#!/usr/bin/env python3
"""PSD fidelity check against real Photoshop files.

Fetches the public test fixtures of psd-tools and ag-psd (about 480 files
saved by Photoshop), renders each with the lumenply CLI and compares the
result with the composite Photoshop stored inside the file (read with
psd-tools). Differences are mean / 99th-percentile absolute error on a
0-255 scale, both composited over white.

    pip install psd-tools numpy pillow
    cargo build --release -p lumenply-cli
    scripts/psd_corpus.py fetch                 # once, into target/psd-corpus
    scripts/psd_corpus.py run [FILTER...] [-o results.json]
    scripts/psd_corpus.py diff before.json after.json

`run` prints the worst files (all of them when filters are given);
`diff` lists the files whose difference changed. Keep the result of a
run from before your change and diff against it afterwards.
"""
import argparse
import json
import os
import subprocess
import sys
import time
from concurrent.futures import ThreadPoolExecutor

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
CORPUS = os.path.join(ROOT, "target", "psd-corpus")
BIN = os.path.join(ROOT, "target", "release", "lumenply")
SOURCES = [
    ("psd-tools", "https://github.com/psd-tools/psd-tools.git", "tests/psd_files"),
    ("ag-psd", "https://github.com/Agamnentzar/ag-psd.git", "test"),
]


def fetch():
    os.makedirs(CORPUS, exist_ok=True)
    for name, url, path in SOURCES:
        dest = os.path.join(CORPUS, name)
        if os.path.isdir(dest):
            print(f"{name}: already fetched")
            continue
        subprocess.run(
            ["git", "clone", "-q", "--depth", "1", "--filter=blob:none", "--sparse", url, dest],
            check=True,
        )
        subprocess.run(["git", "-C", dest, "sparse-checkout", "set", path], check=True)
    print(f"{len(files())} files in {CORPUS}")


def files():
    found = []
    for name, _, _ in SOURCES:
        for root, _, names in os.walk(os.path.join(CORPUS, name)):
            found += [os.path.join(root, n) for n in names if n.lower().endswith((".psd", ".psb"))]
    return sorted(found)


def over_white(img):
    import numpy as np

    a = np.asarray(img.convert("RGBA")).astype(np.float32) / 255.0
    return a[..., :3] * a[..., 3:4] + (1 - a[..., 3:4])


def one(path, out):
    import numpy as np
    from PIL import Image
    from psd_tools import PSDImage

    rel = os.path.relpath(path, CORPUS)
    png = os.path.join(out, rel.replace("/", "__") + ".png")
    rec = {"file": rel}
    ref = None
    try:
        psd = PSDImage.open(path)
        rec.update(mode=str(psd.color_mode), depth=psd.depth)
        ref = psd.topil()
    except Exception as e:  # noqa: BLE001 - psd-tools fails on some fixtures
        rec["reference_error"] = repr(e)[:200]
    t = time.time()
    try:
        p = subprocess.run([BIN, "render", "--out", png, path], capture_output=True, text=True, timeout=120)
        rec.update(code=p.returncode, stderr=p.stderr.strip()[-600:])
    except subprocess.TimeoutExpired:
        rec["code"] = "timeout"
    rec["secs"] = round(time.time() - t, 2)
    if rec.get("code") == 0 and ref is not None and os.path.exists(png):
        ours = Image.open(png)
        if ours.size != ref.size:
            rec["size_mismatch"] = [ours.size, ref.size]
        else:
            d = np.abs(over_white(ours) - over_white(ref))
            rec["mean_diff"] = round(float(d.mean()) * 255, 2)
            rec["p99_diff"] = round(float(np.percentile(d.max(axis=2), 99)) * 255, 1)
            ref.save(png[:-4] + ".ref.png")
    return rec


def run(filters, out_json, workers):
    todo = [f for f in files() if not filters or any(s in f for s in filters)]
    if not todo:
        sys.exit("no files: run `scripts/psd_corpus.py fetch` first (or check the filters)")
    out = os.path.join(CORPUS, "out")
    os.makedirs(out, exist_ok=True)
    with ThreadPoolExecutor(max_workers=workers) as ex:
        results = list(ex.map(lambda f: one(f, out), todo))
    with open(out_json, "w") as f:
        json.dump(results, f, indent=1)
    failed = [r for r in results if r.get("code") != 0]
    close = [r for r in results if r.get("mean_diff", 99) < 1]
    print(f"{len(results)} files: {len(failed)} failed to render, {len(close)} within 1/255 on average")
    shown = sorted(results, key=lambda r: -r.get("mean_diff", 0))
    for r in shown if filters else shown[:25]:
        msg = (r.get("stderr") or "").replace("\n", " | ")[:160]
        print(f"{r.get('mean_diff', '-'):>7} {r.get('p99_diff', '-'):>6}  {r['file']}  {msg}")
    print(f"results: {out_json}; images (ours and .ref): {out}")


def diff(before, after):
    b = {r["file"]: r for r in json.load(open(before))}
    a = {r["file"]: r for r in json.load(open(after))}
    rows = []
    for f, r in a.items():
        old = b.get(f, {})
        if old.get("code") != r.get("code"):
            rows.append((0, f"{f}: exit {old.get('code')} -> {r.get('code')}"))
        x, y = old.get("mean_diff"), r.get("mean_diff")
        if x is not None and y is not None and abs(x - y) > 0.05:
            rows.append((y - x, f"{x:7.2f} -> {y:7.2f}  {f}"))
    for _, line in sorted(rows):
        print(line)
    print(f"{sum(d < 0 for d, _ in rows)} better, {sum(d > 0 for d, _ in rows)} worse")


if __name__ == "__main__":
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sub.add_parser("fetch")
    r = sub.add_parser("run")
    r.add_argument("filters", nargs="*")
    r.add_argument("-o", "--out", default=os.path.join(CORPUS, "results.json"))
    r.add_argument("-j", "--jobs", type=int, default=4)
    d = sub.add_parser("diff")
    d.add_argument("before")
    d.add_argument("after")
    args = ap.parse_args()
    if args.cmd == "fetch":
        fetch()
    elif args.cmd == "run":
        run(args.filters, args.out, args.jobs)
    else:
        diff(args.before, args.after)
