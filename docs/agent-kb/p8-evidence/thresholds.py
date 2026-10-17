#!/usr/bin/env python3
"""P8: how much space each age threshold would save.
usage: thresholds.py <rows.txt> <bench.tsv> [level label]
rows.txt: sqlite3 -separator '|' kb.db "select f.id,f.source,f.mirror_rel,f.mirror_offset,f.is_compressed,f.source_status,coalesce(s.last_activity_at,''),f.src_mtime_ns/1000000000 from files f left join sessions s on s.file_id=f.id"
bench.tsv: output of compress_bench.py (rel relative to raw/)
Groups: live plain mirrors (the candidates), live .zst mirrors (already compressed), plain files no row points to ("orphans").
"""
import sys, datetime as dt
rows = [l.rstrip("\n").split("|") for l in open(sys.argv[1])]
bench = {}
for l in open(sys.argv[2]):
    rel, n, c, t = l.rstrip("\n").split("\t"); bench[rel] = (int(n), int(c), float(t))
now = dt.datetime(2026, 10, 17, 8, 40, tzinfo=dt.timezone.utc)
def ts(r):
    if r[6]: return dt.datetime.fromisoformat(r[6].replace("Z", "+00:00"))
    return dt.datetime.fromtimestamp(int(r[7]), dt.timezone.utc)
live = {r[2] for r in rows}
plain = [r for r in rows if r[4] == "0"]
G = 1e9
print(f"live plain mirrors: {len(plain)} files, {sum(bench[r[2]][0] for r in plain)/G:.3f} GB -> {sum(bench[r[2]][1] for r in plain)/G:.3f} GB")
orph = [k for k in bench if k not in live]
print(f"orphan plain files (no row): {len(orph)} files, {sum(bench[k][0] for k in orph)/G:.3f} GB -> {sum(bench[k][1] for k in orph)/G:.3f} GB")
zs = [r for r in rows if r[4] == "1"]
print(f"live .zst mirrors (compressed by Codex): {len(zs)} files, {sum(int(r[3]) for r in zs)/G:.3f} GB")
print()
print("N days | files | plain GB | compressed GB | saved GB | of live plain total | compress time s")
for n in (7, 14, 30, 60, 90, 180):
    sel = [r for r in plain if (now - ts(r)).days >= n]
    p = sum(bench[r[2]][0] for r in sel); c = sum(bench[r[2]][1] for r in sel); t = sum(bench[r[2]][2] for r in sel)
    tot = sum(bench[r[2]][0] for r in plain)
    print(f"{n:6d} | {len(sel):5d} | {p/G:8.3f} | {c/G:13.3f} | {p/G-c/G:8.3f} | {100*p/tot:5.1f}% | {t:.1f}")
print()
print("by last_activity vs file mtime (src_mtime) for the live plain files: differing by >1 day:",
      sum(1 for r in plain if r[6] and abs((ts(r) - dt.datetime.fromtimestamp(int(r[7]), dt.timezone.utc)).days) > 1), "of", len(plain))
print("live plain files with no last_activity_at (empty sessions):", sum(1 for r in plain if not r[6]))
