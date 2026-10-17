#!/usr/bin/env python3
"""P8: are the plain files no database row points to ("orphans") just older copies of a session that Codex later compressed?
For each orphan X.jsonl: find the live row X.jsonl.zst, decompress it, and test whether the orphan is a byte prefix of it
(or equal). Reads a COPY of raw/ only.
usage: orphans.py <raw copy> <rows.txt>
"""
import hashlib, os, subprocess, sys
raw, rows_file = sys.argv[1:3]
rows = [l.rstrip("\n").split("|") for l in open(rows_file)]
live = {r[2]: r for r in rows}
res = {"equal": 0, "prefix": 0, "no_twin": 0, "differs": 0}; sizes = {k: 0 for k in res}
detail = []
for d, _, fs in os.walk(raw):
    for f in fs:
        p = os.path.join(d, f); rel = os.path.relpath(p, raw)
        if rel in live: continue
        n = os.path.getsize(p)
        twin = live.get(rel + ".zst")
        if twin is None:
            res["no_twin"] += 1; sizes["no_twin"] += n; detail.append((rel, n, "no twin")); continue
        dec = subprocess.Popen(["zstd", "-dc", os.path.join(raw, twin[2])], stdout=subprocess.PIPE)
        h = hashlib.sha256(); total = 0; ok = True
        with open(p, "rb") as a:
            while True:
                x = a.read(1 << 20)
                if not x: break
                y = dec.stdout.read(len(x))
                while len(y) < len(x):
                    z = dec.stdout.read(len(x) - len(y))
                    if not z: break
                    y += z
                if x != y: ok = False; break
        rest = dec.stdout.read(1) if ok else b""
        dec.kill(); dec.wait()
        kind = "differs" if not ok else ("prefix" if rest else "equal")
        res[kind] += 1; sizes[kind] += n
        if kind == "differs": detail.append((rel, n, kind))
for k in res: print(f"{k:8s} {res[k]:4d} files {sizes[k]/1e9:.3f} GB")
for x in detail[:10]: print("  ", x)
