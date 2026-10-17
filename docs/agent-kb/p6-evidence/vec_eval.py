"""usage: vec_eval.py <model name> [--run]   (needs vec-<name>.bin; --run executes the spike query step)"""
import json, sqlite3, subprocess, sys, collections
from queries import NEW, OLD
W="/private/tmp/p6work"
name=sys.argv[1]
queries=[]
for i,s,k,e in NEW: queries += [s,k]
for s,k,e in OLD: queries += [s,k]
queries=sorted(set(queries))
json.dump(queries,open(f"{W}/queries.json","w"),ensure_ascii=False)
if "--run" in sys.argv:
    th=sys.argv[sys.argv.index("--threads")+1] if "--threads" in sys.argv else "4"
    out=subprocess.run([f"{W}/spike/target/release/spike","query",f"{W}/models/{name}",f"{W}/vec-{name}.bin",f"{W}/queries.json",f"{W}/vq-{name}.json","--threads",th],capture_output=True,text=True)
    print(out.stdout.strip()); 
meta={int(k):v for k,v in json.load(open(f"{W}/chunks.jsonl.meta.json")).items()}
db=sqlite3.connect(f"{W}/kb.db")
fid2s={r[0]:(r[1],r[2]) for r in db.execute("select file_id,source,session_id from sessions")}
vq={r["q"]:r["top"] for r in json.load(open(f"{W}/vq-{name}.json"))}
kw=json.load(open(f"{W}/kw.json"))
def vec_sessions(q, agg="max"):
    best=collections.OrderedDict(); 
    per=collections.defaultdict(list)
    for cid,score in vq[q]:
        fid=meta[cid][0]
        per[fid].append(score)
    sc={}
    for fid,l in per.items():
        l.sort(reverse=True)
        sc[fid]=l[0]+(0.25*l[1] if agg=="top2" and len(l)>1 else 0)
    return [(fid2s[f][0],fid2s[f][1],s) for f,s in sorted(sc.items(),key=lambda x:-x[1])]
def rrf(lists, weights, k=60):
    sc=collections.defaultdict(float)
    for l,w in zip(lists,weights):
        for r,(src,sid,*_) in enumerate(l):
            sc[(src,sid)]+=w/(k+r+1)
    return [(a,b) for (a,b),_ in sorted(sc.items(),key=lambda x:-x[1])]
def rank(lst, exp):
    for i,it in enumerate(lst):
        if it[1].startswith(exp): return i+1
    return 0
def summary(label, items, fn):
    r5=r3=0; ranks=[]
    for q,e in items:
        r=rank(fn(q),e); ranks.append(r); r5+=0<r<=5; r3+=0<r<=3
    print(f"{label:34} top5 {r5:>2}/{len(items)} top3 {r3:>2}/{len(items)}  ranks {ranks}")
    return ranks
sets={"NEW sentence":[(s,e) for i,s,k,e in NEW],"NEW keywords":[(k,e) for i,s,k,e in NEW],"OLD keywords":[(k,e) for s,k,e in OLD],"OLD sentence":[(s,e) for s,k,e in OLD]}
K=int(sys.argv[sys.argv.index("--k")+1]) if "--k" in sys.argv else 60
for label,items in sets.items():
    print("==",label)
    summary("keyword only",items,lambda q:kw[q]["results"])
    summary("vector only (max)",items,lambda q:vec_sessions(q))
    summary("vector only (top2)",items,lambda q:vec_sessions(q,"top2"))
    for w in (0.5,1.0,2.0):
        summary(f"RRF k={K} kw:1 vec:{w}",items,lambda q:rrf([kw[q]["results"],vec_sessions(q)],[1,w],K))
