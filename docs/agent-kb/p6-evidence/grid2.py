import sys, itertools
name=sys.argv[1]; sys.argv=[sys.argv[0],name]
exec(open("vec_eval.py").read().split("K=int(")[0])
def run(fn):
    o={}
    for label,items in sets.items():
        rs=[rank(fn(q),e) for q,e in items]
        o[label]=(sum(0<r<=5 for r in rs),sum(0<r<=3 for r in rs),sum(r==1 for r in rs))
    return o
res=[]
for k,wany,wvec,mg,nv in itertools.product((10,60),(0.3,0.5,1.0),(1.0,1.5),(0.05,0.1,0.15),(10,)):
    def fn(q):
        v=vec_sessions(q,"top2")[:nv]
        if v: v=[x for x in v if x[2]>=v[0][2]-mg]
        w=1.0 if kw[q]["mode"]=="all" else wany
        return rrf([kw[q]["results"][:20],v],[w,wvec],k)
    r=run(fn)
    ns,nk,os_,ok=r["NEW sentence"],r["NEW keywords"],r["OLD sentence"],r["OLD keywords"]
    res.append((ns[0]+nk[0]+ns[1]+nk[1], os_[1]+ok[1], ns,nk,os_,ok,k,wany,wvec,mg))
res=[x for x in res if x[1]==32] or res
res.sort(key=lambda x:-x[0])
for x in res[:8]: print(f"k={x[6]} w_any={x[7]} w_vec={x[8]} margin={x[9]}  NEWs(5/3/1)={x[2]} NEWk={x[3]} OLDs={x[4]} OLDk={x[5]}")
