#!/usr/bin/env python3
"""Run P6-eval.md (15 reworded questions, two forms) and the old 16 questions through the real
`--kb-search`, once with semantic search off and once on, and print where the expected session lands.

usage: rank_eval_p6.py <AgentKB binary> <HOME with semantic OFF> <HOME with semantic ON> [--json out.json]

Both HOMEs hold the same library (the same kb.db); they differ only in config.json
(semantic.enabled) and in the vectors/models folders that only the ON one has. Default parameters,
the whole sentence or the keywords exactly as written in queries.py.
"""
import json, os, subprocess, sys, time
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from queries import NEW, OLD

binary, home_off, home_on = sys.argv[1:4]

def search(home, query):
    t = time.time()
    out = subprocess.run([binary, "--kb-search", query], env={**os.environ, "HOME": home}, capture_output=True).stdout
    return json.loads(out), time.time() - t

def rank(data, expected):
    for i, r in enumerate(data["results"]):
        if r["session"]["session_id"].startswith(expected):
            return i + 1
    return 0

groups = {
    "新题-整句": [(f"{i}", s, e) for i, s, k, e in NEW],
    "新题-关键词": [(f"{i}", k, e) for i, s, k, e in NEW],
    "旧题-关键词": [(str(n + 1), k, e) for n, (s, k, e) in enumerate(OLD)],
    "旧题-整句": [(str(n + 1), s, e) for n, (s, k, e) in enumerate(OLD)],
}
result, states = {}, {}
for label, items in groups.items():
    rows = []
    for name, q, e in items:
        off, _ = search(home_off, q)
        on, secs = search(home_on, q)
        states[on["semantic"]["state"]] = states.get(on["semantic"]["state"], 0) + 1
        rows.append((name, rank(off, e), rank(on, e), round(secs, 2), on["mode_used"], off["mode_used"]))
    result[label] = rows
    top5 = lambda j: sum(0 < r[j] <= 5 for r in rows)
    top3 = lambda j: sum(0 < r[j] <= 3 for r in rows)
    first = lambda j: sum(r[j] == 1 for r in rows)
    print(f"\n{label}:  " + "  ".join(f"{n}:{a}/{b}" for n, a, b, *_ in rows))
    print(f"   只用关键词: 前5 {top5(1)}/{len(rows)}  前3 {top3(1)}/{len(rows)}  第1 {first(1)}")
    print(f"   混合(语义开): 前5 {top5(2)}/{len(rows)}  前3 {top3(2)}/{len(rows)}  第1 {first(2)}   (每次搜索耗时中位数 {sorted(r[3] for r in rows)[len(rows)//2]} s)")
print("\n语义状态计数:", states)
if "--json" in sys.argv:
    json.dump(result, open(sys.argv[sys.argv.index("--json") + 1], "w"), ensure_ascii=False)
