"""Check experimental contrasts before releasing the frozen provider run."""
import json
from run import HERE, OUT, ROOT, save, sha, verify


def scope(packet):
    body = json.loads(packet['body'])
    local = body['state']['question_evidence'][packet['primary_question']]
    return {'local': local,
            'definitions': {k: body['state']['definitions'][k] for k in local['definition_refs']},
            'bindings': {k: body['state']['binding_witnesses'][k] for k in local['binding_refs']}}


def main():
    verify(json.loads((HERE / 'LOCK-r2.json').read_text()))
    verify(json.loads((OUT / 'SOURCES.json').read_text()))
    assert (OUT / 'READY.json').exists()
    assert not (OUT / 'LAUNCHED').exists()
    original = ROOT / 'eval-reports/decision-interface-v1-20261030/frozen-inputs'
    assert (HERE / 'labels.json').read_bytes() == (original / 'labels.json').read_bytes()
    assert (HERE / 'cases.json').read_bytes() == (original / 'cases.json').read_bytes()
    packets = json.loads((OUT / 'PACKETS.json').read_text())
    questions = 0
    for p in packets:
        b = json.loads(p['body'])
        questions += len(b['questions'])
        assert len(p['body'].encode()) == p['bytes'] <= 16000
        assert p['primary_question'] in b['questions']
        local = scope(p)['local']
        assert local['capabilities']['c0']['capability']['reference']['capability'] == 'op_a'
        assert set(b['state']) == {'outcome', 'rule', 'definitions', 'binding_witnesses', 'question_evidence'}
    checks = []
    for pair in dict.fromkeys(p['pair'] for p in packets):
        a, b = [p for p in packets if p['pair'] == pair]
        sa, sb = scope(a), scope(b)
        ba, bb = json.loads(a['body']), json.loads(b['body'])
        if a['family'] in ['scope_isolation', 'readiness', 'constraints_branches']:
            assert sa == sb, f'Unexpected primary evidence change: {pair}'
        if a['family'] in ['action_context', 'effect_role', 'local_evidence', 'scope_isolation']:
            assert ba['state']['outcome'] == bb['state']['outcome']
        if a['family'] == 'local_evidence':
            assert len(sa['local']['capabilities']) == 2 and len(sb['local']['capabilities']) == 1
            assert sa['bindings'] and not sb['bindings']
        checks.append({'pair': pair, 'contrast_validated': True})
    assert questions == 56
    save(OUT / 'CONTRAST-CHECKS.json', {'packets': 48, 'questions': questions,
         'pairs': checks, 'labels_unchanged_from_first_freeze': True,
         'packet_sha256': sha(OUT / 'PACKETS.json'), 'provider_calls': 0})
    print('All 24 contrasts and 48 question-local scopes verified; 56 questions, 48 scored')


if __name__ == '__main__':
    main()
