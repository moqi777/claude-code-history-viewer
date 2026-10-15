import os,subprocess,sys,time,json
out='/Users/mo7/Dev/JavaCode/claude-code-history-viewer/scratch/p4/lt'
open(out+'/parent.log','w').write(json.dumps({'path':os.environ.get('PATH'),'pid':os.getpid(),'pgid':os.getpgid(0),'t':time.time()})+'\n')
inp=open(out+'/in.txt','rb')
p=subprocess.Popen(['/Users/mo7/.local/bin/claude','-p','--no-session-persistence','--safe-mode','--tools','','--output-format','json','--model','haiku'],
  cwd=out+'/work',stdin=inp,stdout=open(out+'/child.out','wb'),stderr=open(out+'/child.err','wb'),start_new_session=True)
open(out+'/parent.log','a').write(json.dumps({'child':p.pid,'child_pgid':os.getpgid(p.pid)})+'\n')
sys.exit(0)
