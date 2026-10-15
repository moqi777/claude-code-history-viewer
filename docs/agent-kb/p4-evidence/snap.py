import os,sys,json
out={}
for root in [os.path.expanduser('~/.claude'),os.path.expanduser('~/.codex')]:
    for d,ds,fs in os.walk(root):
        for f in fs:
            p=os.path.join(d,f)
            try:
                st=os.lstat(p); out[p]=[st.st_size,int(st.st_mtime)]
            except OSError: pass
json.dump(out,open(sys.argv[1],'w'))
print(len(out))
