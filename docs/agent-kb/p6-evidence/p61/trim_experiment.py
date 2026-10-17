#!/usr/bin/env python3
"""P6.1 design, the road not taken: delete the common words from the keyword sentence and search with the rest.
usage: trim_experiment.py <AgentKB binary> <HOME with semantic OFF>      (needs the real-library copy; fusion as in sim61.py)
A word is common when it is in >= theta % of the sessions (the percentages are in vecall.jsonl). Only sentences of >= 5 words are trimmed."""
import json, os, subprocess, sys, functools
sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import sim61 as s
binary, off = sys.argv[1:3]

@functools.lru_cache(None)
def kw(query):
    d = json.loads(subprocess.run([binary, "--kb-search", query, "--limit", "20"], env={**os.environ, "HOME": off}, capture_output=True).stdout)
    return tuple(r["session"]["session_id"] for r in d["results"]), d["mode_used"]

def run(theta):
    out = {}
    for qid, q, acc in s.Q:
        dfs = s.VEC[qid][2]
        words = [t for t, n, p in dfs if theta is None or p < theta]
        query = q if (theta is None or len(dfs) < 5 or not words or len(words) == len(dfs)) else " ".join(words)
        kl, mode = kw(query)
        o = s.fuse(kl, 1.0 if mode == "all" else 0.5, s.vlist(qid, 0.0, 0.1))
        out.setdefault("C" if qid[0] == "C" else qid[0] + qid[-1], []).append(s.rank(o, acc))
    return out

for th in (None, 15, 20, 30, 40):
    print(s.line(f"common words deleted, theta {th}", run(th)))
