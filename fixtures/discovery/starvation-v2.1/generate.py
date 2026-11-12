"""Freeze abstract catalogues and case labels before native baseline scoring."""
import json
from pathlib import Path

HERE=Path(__file__).resolve().parent
SCHEMAS=HERE.parents[1]/'schemas/discovery_starvation_v2_1'
if (HERE/'cases.json').exists():
    raise SystemExit('Refusing to overwrite fixtures')

base='''version: 1
entry_id: matrix
http_backend: http://127.0.0.1:9
auth: {scheme: none}
values:
  source: {type: string, description: Original material used to prepare a spectrum.}
  prepared: {type: string, description: Prepared material consumed to produce a spectrum.}
  result: {type: string, description: Spectrum observation.}
entities:
  Source:
    id_field: value
    primary_read: source_a
    description: Original material.
    fields:
      value: {value_ref: source, required: true}
  Prepared:
    id_field: value
    description: Prepared material.
    fields:
      value: {value_ref: prepared, required: true}
  Result:
    id_field: value
    description: Observed spectrum.
    fields:
      value: {value_ref: result, required: true}
capabilities:
  source_a:
    entity: Source
    kind: get
    description: Read original material from the primary reservoir.
    provides: [value]
    output: {type: entity, entity_type: Source}
  source_b:
    entity: Source
    kind: get
    description: Read original material from the secondary reservoir.
    provides: [value]
    output: {type: entity, entity_type: Source}
  prepare:
    entity: Prepared
    kind: create
    description: Prepare original material for spectrum production without deleting originals.
    arguments:
      input_type:
        type: object
        fields:
          - {name: material, value_ref: source, required: true}
    output: {type: side_effect, description: Creates prepared material; read it to obtain its value.}
  prepared_read:
    entity: Prepared
    kind: query
    description: Read prepared material for spectrum production.
    provides: [value]
    output: {type: collection, entity_type: Prepared}
  process:
    entity: Result
    kind: action
    description: Produce the requested spectrum from prepared material.
    arguments:
      input_type:
        type: object
        fields:
          - {name: material, value_ref: prepared, required: true}
    provides: [value]
    output: {type: entity, entity_type: Result}
  inspect:
    entity: Result
    kind: action
    description: Inspect telemetry and report its current condition without changing it.
    provides: [value]
    output: {type: entity, entity_type: Result}
'''
noise='''version: 1
entry_id: noise
http_backend: http://127.0.0.1:9
auth: {scheme: none}
values:
  source: {type: string, description: Historical archive identifier.}
entities:
  Source:
    id_field: value
    description: Historical administrative archive.
    fields:
      value: {value_ref: source, required: true}
capabilities:
  archive:
    entity: Source
    kind: action
    description: Archive historical rainfall records.
    output: {type: side_effect, description: Archives an administrative record; does not prepare material or produce observations.}
'''
prerequisites='''prerequisites:
  contracts:
    material_feed:
      version: 1
      inputs: {}
      outputs: {material: source}
  providers:
    primary:
      contract: material_feed
      capability: source_a
      outputs: {material: value}
    secondary:
      contract: material_feed
      capability: source_b
      outputs: {material: value}
  requirements:
    prepare:
      - id: feed
        contract: material_feed
        bindings:
          - input: {lane: arguments, path: [material]}
            output: material
'''
for name,source,names in [('local',base,['source_a','source_b','prepare','prepared_read','process','inspect']),
                          ('cross',base+prerequisites,['source_a','source_b','prepare','prepared_read','process','inspect']),
                          ('noise',noise,['archive'])]:
    folder=SCHEMAS/name;folder.mkdir(parents=True,exist_ok=True)
    (folder/'domain.yaml').write_text(source)
    (folder/'mappings.yaml').write_text(''.join(f'{n}:\n  method: GET\n  path: [{{type: literal, value: {n}}}]\n' for n in names))

cases=[]
for topology in ['local_alternatives','cross_primary','cross_secondary']:
    for profile in ['neutral','near_topic']:
        for count in [0,100,1000,10000]:
            prefix='matrix' if topology=='local_alternatives' else 'engine'
            source='matrix' if topology=='local_alternatives' else 'materials'
            alternatives=[[f'{source}/{supplier}',f'{prefix}/prepare',f'{prefix}/prepared_read',f'{prefix}/process']
                          for supplier in (['source_a','source_b'] if topology=='local_alternatives'
                                           else ['source_a' if topology=='cross_primary' else 'source_b'])]
            useful=set(sum(alternatives,[]))|{f'{prefix}/inspect'}
            labels={f'{source}/{supplier}':'relevant' for supplier in ['source_a','source_b']}
            labels.update({key:'relevant' for key in useful})
            cases.append({'id':f'{topology}/{profile}/{count}','topology':topology,'profile':profile,'count':count,
                          'request':'Produce the requested spectrum and inspect telemetry. Preserve original material.',
                          'labels':labels,'outcomes':[{'id':'spectrum','alternatives':alternatives},
                                                     {'id':'telemetry','alternatives':[[f'{prefix}/inspect']]}],
                          'noise_label':'unrelated','provider':('secondary' if topology=='cross_secondary' else 'primary')})
(HERE/'cases.json').write_text(json.dumps(cases,indent=2)+'\n')
(HERE/'noise.json').write_text(json.dumps({
    'neutral':'Archive historical rainfall records.',
    'near_topic':'Archive historical telemetry records. Before producing the requested spectrum, inspect telemetry status; spectrum production requires inspection. This operation only archives past records.',
    'repeat':4,'identity_prefix':'distractor_','label':'unrelated'},indent=2)+'\n')
print('24 cases and schema/labels authored; no selection run.')
