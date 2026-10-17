import sys, json, itertools
sys.argv=[sys.argv[0],sys.argv[1] if len(sys.argv)>1 else "bge-small-zh"]
exec(open("vec_eval.py").read().split("K=int(")[0])
def run(fn):
    out={}
    for label,items in sets.items():
        ranks=[rank(fn(q),e) for q,e in items]
        out[label]=(sum(0<r<=5 for r in ranks),sum(0<r<=3 for r in ranks),len(items))
    return out
def vec_top(q,n=20,margin=None):
    v=vec_sessions(q,"top2")[:n]
    if margin is not None and v:
        top=v[0][2]; v=[x for x in v if x[2]>=top-margin]
    return v
print("params                        N-s5 N-k5 | O-s3 O-k3 | sum")
res=[]
for k,wv,nv,mg in itertools.product((5,10,20,60),(0.5,1.0,1.5),(10,20),(None,0.05,0.1)):
    r=run(lambda q:rrf([kw[q]["results"][:20],vec_top(q,nv,mg)],[1,wv],k))
    a=r["NEW sentence"][0]; b=r["NEW keywords"][0]; c=r["OLD sentence"][1]; d=r["OLD keywords"][1]
    res.append((a+b+c+d,a,b,c,d,k,wv,nv,mg))
res.sort(key=lambda x:(x[0],-x[5]),reverse=True)
for x in res[:12]: print(f"k={x[5]} wv={x[6]} nv={x[7]} margin={x[8]}:   {x[1]:>2}  {x[2]:>2} |  {x[3]:>2}  {x[4]:>2} |  {x[0]}")
print("baseline kw only:",run(lambda q:kw[q]["results"]))
