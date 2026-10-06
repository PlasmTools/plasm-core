import os,json,time,subprocess,requests,warnings
from pathlib import Path
warnings.filterwarnings('ignore')
os.environ['APPWORLD_ROOT']='/world'
Path('/world').mkdir(exist_ok=True);Path('/world/data').symlink_to('/dataset')
import freezegun
freezegun.configure(extend_ignore_list=['subprocess','threading','selectors','urllib','http','requests','urllib3'])
from appworld import AppWorld
log=open('/proof/api.log','w')
p=subprocess.Popen(['appworld','serve','apis','--port','9000','--root','/world'],stdout=log,stderr=log)
for _ in range(240):
 try:
  if requests.get('http://localhost:9000/',timeout=1).ok:break
 except requests.RequestException:pass
 time.sleep(.25)
try:
 with AppWorld(task_id='18670a5_1',experiment_name='catalog_contract_live_probe',load_ground_truth=False,remote_apis_url='http://127.0.0.1:9000') as world:
  def get(path,token=None,params=None):
   r=requests.get('http://127.0.0.1:9000'+path,headers={'Authorization':'Bearer '+token} if token else {},params=params,timeout=30);r.raise_for_status();return r.json()
  profile=get('/supervisor/profile');passwords=get('/supervisor/account_passwords')
  tokens={}
  for row in passwords:
   app=row['account_name']
   r=requests.post('http://127.0.0.1:9000/'+app+'/auth/token',data={'username':profile['phone_number'] if app=='phone' else profile['email'],'password':row['password']},timeout=30)
   if r.ok:tokens[app]=r.json()['access_token']
  manifest={'base_url':'http://127.0.0.1:9000','tokens':tokens,'cases':[]}
  def case(app,entity,id,program=None,fields=None):manifest['cases'].append({'app':app,'entity':entity,'id':str(id),'program':program or entity+'('+json.dumps(str(id))+')','fields':fields or []})
  # Real read targets, never fabricated identifiers.
  orders=get('/amazon/orders',tokens['amazon']);
  if orders:
   o=orders[0];oid=o['order_id'];case('amazon','Order',oid,fields=['order_id']);case('amazon','Order',oid,'Order('+json.dumps(str(oid))+').items',['order_id','product_id','price'])
   product=o['order_items'][0]['product_id'];case('amazon','Product',product,fields=['num_product_reviews','num_product_questions','shareable_link']);case('amazon','RatingDistribution',product,'RatingDistribution('+json.dumps(str(product))+').breakdown',['product_id','rating','count','percentage']);case('amazon','LastPurchase',product,fields=['requested_product_id','product_id','order_id'])
  for cat in ['inbox','outbox']:
   rows=get('/gmail/email_threads/category/'+cat,tokens['gmail'])
   if rows:
    tid=rows[0]['email_thread_id'];case('gmail','EmailThread',tid,'EmailThread('+json.dumps(str(tid))+').emails',['sender_email','sender_name']);thread=get('/gmail/email_threads/'+str(tid),tokens['gmail']);
    if thread['emails']:
     eid=thread['emails'][0]['email_id'];case('gmail','Email',eid,'Email('+json.dumps(str(eid))+').recipients',['email','name'])
    break
  groups=get('/splitwise/groups',tokens['splitwise'])
  for group in groups:
   gid=group['group_id'];ex=get('/splitwise/group/'+str(gid)+'/expenses',tokens['splitwise'])
   if ex:
    eid=ex[0]['expense_id'];case('splitwise','Expense',eid,'Expense('+json.dumps(str(eid))+').shares',['expense_id','debtor_email','debt_amount']);case('splitwise','Group',gid,fields=['creator_email']);case('splitwise','Group',gid,'Group('+json.dumps(str(gid))+').balances.participants',['group_id','participant_email']);case('splitwise','Group',gid,'Group('+json.dumps(str(gid))+').balances.participants.outgoing',['group_id','participant_email','counterparty_email','amount']);break
  projs=get('/todoist/projects',tokens['todoist'])
  if projs:
   pid=projs[0]['project_id'];case('todoist','Project',pid,fields=['creator_email','num_tasks','is_archived']);tasks=get('/todoist/projects/'+str(pid)+'/tasks',tokens['todoist']);ts=tasks['no_section_tasks']+[t for s in tasks['sections'] for t in s.get('tasks',[])]
   if ts:case('todoist','Task',ts[0]['task_id'],fields=['creator_email','order_index','num_sub_tasks'])
  for app,path,entity,key,fields in [('phone','/phone/alarms','Alarm','alarm_id',['user_phone_number']),('simple_note','/simple_note/notes','Note','note_id',['content']),('venmo','/venmo/transactions','Transaction','transaction_id',['liked']),('spotify','/spotify/playlists','Playlist','playlist_id',['owner_email','review_count'])]:
   rows=get(path,tokens[app]);
   if rows:case(app,entity,rows[0][key],fields=fields)
  case('spotify','Library','probe','Library{access_token='+json.dumps(tokens['spotify'])+'}.songs',['song_id'])
  case('supervisor','Supervisor',profile['email'],fields=['email'])
  # File read uses a real account path; enumerate via the documented file query.
  files=get('/file_system/directory',tokens['file_system'],{'directory_path':'/','entry_type':'files','recursive':True})
  Path('/proof/files-shape.json').write_text(json.dumps(files))
  if files:
   row=files[0];path=row if isinstance(row,str) else row.get('file_path',row.get('path'));case('file_system','File',path,fields=['path'])
  Path('/proof/private-manifest.json').write_text(json.dumps(manifest));os.chmod('/proof/private-manifest.json',0o600)
  Path('/proof/ready').write_text('ready')
  while not Path('/proof/stop').exists():time.sleep(1)
finally:
 p.terminate();p.wait(timeout=20)
