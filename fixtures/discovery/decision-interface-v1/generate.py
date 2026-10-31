"""Author abstract diagnostic fixtures. No task catalogues or model calls."""
import json
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[2]


def save(path, value):
    path.parent.mkdir(parents=True, exist_ok=True)
    with path.open('x') as f:
        json.dump(value, f, indent=2)
        f.write('\n')


def schema(description, output=None, consumer=None, peer=None, input_needed=False):
    values = {'item_id': {'type': 'string', 'description': 'Identity of a controlled item.'},
              'ticket_id': {'type': 'string', 'description': 'Opaque routing token identity.'}}
    entities = {name: {'id_field': 'id', 'fields': {'id': {'value_ref': value, 'required': True}}}
                for name, value in [('Item', 'item_id'), ('Token', 'ticket_id')]}
    target = {'entity': 'Item', 'kind': 'action', 'description': description,
              'output': {'type': 'side_effect', 'description': output or description}}
    if input_needed:
        target['arguments'] = {'input_type': {'type': 'object', 'fields': [
            {'name': 'token', 'value_ref': 'ticket_id', 'required': True}]}}
    caps = {'op_a': target}
    if consumer:
        caps['op_a'] = {'entity': 'Token', 'kind': 'get', 'description': description,
                        'provides': ['id'], 'output': {'type': 'entity', 'entity_type': 'Token'}}
        caps['op_b'] = {'entity': 'Item', 'kind': 'action', 'description': consumer,
                        'arguments': {'input_type': {'type': 'object', 'fields': [
                            {'name': 'token', 'value_ref': 'ticket_id', 'required': True}]}},
                        'output': {'type': 'side_effect', 'description': consumer}}
    if peer:
        caps['op_b'] = {'entity': 'Item', 'kind': 'action', 'description': peer,
                        'output': {'type': 'side_effect', 'description': peer}}
    if not consumer:
        del entities['Token']
    return {'version': 1, 'entry_id': 'diagnostic', 'http_backend': 'http://127.0.0.1:9',
            'auth': {'scheme': 'none'}, 'values': values, 'entities': entities, 'capabilities': caps}


def main():
    cases, labels = [], {}

    def pair(family, number, request, descriptions, expected, rationale,
             requests=None, consumer=None, witnesses=None, peers=None, input_needed=False,
             workflow=None, relation='sensitivity'):
        pair_id = f'{family}-{number}'
        for arm in range(2):
            case_id = f'{pair_id}-{arm}'
            cgs = schema(descriptions[arm], consumer=consumer,
                         peer=peers[arm] if peers else None, input_needed=input_needed)
            directory = ROOT / 'fixtures/schemas/discovery_decision_v1' / case_id
            save(directory / 'domain.yaml', cgs)  # JSON is a YAML subset.
            save(directory / 'mappings.yaml', {name: {'method': 'GET' if c['kind'] == 'get' else 'POST',
                 'path': [{'type': 'literal', 'value': name}]} for name, c in cgs['capabilities'].items()})
            cases.append({'id': case_id, 'pair': pair_id, 'family': family,
                          'request': requests[arm] if requests else request,
                          'targets': ['op_a', 'op_b'] if peers else ['op_a'],
                          'witness': bool(witnesses and witnesses[arm])})
            labels[case_id] = {'target': 'op_a', 'admissible': expected[arm],
                              'workflow_useful': workflow[arm] if workflow else expected[arm] == ['relevant'],
                              'rationale': rationale[arm], 'pair_relation': relation}

    objects = ['ceramic tile', 'optical plate', 'woven panel', 'resin block']
    for i, obj in enumerate(objects):
        pair('action_context', i, f'Seal the {obj} with a clear finish. The surface check is already complete; no inspection records are needed.',
             [f'Apply a clear protective seal to the {obj}.',
              f'Read the archived surface inspection of the {obj}. Does not apply or change any seal.'],
             [['relevant'], ['unrelated']],
             ['The operation performs the requested sealing.',
              'Historical inspection serves an explicitly unneeded observation, not sealing.'])

    roles = [
        ('Open the iris gate.', 'Open the iris gate.', 'Close the iris gate.'),
        ('Publish the prepared notice.', 'Publish the prepared notice for recipients to read.', 'Store a historical record of an earlier publication; does not publish.'),
        ('Activate the prepared beacon.', 'Activate the prepared beacon.', 'Deactivate the beacon.'),
        ('Dispatch the prepared capsule.', 'Dispatch the prepared capsule to its destination.', 'Record receipt of a capsule already delivered; does not dispatch.'),
    ]
    for i, (request, yes, no) in enumerate(roles):
        pair('effect_role', i, request, [yes, no], [['relevant'], ['unrelated']],
             ['Performs the requested effect.', 'Performs the reverse effect or records a different stage; it does not cause the requested effect.'])

    actions = ['calibrate the prism', 'stabilize the spindle', 'align the aperture', 'charge the capsule']
    for i, action in enumerate(actions):
        purpose = f'Use a routing token to {action}. Does not consume or alter other items.'
        pair('readiness', i, '', [purpose, purpose], [['relevant'], ['relevant']],
             ['Known input makes execution readiness clear, but contribution comes from the effect.',
              'The same effect remains useful before its input has been obtained.'],
             requests=[f'{action.capitalize()}. The routing token is already available.',
                       f'{action.capitalize()}. The routing token is not yet available; it can be obtained before executing.'],
             input_needed=True, relation='invariance')

    for i, action in enumerate(['tune the resonator', 'anneal the wafer', 'balance the rotor', 'sterilize the chamber']):
        pair('local_evidence', i, f'{action.capitalize()}.',
             ['Obtain an opaque routing token. Its downstream use is unspecified.']*2,
             [['relevant'], ['uncertain', 'unrelated']],
             ['An authorized typed witness shows this token feeds an operation with the requested effect.',
              'The local packet does not identify any helpful downstream use. Global utility is deliberately hidden, so a positive answer is not established locally.'],
             consumer=f'Use the supplied routing token to {action}.', witnesses=[True, False],
             workflow=[True, True], relation='evidence_resolution')

    restrictions = [
        ('Recoat the alloy marker, replacing its existing surface.', 'Recoat the alloy marker; replacing its surface is permitted.', 'Only inspect the alloy marker. Preserve its surface unchanged.'),
        ('Publish the aperture reading on a public notice board.', 'Publish the aperture reading publicly.', 'Keep the aperture reading private. Do not publish it.'),
        ('Polish the ceramic disk.', 'If the disk is scratched, polish it; otherwise leave it unchanged. The condition has not yet been checked.', 'Never polish the ceramic disk. Inspect it without changing it.'),
        ('Notify the operator by sending a message.', 'If the threshold is exceeded, notify the operator; otherwise remain silent. The threshold result is not yet known.', 'Produce a private local draft only. Do not notify the operator or send a message.'),
    ]
    for i, (purpose, yes, no) in enumerate(restrictions):
        pair('constraints_branches', i, '', [purpose]*2, [['relevant'], ['unrelated']],
             ['The effect supports an explicitly requested outcome or conditional branch; exposure does not execute it.',
              'The documented effect contradicts an explicit restriction on the current request.'], requests=[yes, no])

    isolated = [
        ('Seal the membrane.', 'Read an archive entry; this never seals membranes.', ['unrelated']),
        ('Illuminate the dial.', 'Illuminate the dial.', ['relevant']),
        ('Balance the pendulum.', 'Apply an unspecified procedure; its output and effects are undocumented.', ['uncertain']),
        ('Open the shutter.', 'Close the shutter.', ['unrelated']),
    ]
    for i, (request, purpose, expected) in enumerate(isolated):
        pair('scope_isolation', i, request, [purpose]*2, [expected, expected],
             ['Only this target and its local references govern its contribution.']*2,
             peers=[request, 'Record historical humidity readings without affecting any other resource.'], relation='invariance',
             workflow=[None if expected == ['uncertain'] else expected == ['relevant']]*2)
    assert len(cases) == 48 and len(labels) == 48
    save(HERE / 'cases.json', cases)
    save(HERE / 'labels.json', labels)
    print('Authored 24 pairs / 48 cases; no model calls')


if __name__ == '__main__':
    main()
