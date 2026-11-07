"""Mechanical schema erratum before any live judgment; preserve v2 evidence."""
import json
from pathlib import Path
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[2]
OLD=HERE.parent/'live-v2'
assert not (HERE/'cases.json').exists()
for name in ['cases.json','LABELS.md','PROTOCOL.md','generate.py']:
    (HERE/name).write_bytes((OLD/name).read_bytes())
for old in sorted((ROOT/'fixtures/schemas/discovery_live_v2').rglob('*.yaml')):
    relative=old.relative_to(ROOT/'fixtures/schemas/discovery_live_v2')
    new=ROOT/'fixtures/schemas/discovery_live_v2_1'/relative
    new.parent.mkdir(parents=True,exist_ok=True)
    if str(relative)=='suppliers/matrix/domain.yaml':
        schema=json.loads(old.read_text());schema['entities']['Certificate']['primary_read']='certificate_primary'
        new.write_text(json.dumps(schema,indent=2)+'\n')
    else:new.write_bytes(old.read_bytes())
runner=(OLD/'run.py').read_text().replace('discovery-live-native-v2-proof-20261029','discovery-live-native-v2_1-20261029').replace('schemas/discovery_live_v2','schemas/discovery_live_v2_1')
runner=runner.replace("'LABELS.md','generate.py'","'LABELS.md','generate.py','repair.py','ERRATUM.md'")
(HERE/'run.py').write_text(runner)
