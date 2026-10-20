#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-only
"""Owned, model-free Herdr protocol-22 conformance.

Run through scripts/no-pilot-env.sh. Arguments: artifact directory, cs binary.
Uses only newly created short /tmp roots and one unique private Herdr session.
Retains the throwaway galaxy; always stops/deletes the owned Herdr session.
"""
import os,sys,time,json,socket,tempfile,subprocess,pathlib,shutil
D=pathlib.Path(sys.argv[1]).resolve()
D.mkdir(parents=True,exist_ok=True)
CS=str(pathlib.Path(sys.argv[2]).resolve())
root=pathlib.Path(tempfile.mkdtemp(prefix='cd-',dir='/tmp'))
(root/'bin').mkdir(); (root/'.cosmon/formulas').mkdir(parents=True)
herdrroot=pathlib.Path(tempfile.mkdtemp(prefix='hd-',dir='/tmp'))
name='cosmon-dada-'+str(os.getpid()); sock=herdrroot/'herdr/sessions'/name/'herdr.sock'
(herdrroot/'herdr').mkdir(); (herdrroot/'herdr/config.toml').write_text('[update]\nversion_check=false\nmanifest_check=false\n')
env={k:v for k,v in os.environ.items() if not k.startswith(('HERDR_','COSMON_','CB_','ANTHROPIC_'))}
env.update(PATH=str(root/'bin')+':'+env['PATH'],COSMON_CONFIG_HOME=str(root/'config-home'),CODEX_HOME=str(root/'codex-home'),TMUX_TMPDIR=str(root),HOME=str(root))
henv=dict(env);henv.update(XDG_CONFIG_HOME=str(herdrroot),XDG_STATE_HOME=str(herdrroot/'state'))
log=open(D/'private-conformance.jsonl','w')
def emit(kind,data):log.write(json.dumps(dict(kind=kind,data=data))+'\n');log.flush()
def run(argv,where=root,extra=None,check=True):
 e=dict(env);e.update(extra or {});r=subprocess.run(argv,cwd=where,env=e,capture_output=True,text=True,timeout=90);emit('command',dict(argv=argv,code=r.returncode,out=r.stdout,err=r.stderr));
 if check and r.returncode:raise RuntimeError(str(argv)+': '+r.stderr)
 return r
# Preserve the operator's configured git identity; read it, do not modify it.
for key in ['user.name','user.email']:
 value=subprocess.check_output(['git','config','--get',key],text=True).strip()
 # HOME isolation would hide existing global git settings. Point at the existing global file instead.
env['GIT_CONFIG_GLOBAL']=str(pathlib.Path(os.environ['HOME'])/'.gitconfig')
config=root/'.cosmon/config.toml'
def configure(host):config.write_text('[project]\nproject_id="private-herdr-dada"\ntrunk_branch="main"\n[transport]\nbackend="'+host+'"\n[adapters.codex]\nmode="exec"\nupdate="skip"\nworker_exit_receipts=true\n[worker]\non_complete="commit"\n')
configure('herdr')
(root/'.gitignore').write_text('.cosmon/state/\n.worktrees/\nbin/\nconfig-home/\ncodex-home/\ntmux-*/\n')
(root/'.cosmon/formulas/witness.formula.toml').write_text('formula="witness"\nversion=1\nid_prefix="witness"\ndescription="Private model-free qualification"\n[[steps]]\nid="write"\ntitle="Write"\ndescription="Write a stub artifact"\nacceptance="Stub result committed"\n')
stub=root/'bin/codex';stub.write_text('''#!/bin/sh
if [ "$1" = --version ]; then echo 'codex 0.0.0'; exit 0; fi
printf 'OpenAI Codex\\nready\\n'
echo ready > "$COSMON_MOL_DIR/stub-ready"
echo started >> "$COSMON_MOL_DIR/stub-starts"
while [ ! -f "$COSMON_MOL_DIR/stub-go" ]; do sleep 0.1; done
printf 'stub result\\n' > "stub-$COSMON_PARENT_MOL_ID.txt"
cs evolve "$COSMON_PARENT_MOL_ID" --evidence 'stub wrote artifact' --formula .cosmon/formulas/witness.formula.toml
cs complete "$COSMON_PARENT_MOL_ID" --reason 'model-free worker result'
exit 0
''');stub.chmod(0o700)
# cs must be available to the stub without changing the installed CLI.
(root/'bin/cs').symlink_to(CS)
server=None
workers=[]
def start():
 global server
 server=subprocess.Popen(['herdr','--session',name,'server'],env=henv,stdout=open(D/'herdr-private.log','a'),stderr=subprocess.STDOUT)
 for _ in range(200):
  if sock.exists():
   try: req('session.snapshot');return
   except (OSError,ValueError):pass
  if server.poll() is not None:raise RuntimeError('private Herdr exited')
  time.sleep(.05)
 raise RuntimeError('private Herdr did not become ready')
def stop():
 r=subprocess.run(['herdr','--session',name,'server','stop'],env=henv,capture_output=True,text=True,timeout=20);emit('private-stop',dict(code=r.returncode,out=r.stdout,err=r.stderr));server.wait(timeout=20)
def req(method,params={}):
 with socket.socket(socket.AF_UNIX) as s:
  s.settimeout(8);s.connect(str(sock));s.sendall((json.dumps(dict(id='private',method=method,params=params))+'\n').encode());r=json.loads(s.makefile().readline())
 emit('herdr:'+method,dict(params=params,response=r));return r
try:
 run(['git','init','-q','-b','main']);run(['git','add','.']);run(['git','commit','-qm','test: initialize private Herdr qualification'])
 start()
 for host,next_host in [('herdr','tmux'),('tmux','herdr')]:
  configure(host)
  run(['git','add','.cosmon/config.toml']);run(['git','commit','-qm','test: select '+host],check=False)
  mol=json.loads(run([CS,'--json','nucleate','witness']).stdout)['id'];workers.append(mol)
  extra={'HERDR_SOCKET_PATH':str(sock)}
  run([CS,'tackle',mol,'--adapter','codex','--model','gpt-5.6-sol'],extra=extra)
  mdir=root/'.cosmon/state/fleets/default/molecules'/mol
  # Resolve the canonical directory actually produced by this galaxy.
  states=list((root/'.cosmon/state').rglob(mol+'/state.json'))
  assert len(states)==1,states
  mdir=states[0].parent
  assert (mdir/'stub-ready').exists(), 'stub marker absent: refuse model-free claim'
  state=json.loads(states[0].read_text());emit('pin:'+host,state.get('process'))
  configure(next_host)
  run(['git','add','.cosmon/config.toml']);run(['git','commit','-qm','test: switch config while '+host+' worker runs'])
  (mdir/'stub-go').touch()
  run([CS,'wait',mol,'--timeout','30'],extra=extra)
  run([CS,'done',mol],extra=extra)
  assert not (root/'.worktrees'/mol).exists()
 configure('herdr')
 def launch(label):
  mol=json.loads(run([CS,'--json','nucleate','witness']).stdout)['id'];workers.append(mol)
  run([CS,'tackle',mol,'--adapter','codex','--name',label,'--model','gpt-5.6-sol'],extra={'HERDR_SOCKET_PATH':str(sock)})
  states=list((root/'.cosmon/state').rglob(mol+'/state.json'));assert len(states)==1
  mdir=states[0].parent;assert (mdir/'stub-ready').exists()
  state=json.loads(states[0].read_text());return mol,mdir,state['process']
 def receipt(mdir):
  paths=list(mdir.rglob('result.json'));return json.loads(paths[0].read_text()) if paths else None
 # A and B deliberately have the same worker name and workspace label.
 a,adir,ap=launch('shared-worker');b,bdir,bp=launch('shared-worker')
 before=req('pane.get',{'pane_id':bp['terminal']['pane_id']})['result']['pane']
 b_birth=json.loads(next(bdir.rglob('birth.json')).read_text());os.kill(b_birth['child_pid'],0)
 run([CS,'complete',a,'--reason','private stop conformance'])
 run([CS,'done',a])
 after=req('pane.get',{'pane_id':bp['terminal']['pane_id']})['result']['pane']
 assert before['terminal_id']==after['terminal_id'];assert receipt(bdir) is None
 fleet=json.loads((root/'.cosmon/state/fleet.json').read_text());assert fleet['workers']['shared-worker']['current_molecule']==b
 assert receipt(adir)['observation']=={'kind':'signaled','signal':15,'core_dumped':False}
 os.kill(b_birth['child_pid'],0)
 emit('same-name-a-b',dict(a=a,b=b,a_receipt=receipt(adir),b_receipt=receipt(bdir),b_terminal=after['terminal_id']))
 (bdir/'stub-go').touch();run([CS,'wait',b,'--timeout','30']);run([CS,'done',b])
 assert 'shared-worker' not in json.loads((root/'.cosmon/state/fleet.json').read_text())['workers']
 r,rdir,rp=launch('restart-worker');old=rp['terminal'];starts=(rdir/'stub-starts').read_text();stop();start()
 restored=req('pane.get',{'pane_id':old['pane_id']})['result']['pane']
 assert restored['terminal_id']!=old['terminal_id']
 # A same-name tmux terminal must not be stopped by Herdr purge.
 run(['tmux','-L','private-herdr-dada','-f','/dev/null','new-session','-d','-s','restart-worker','sleep 120'])
 run([CS,'purge','restart-worker','--force'])
 run(['tmux','-L','private-herdr-dada','has-session','-t','=restart-worker'])
 untouched=req('pane.get',{'pane_id':old['pane_id']})['result']['pane']
 assert restored['terminal_id']==untouched['terminal_id'];assert receipt(rdir) is None
 time.sleep(.5);assert (rdir/'stub-starts').read_text()==starts
 assert (rdir/'worker-launch.json').exists()
 emit('restart-refusal',dict(molecule=r,old=old,restored_terminal=untouched['terminal_id'],receipt='Unknown',unresolved=True))
 run(['tmux','-L','private-herdr-dada','kill-session','-t','=restart-worker'])
 emit('result',dict(root=str(root),private_name=name,socket=str(sock),workers=workers))
finally:
 if server and server.poll() is None:stop()
 r=subprocess.run(['herdr','--session',name,'session','delete',name,'--json'],env=henv,capture_output=True,text=True,timeout=20);emit('private-delete',dict(code=r.returncode,out=r.stdout,err=r.stderr,exists=sock.parent.exists()))
 shutil.rmtree(herdrroot)
 (D/'conformance-galaxy-path.txt').write_text(str(root)+'\n')
 log.close()
