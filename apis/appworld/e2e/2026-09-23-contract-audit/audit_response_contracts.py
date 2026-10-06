"""Independent response-contract audit; does not generate CGS or modify schemas."""
import yaml,json,re
from pathlib import Path
ROOT=Path('/Users/ryan/code/plasm/plasm-oss/apis/appworld')
def alternatives(s):
 return sum([alternatives(x) for x in s.get('anyOf',s.get('oneOf',[]))],[]) if 'anyOf' in s or 'oneOf' in s else [s]
def at(s,path):
 if not path:return alternatives(s)
 out=[]
 for x in alternatives(s):
  k=path[0]
  if k=='*':
   if 'items' in x:out+=at(x['items'],path[1:])
  elif k in x.get('properties',{}):out+=at(x['properties'][k],path[1:])
 return out
def pat(x):
 if x.get('type') in ['literal','const']:return re.escape(str(x.get('value','')))
 if x.get('type')=='if':return '(?:'+pat(x['then_expr'])+'|'+pat(x['else_expr'])+')'
 if x.get('type')=='format':return re.sub(r'\\\{[^}]+\\\}', '[^/]+',re.escape(x['template']))
 return '[^/]+'
def response_schema(s,m):
 r=m.get('response',{})
 if isinstance(r,dict):
  prep=r.get('response_preprocess',{})
  if prep.get('kind')=='object_projection':
   s={'type':'object','properties':{k:(at(s,v) or [{}])[0] for k,v in prep['fields'].items()}}
  elif prep.get('kind')=='concat_arrays':
   children=[]
   for source in prep['sources']:
    p=source['path']+['*']+([source['from_each'],'*'] if 'from_each' in source else [])
    children+=at(s,p)
   return children
  path=r.get('items_path') or ([r['items']] if isinstance(r.get('items'),str) else [])
  if path:
   return sum([at(t,['*']) for t in at(s,path)],[])
 return at(s,['*']) if s.get('type')=='array' else [s]
result=[]
for p in sorted(ROOT.glob('*/domain.yaml')):
 d=yaml.safe_load(p.read_text());maps=yaml.safe_load((p.parent/'mappings.yaml').read_text());spec=json.loads(Path('/private/tmp/appworld-data-0.2.0/data/api_docs/openapi',p.parent.name+'.json').read_text()); contexts={n:[] for n in d['entities']};viewfields={}
 for v in d.get('views',{}).values():viewfields.setdefault(v['entity'],set()).update(v.get('output',{}))
 for cap,m in maps.items():
  if cap not in d['capabilities'] or 'method' not in m or not isinstance(m.get('path'),list):continue
  c=d['capabilities'][cap];entity=c.get('output',{}).get('entity_type',c['entity']);pattern='/'+ '/'.join(pat(x) for x in m['path'])
  for path,ops in spec['paths'].items():
   if not re.fullmatch(pattern,path):continue
   op=ops.get(m['method'].lower(),{});s=op.get('responses',{}).get('200',{}).get('content',{}).get('application/json',{}).get('schema')
   if s and entity in contexts:contexts[entity]+=response_schema(s,m)
 # Embedded objects contribute their actual response schemas, not their names.
 for _ in range(len(contexts)):
  changed=False
  for e,ent in d['entities'].items():
   for rel in ent.get('relations',{}).values():
    m=rel.get('materialize',{})
    if m.get('kind') not in ['from_parent_get','prefer_from_parent_get']:continue
    path=[x.get('key','*') for x in m['path']];target=rel['target']
    for s in list(contexts[e]):
     for child in at(s,path):
      if child not in contexts[target]:contexts[target].append(child);changed=True
  if not changed:break
 for e,ent in d['entities'].items():
  if not contexts[e]:continue
  misses=[]
  for f,fs in ent.get('fields',{}).items():
   if f in viewfields.get(e,set()):continue
   if f==ent['id_field'] and ent.get('implicit_request_identity'):continue
   if f in ent.get('key_vars',[]) and f!=ent['id_field']:continue
   path=fs.get('path',[f]);found=any(at(s,path) for s in contexts[e])
   if not found:misses.append(f)
  if misses:result.append({'app':p.parent.name,'entity':e,'fields_without_response_source':misses})
print(json.dumps(result,indent=2))
