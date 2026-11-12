"""Remove orphan entity definitions in the split federation fixture."""
import json
from pathlib import Path
HERE=Path(__file__).resolve().parent;ROOT=HERE.parents[2];OLD=HERE.parent/'live-v2.1'
assert not (HERE/'cases.json').exists()
for name in ['cases.json','LABELS.md','PROTOCOL.md','generate.py']:(HERE/name).write_bytes((OLD/name).read_bytes())
for old in sorted((ROOT/'fixtures/schemas/discovery_live_v2_1').rglob('*.yaml')):
 relative=old.relative_to(ROOT/'fixtures/schemas/discovery_live_v2_1');new=ROOT/'fixtures/schemas/discovery_live_v2_2'/relative;new.parent.mkdir(parents=True,exist_ok=True)
 if relative.parts[0]=='federation' and relative.name=='domain.yaml':
  schema=json.loads(old.read_text());owned={c['entity'] for c in schema['capabilities'].values()};schema['entities']={k:v for k,v in schema['entities'].items() if k in owned};new.write_text(json.dumps(schema,indent=2)+'\n')
 else:new.write_bytes(old.read_bytes())
runner=(OLD/'run.py').read_text().replace('discovery-live-native-v2_1-20261029','discovery-live-native-v2_2-20261029').replace('schemas/discovery_live_v2_1','schemas/discovery_live_v2_2');(HERE/'run.py').write_text(runner)
