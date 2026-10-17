#!/usr/bin/env python3
"""Semantic search OFF: 0.6.1 must answer exactly like 0.6.0. usage: off_same.py <old binary> <new binary> <HOME with semantic OFF>
Compares the whole JSON (session ids, order, scores, hits) for the 70 evaluation queries plus 电池 耗电 and a short keyword query."""
import json, os, subprocess, sys
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from queries import NEW, OLD
from queries61 import COLLOQUIAL
old, new, home = sys.argv[1:4]
qs = [q for _, q, _ in COLLOQUIAL] + [x for i, s, k, e in NEW for x in (s, k)] + [x for s, k, e in OLD for x in (s, k)] + ["电池 耗电", "tedis 依赖"]
def run(b, q):
    out = subprocess.run([b, "--kb-search", q, "--limit", "20"], env={**os.environ, "HOME": home}, capture_output=True).stdout.decode()
    d = json.loads(out.replace(b, "EXE"))      # the commands in the output name the binary
    d.pop("data_as_of", None)
    return d
bad = [q for q in qs if run(old, q) != run(new, q)]
print(f"{len(qs)} queries, {len(bad)} differ", bad)
