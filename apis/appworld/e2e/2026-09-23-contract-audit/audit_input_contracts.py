import yaml,json,re
from pathlib import Path
root=Path('/Users/ryan/code/plasm/plasm-oss/apis/appworld')
out=[]
for p in sorted(root.glob('*/domain.yaml')):
 app=p.parent.name;d=yaml.safe_load(p.read_text()); maps=yaml.safe_load((p.parent/'mappings.yaml').read_text());spec=json.loads(Path('/private/tmp/appworld-data-0.2.0/data/api_docs/openapi',app+'.json').read_text())
 for cap,m in maps.items():
  if cap not in d['capabilities'] or not isinstance(m,dict) or 'method' not in m:continue
  seg=m.get('path',[])
  if not isinstance(seg,list):continue
  def pat(x):
   if x.get('type') in ['literal','const']:return re.escape(str(x.get('value','')))
   if x.get('type')=='if':return '(?:'+pat(x['then_expr'])+'|'+pat(x['else_expr'])+')'
   if x.get('type')=='format':return re.sub(r'\\\{[^}]+\\\}', '[^/]+',re.escape(x['template']))
   return '[^/]+'
  pattern='/'+ '/'.join(pat(x) for x in seg)
  for path,ops in spec['paths'].items():
   op=ops.get(m['method'].lower());
   if not op or not re.fullmatch(pattern,path):continue
   expected={x['name'] for x in op.get('parameters',[]) if x.get('in')=='query'}
   for c in op.get('requestBody',{}).get('content',{}).values():expected.update(c.get('schema',{}).get('properties',{}))
   actual=set()
   for lane in ['query','body']:
    node=m.get(lane,{})
    if isinstance(node,dict) and node.get('type')=='object':actual.update(x[0] for x in node.get('fields',[]))
   actual.update(m.get('pagination',{}).get('params',{}))
   missing=expected-actual
   if missing:out.append({'app':app,'cap':cap,'path':path,'method':m['method'],'missing':sorted(missing),'descriptions':{x['name']:x.get('description','') for x in op.get('parameters',[]) if x['name'] in missing}})
Path('/private/tmp/appworld-input-gaps.json').write_text(json.dumps(out,indent=2))
for x in out:print(x['app'],x['cap'],x['path'],','.join(x['missing']))
