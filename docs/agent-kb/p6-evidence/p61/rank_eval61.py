#!/usr/bin/env python3
"""P6.1 acceptance: the 8 colloquial questions + P6's 15 reworded (sentence / keywords) + the old 16 (keywords / sentence),
through the real `--kb-search` on ONE snapshot of the library, development sessions removed from the results before ranking.

usage: rank_eval61.py <binary> <HOME with semantic ON> <dev-ids file> [--json out.json]
  dev-ids file: one session id per line (AgentKB's own development sessions, which contain the test questions verbatim)
Prints, per group, the rank of the expected session (0 = not in the first 20) and the count within the top 5 (old 16: top 3).
"""
import json, os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
sys.path.insert(0, os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
from queries import NEW, OLD
from queries61 import COLLOQUIAL
binary, home, devfile = sys.argv[1:4]
DEV = {l.strip() for l in open(devfile) if l.strip()}

def search(q):
    t = time.time()
    d = json.loads(subprocess.run([binary, "--kb-search", q, "--limit", "20"], env={**os.environ, "HOME": home}, capture_output=True).stdout)
    return d, time.time() - t

def rank(d, acc):
    n = 0
    for r in d["results"]:
        if r["session"]["session_id"] in DEV: continue
        n += 1
        if any(r["session"]["session_id"].startswith(a) for a in acc): return n
    return 0

groups = {
    "口语8": [(str(n), q, acc) for n, q, acc in COLLOQUIAL],
    "新15-整句": [(str(i), s, [e]) for i, s, k, e in NEW],
    "新15-关键词": [(str(i), k, [e]) for i, s, k, e in NEW],
    "旧16-关键词": [(str(n + 1), k, [e]) for n, (s, k, e) in enumerate(OLD)],
    "旧16-整句": [(str(n + 1), s, [e]) for n, (s, k, e) in enumerate(OLD)],
}
out, secs, states = {}, [], {}
for label, items in groups.items():
    rows = []
    for name, q, acc in items:
        d, t = search(q); secs.append(t); states[d["semantic"]["state"]] = states.get(d["semantic"]["state"], 0) + 1
        rows.append((name, rank(d, acc)))
    out[label] = rows
    lim = 3 if label.startswith("旧") else 5
    print(f"{label}: " + " ".join(f"{n}:{r}" for n, r in rows))
    print(f"   前{lim}: {sum(0 < r <= lim for _, r in rows)}/{len(rows)}")
bm = json.loads(subprocess.run([binary, "--kb-search", "电池 耗电", "--limit", "20"], env={**os.environ, "HOME": home}, capture_output=True).stdout)
print("电池 耗电 -> a3dc8377 rank:", rank(bm, ["a3dc8377"]))
print("semantic states:", states)
if "--json" in sys.argv: json.dump(out, open(sys.argv[sys.argv.index("--json") + 1], "w"), ensure_ascii=False)
