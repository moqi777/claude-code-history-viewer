#!/usr/bin/env python3
"""P6.1 design simulation. No binary, model or library needed: it replays the fusion of rank.rs on the dumps in this folder.

  vecall.jsonl  per query: [id, text, [(session, vector score, chunks in the session) x80], [(token, sessions, % of sessions) ...]]
                (written by the scratch test in diag61.rs.txt on a copy of the real library, 273 sessions, 24,474 chunks)
  gate.jsonl    per query: [id, keyword mode, keyword result (limit 20, no note promotion), {theta: result after the common-word gate}]
  qall.json     [id, text, accepted session prefixes]: C1-8 = P6-eval.md section 5, N<i>s/N<i>k = section 2 (sentence / keywords), O<i>k/O<i>s = the old 16

  python3 sim61.py            # all the tables of P6.1-design.md
Notes promotion (`note-first`) is NOT in the replay, so one of the old 16 questions that rides on it reads 15/16 instead of 16/16 here.
"""
import json, math, os, sys
D = os.path.dirname(os.path.abspath(__file__))
Q = json.load(open(f"{D}/qall.json"))
VEC = {}
for line in open(f"{D}/vecall.jsonl"):
    qid, q, lst, dfs = json.loads(line)
    VEC[qid] = (q, lst, dfs)
GATE = {}
for line in open(f"{D}/gate.jsonl"):
    qid, mode, all_, bt = json.loads(line)
    GATE[qid] = (mode, all_, {int(t): k for t, k in bt})

K, TOP = 10, 10          # RRF constant, sessions kept from the vector side (P6 values)

def fuse(kwl, wk, vl):
    s = {}
    for r, i in enumerate(kwl[:20]): s[i] = s.get(i, 0) + wk / (K + r + 1)
    for r, i in enumerate(vl): s[i] = s.get(i, 0) + 1.0 / (K + r + 1)
    return sorted(s, key=lambda i: -s[i])

def vlist(qid, alpha, margin):
    _, lst, _ = VEC[qid]
    adj = sorted(((i, sc - alpha * math.log(n)) for i, sc, n in lst), key=lambda x: -x[1])
    return [i for i, sc in adj if sc >= adj[0][1] - margin][:TOP]

def rank(order, acc):
    for n, i in enumerate(order):
        if any(i.startswith(a) for a in acc): return n + 1
    return 0

def run(alpha=0.0, margin=0.1, theta=None):
    out = {}
    for qid, _, acc in Q:
        key = "C" if qid[0] == "C" else qid[0] + qid[-1]
        mode, all_, bt = GATE[qid]
        kl = all_ if theta is None else bt[theta]
        o = fuse(kl, 1.0 if mode == "all" else 0.5, vlist(qid, alpha, margin))
        out.setdefault(key, []).append(rank(o, acc))
    return out

def line(name, o):
    top = lambda key, lim: sum(0 < r <= lim for r in o[key])
    return (f"{name:38s} C {top('C',5)}/8  Ns {top('Ns',5)}/15  Nk {top('Nk',5)}/15  "
            f"Ok {top('Ok',3)}/16  Os {top('Os',3)}/16   C ranks {o['C']}")

if __name__ == "__main__":
    slopes = [float(l.split()[2]) for l in open(f"{D}/slope.txt")]
    print(f"slope of the session score against ln(chunks), all sessions, 70 queries: mean {sum(slopes)/70:.4f}  min {min(slopes):.4f}  max {max(slopes):.4f}\n")
    print("== 1. now (P6 values)");           print(line("alpha 0, margin 0.1", run()))
    print("\n== 2. size correction alone: score - alpha*ln(chunks)")
    for a in (0.01, 0.02, 0.025, 0.03, 0.035, 0.05, 0.08): print(line(f"alpha {a}, margin 0.1", run(a)))
    print("\n== 3. margin (alpha 0.025 / 0.03)")
    for a in (0.025, 0.03):
        for m in (0.1, 0.15, 0.2, 0.25, 0.3, 9): print(line(f"alpha {a}, margin {m}", run(a, m)))
    print("\n== 4. common-word gate on the keyword list (theta = % of sessions that have the word)")
    for a, m in ((0.0, 0.1), (0.025, 0.25)):
        for th in (None, 15, 20, 30, 40): print(line(f"alpha {a}, margin {m}, gate {th}", run(a, m, th)))
