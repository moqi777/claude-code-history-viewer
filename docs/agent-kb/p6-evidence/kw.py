"""Keyword results from the installed 0.5.0 binary on the clone: session id lists, cached to kw.json."""
import json, subprocess, os, sys
from queries import NEW, OLD
HOME="/private/tmp/p6work/home"; BIN="/Applications/AgentKB.app/Contents/MacOS/AgentKB"
def run(q):
    out=subprocess.run([BIN,"--kb-search",q,"--limit","30"],env={**os.environ,"HOME":HOME},capture_output=True).stdout
    d=json.loads(out)
    return {"results":[(r["session"]["source"],r["session"]["session_id"],r["score"],bool(r.get("has_note")),bool(r["hits"])) for r in d["results"]],
            "notes":[(n["session"]["source"],n["session"]["session_id"],n["score"]) for n in d.get("notes",[])],"mode":d["mode_used"]}
qs=set()
for i,s,k,e in NEW: qs.add(s); qs.add(k)
for s,k,e in OLD: qs.add(s); qs.add(k)
res={q:run(q) for q in sorted(qs)}
json.dump(res,open("/private/tmp/p6work/kw.json","w"),ensure_ascii=False)
print(len(res),"queries")
