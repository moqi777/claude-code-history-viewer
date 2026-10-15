import json,subprocess,sys,os,re
BIN=sys.argv[1]; DATA=sys.argv[2]; EXTRA=sys.argv[3:]
rows=[]
for line in open('docs/agent-kb/P3-eval.md'):
    m=re.match(r'\|\s*(\d+)\s*\|\s*(.+?)\s*\|\s*`(.+?)`\s*\|\s*(claude|codex) `(\w+)`',line)
    if m: rows.append((int(m[1]),m[2],m[3],m[4],m[5]))
assert len(rows)==15
def run(q):
    env=dict(os.environ,AGENTKB_DATA_DIR=DATA)
    out=subprocess.run([BIN,'--kb-search',q]+EXTRA,capture_output=True,text=True,env=env)
    return out.stdout
tot={'sentence':[0,0],'keywords':[0,0]}
for n,sent,kw,src,sid in rows:
    line=[f'#{n}']
    for name,q in (('sentence',sent),('keywords',kw)):
        raw=run(q); d=json.loads(raw)
        ids=[r['session']['session_id'] for r in d['results']]
        pos=next((i+1 for i,x in enumerate(ids) if x.startswith(sid)),None)
        tot[name][0]+=pos is not None; tot[name][1]+=len(raw.encode())
        line.append(f"{name}: {'@'+str(pos) if pos else 'miss'} {len(raw.encode())}B")
    print(' | '.join(line))
print({k:{'hits':v[0],'avg_bytes':v[1]//15} for k,v in tot.items()})
