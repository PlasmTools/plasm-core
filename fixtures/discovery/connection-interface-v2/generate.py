"""Fresh catalogue controls; no provider access or outcome-dependent generation."""
import importlib.util
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]
spec = importlib.util.spec_from_file_location('v1', HERE.parent / 'decision-interface-v1/generate.py')
v1 = importlib.util.module_from_spec(spec)
spec.loader.exec_module(v1)


def save(p, value):
    p.parent.mkdir(parents=True, exist_ok=True)
    with p.open('x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')


def main():
    cases, labels = [], {}
    effects = ['Archive the prepared audio segment.', 'Vent the pressure vessel.',
               'Register the geometric mesh.', 'Irrigate the seed tray.']
    for family in ['support', 'different_outcome', 'prohibited', 'opaque', 'absent_witness', 'direct']:
        for n, goal in enumerate(effects):
            case_id = f'connection-v2-{family}-{n}'
            request = goal
            consumer = goal
            purpose = 'Obtain a routing token identified by its id.'
            witness = True
            expected = ['relevant']
            if family == 'different_outcome':
                consumer = ['Measure the cabinet humidity.', 'Read the control-panel clock.',
                            'List the archived calibration dates.', 'Read the lamp serial number.'][n]
                expected = ['uncertain', 'unrelated']
            elif family == 'prohibited':
                request = ['Keep the audio segment local; do not archive it.',
                           'Keep the pressure vessel sealed; do not vent it.',
                           'Keep the mesh unregistered; do not register it.',
                           'Keep the seed tray dry; do not irrigate it.'][n]
                expected = ['uncertain', 'unrelated']
            elif family == 'opaque':
                consumer = 'Apply an unspecified procedure. Its effects are undocumented.'
                expected = ['uncertain']
            elif family == 'absent_witness':
                witness = False
                expected = ['uncertain', 'unrelated']
            elif family == 'direct':
                purpose, consumer, witness = goal, None, False
            cgs = v1.schema(purpose, consumer=consumer)
            directory = ROOT / 'fixtures/schemas/discovery_decision_v1' / case_id
            save(directory / 'domain.yaml', cgs)
            save(directory / 'mappings.yaml', {name: {'method': 'GET' if c['kind'] == 'get' else 'POST',
                 'path': [{'type': 'literal', 'value': name}]} for name, c in cgs['capabilities'].items()})
            cases.append({'id': case_id, 'pair': case_id, 'family': family, 'request': request,
                          'targets': ['op_a'], 'witness': witness})
            labels[case_id] = {'admissible': expected, 'workflow_useful': family in ['support', 'absent_witness', 'direct'] if family != 'opaque' else None}
    save(HERE / 'cases.json', cases)
    save(HERE / 'labels.json', labels)
    print('24 fresh paired controls authored; no model calls')


if __name__ == '__main__':
    main()
