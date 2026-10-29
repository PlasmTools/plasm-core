"""Author the fixed development fixture matrix once, before native scoring."""
import json
from pathlib import Path

here = Path(__file__).resolve().parent
if (here / 'cases.json').exists():
    raise SystemExit('Refusing to overwrite authored fixtures')
cases = []
for domain, noun in [('optics','spectrum'), ('acoustics','waveform'),
                     ('microscopy','specimen'), ('fabrication','stencil'),
                     ('telemetry','signal'), ('geometry','contour')]:
    base = {'source_read': f'Read original {noun} material.',
            'prepare': f'Prepare {noun} material without deleting originals.',
            'process': f'Produce the requested {noun}.',
            'opaque': 'Apply a configured routine whose result is unspecified.'}
    for family in ['conditions','context','purpose','request_roles','composition',
                   'uncertainty_permission','allocation','revision']:
        descriptions = dict(base)
        turns = [f'Produce the requested {noun}; preserve originals.']
        labels = {'source_read':'relevant','prepare':'relevant','process':'relevant','opaque':'uncertain'}
        expected = [['source_read','prepare','process']]
        denied = []
        exposed = []
        oversized = None
        if family == 'conditions':
            descriptions['opaque'] = f'Archive the {noun}. ' + f'Inspect status before producing {noun}; already produced material is rejected. ' * 8
            labels['opaque'] = 'unrelated'
        elif family == 'context':
            descriptions['opaque'] = f'Archive historical {noun} records.'
            labels['opaque'] = 'unrelated'
        elif family == 'purpose':
            descriptions['opaque'] = f'Applies to configured facilities. Polarize the {noun}. Records the outcome.'
            turns = [f'Polarize the {noun}.']
            labels = {key: 'unrelated' for key in base}
            labels['opaque'] = 'relevant'
            expected = [['opaque']]
        elif family == 'request_roles':
            turns = [f'Produce the requested {noun} from original material. Preserve originals; if production is unavailable, read the original material.']
            # Both primary and conditional branch must remain discoverable.
            expected = [['source_read','prepare','process'], ['source_read']]
        elif family == 'uncertainty_permission':
            denied = ['prepare']
            expected = [['source_read'], ['opaque']]
            turns = [f'Read original {noun} and inspect the configured routine. Its readiness is unknown.']
            labels['process'] = 'unrelated'
        elif family == 'allocation':
            oversized = 'opaque'
            descriptions['opaque'] = f'Inspect {noun} telemetry. ' + 'Authored complete evidence. ' * 1000
            turns = [f'Inspect {noun} telemetry and read original {noun} material.']
            expected = [['source_read'], ['opaque']]
            labels['prepare'] = labels['process'] = 'unrelated'
        elif family == 'revision':
            turns = [f'Produce the requested {noun}. Preserve originals.',
                     f'Instead read original {noun} material; do not produce it.']
            exposed = ['process']
            expected = [['source_read']]
            labels['prepare'] = labels['process'] = 'unrelated'
        cases.append({'id':domain+'/'+family,'family':family,'descriptions':descriptions,
                      'context': (f'Inspect complete {noun} already available. '*8 if family=='context' else ''),
                      'turns':turns,'labels':labels,'expected_paths':expected,
                      'denied':denied,'exposed':exposed,'oversized':oversized,
                      'rationale':'Path labels follow source -> prepared -> result value domains; opaque is unknown unless a different explicit effect/purpose is supplied. Permission is independent of contribution.'})
(here/'cases.json').write_text(json.dumps(cases,indent=2)+'\n')
print(f'Authored {len(cases)} cases; no scoring performed.')
