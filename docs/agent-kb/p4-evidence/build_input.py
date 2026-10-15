"""Prototype of the digest input builder (the Rust version follows the same rules).
usage: build_input.py <session_id prefix> [from_seq] [char_budget] [prev_notes.json] > input.txt"""
import sqlite3,sys,json
U,A,TU,TE=1200,1500,160,300
db=sqlite3.connect('kb-copy.db')
pref=sys.argv[1]; from_seq=int(sys.argv[2]) if len(sys.argv)>2 else 0
budget=int(sys.argv[3]) if len(sys.argv)>3 else 150000
prev=json.load(open(sys.argv[4])) if len(sys.argv)>4 else None
s=db.execute("select file_id,source,session_id,title,project_name,cwd,started_at,last_activity_at from sessions where session_id like ? and is_subagent=0",(pref+'%',)).fetchone()
fid,src,sid,title,proj,cwd,st,la=s
def clip(t,n,tail=0):
    t=t.strip()
    if len(t)<=n: return t
    if tail: return t[:n-tail]+' […] '+t[-tail:]
    return t[:n]+' […]'
out=[];used=0;last=None;first=None
for seq,kind,ts,text in db.execute("select seq,kind,ts,text from messages where file_id=? and seq>? order by seq",(fid,from_seq)):
    if kind=='user': h,b='user',clip(text,U)
    elif kind=='assistant': h,b='assistant',clip(text,A,tail=400)
    elif kind=='tool_use': h,b='tool',clip(text.replace('\n',' '),TU)
    elif kind=='tool_error': h,b='tool-error',clip(text,TE)
    else: continue
    line=f"[#{seq} {h}{' '+ts[5:16] if ts and kind in ('user',) else ''}] {b}"
    if used+len(line)>budget and kind=='user' and out: break   # cut at a turn start
    out.append(line); used+=len(line)+1; last=seq; first=first or seq
print(f"<session>\nsource: {src}\ntitle: {title}\nproject: {proj}\ncwd: {cwd}\nactive: {st} .. {la}\n</session>")
if prev: print("<previous_notes>\n"+json.dumps(prev,ensure_ascii=False)+"\n</previous_notes>")
print(f"<messages from=#{first} to=#{last}>\n"+"\n".join(out)+"\n</messages>")
sys.stderr.write(f"session={sid} first={first} last={last} msgs={len(out)} chars={used}\n")
