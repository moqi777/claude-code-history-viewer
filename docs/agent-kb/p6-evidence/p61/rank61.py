#!/usr/bin/env python3
"""P6.1: where do the colloquial questions land? usage: rank61.py <binary> <HOME off> <HOME on> [detail]
Prints rank (0 = not in results) with semantic off / on, and with `detail` the full hybrid list."""
import json, os, subprocess, sys
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from queries61 import COLLOQUIAL
binary, off, on = sys.argv[1:4]
def run(home, q):
    return json.loads(subprocess.run([binary, "--kb-search", q, "--limit", "20"], env={**os.environ, "HOME": home}, capture_output=True).stdout)
def rank(d, acc):
    for i, r in enumerate(d["results"]):
        if any(r["session"]["session_id"].startswith(a) for a in acc): return i + 1
    return 0
hit = [0, 0]
for n, q, acc in COLLOQUIAL:
    a, b = run(off, q), run(on, q)
    ra, rb = rank(a, acc), rank(b, acc)
    hit[0] += 0 < ra <= 5; hit[1] += 0 < rb <= 5
    print(f"{n}: keyword {ra}  hybrid {rb}  ({b['semantic']['state']}, {len(b['results'])} results)  {q}")
    if len(sys.argv) > 4:
        for i, r in enumerate(b["results"]):
            print(f"     {i+1} {r['session']['session_id'][:8]} {r.get('match')} score={r['score']:.3f}  {r['session']['title'][:36]}")
print(f"top-5: keyword {hit[0]}/8  hybrid {hit[1]}/8")
