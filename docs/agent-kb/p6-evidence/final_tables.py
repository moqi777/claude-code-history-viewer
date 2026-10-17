import sys
name=sys.argv[1]; margin=float(sys.argv[2]); sys.argv=[sys.argv[0],name]
exec(open("vec_eval.py").read().split("K=int(")[0])
def vtop(q,n=10):
    v=vec_sessions(q,"top2")[:n]
    if v: v=[x for x in v if x[2]>=v[0][2]-margin]
    return v
hyb=lambda q:rrf([kw[q]["results"][:20],vtop(q)],[1.0 if kw[q]["mode"]=="all" else 0.5,1],10)
def row(label,q,e):
    return (rank(kw[q]["results"],e),rank(vec_sessions(q,"top2"),e),rank(hyb(q),e))
tot={}
for label,items in [("新题-整句",[(f"{i}",s,e) for i,s,k,e in NEW]),("新题-关键词",[(f"{i}",k,e) for i,s,k,e in NEW]),("旧题-关键词",[(str(n+1),k,e) for n,(s,k,e) in enumerate(OLD)]),("旧题-整句",[(str(n+1),s,e) for n,(s,k,e) in enumerate(OLD)])]:
    rs=[row(label,q,e) for _,q,e in items]
    print(f"\n{label}:  "+"  ".join(f"{n}:{a}/{b}/{c}" for (n,_,_),(a,b,c) in zip(items,rs)))
    for j,nm in enumerate(("关键词","向量","混合")):
        print(f"   {nm}: 前5 {sum(0<r[j]<=5 for r in rs)}/{len(rs)}  前3 {sum(0<r[j]<=3 for r in rs)}/{len(rs)}  第1 {sum(r[j]==1 for r in rs)}")
