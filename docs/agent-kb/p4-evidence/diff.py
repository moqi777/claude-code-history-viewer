import json,sys,subprocess
subprocess.run(['python3','snap.py','cur.json'],capture_output=True)
a=json.load(open(sys.argv[1]));b=json.load(open('cur.json'))
new=[p for p in b if p not in a]; gone=[p for p in a if p not in b]; chg=[p for p in b if p in a and a[p]!=b[p]]
print('new',len(new),'gone',len(gone),'changed',len(chg))
for p in new[:40]: print(' +',p,b[p])
for p in gone[:20]: print(' -',p)
for p in chg[:25]: print(' ~',p)
