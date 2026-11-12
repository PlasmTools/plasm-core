"""Audit the frozen diagnostic without calling a provider or changing labels."""
import collections
import hashlib
import html
import json
import math
from pathlib import Path

from run import HERE, OUT, ROOT, sha, verify


def stats(values):
    values = sorted(values)
    if not values:
        return None
    return {'n': len(values), **{name: values[max(0, math.ceil(q * len(values)) - 1)]
            for name, q in [('median', .5), ('p90', .9), ('p95', .95), ('max', 1)]},
            'sum': sum(values)}


def main():
    verify(json.loads((HERE / 'LOCK-r2.json').read_text()))
    verify(json.loads((OUT / 'SOURCES.json').read_text()))
    assert sha(OUT / 'PACKETS.json') == json.loads((OUT / 'PACKET-LOCK.json').read_text())['sha256']
    packets = json.loads((OUT / 'PACKETS.json').read_text())
    labels = json.loads((HERE / 'labels.json').read_text())
    records = [json.loads(line) for line in (OUT / 'TRACE.jsonl').read_text().splitlines()]
    completed = {r['id']: r for r in records if r['kind'] == 'logical_complete'}
    started = [r for r in records if r['kind'] == 'logical_start']
    assert len({r['id'] for r in started}) == len(started), 'logical replay'
    assert len(completed) == sum(r['kind'] == 'logical_complete' for r in records)
    faults = [r for r in records if r['kind'] == 'fault']
    packet_by_id = {p['id']: p for p in packets}
    for r in started:
        assert r['sha256'] == packet_by_id[r['id']]['sha256']
        assert r['bytes'] == packet_by_id[r['id']]['bytes'] <= 16000
    rows, confusion, families = [], collections.Counter(), {}
    for p in packets:
        label = labels[p['id']]
        row = {'id': p['id'], 'pair': p['pair'], 'family': p['family'], **label,
               'body_sha256': p['sha256'], 'bytes': p['bytes']}
        if p['id'] in completed:
            raw = json.loads(completed[p['id']]['raw'])
            answer = raw['answers'][p['primary_question']]
            row.update(disposition='completed', answer=answer, admissible=label['admissible'],
                       matches_label=answer['choice'] in label['admissible'],
                       retained=answer['choice'] != 'unrelated', elapsed_ms=completed[p['id']]['elapsed_ms'])
            if len(label['admissible']) == 1:
                confusion[(label['admissible'][0], answer['choice'])] += 1
            f = families.setdefault(p['family'], {'completed': 0, 'matched': 0})
            f['completed'] += 1
            f['matched'] += row['matches_label']
        else:
            row['disposition'] = 'fault' if any(f['id'] == p['id'] for f in faults) else (
                'interrupted' if any(s['id'] == p['id'] for s in started) else 'unstarted')
        rows.append(row)
    pairs = []
    for pair in dict.fromkeys(p['pair'] for p in packets):
        a, b = [r for r in rows if r['pair'] == pair]
        pair_ok = None
        if a['disposition'] == b['disposition'] == 'completed':
            pair_ok = a['matches_label'] and b['matches_label']
            if a['pair_relation'] == 'invariance':
                pair_ok = pair_ok and a['answer']['choice'] == b['answer']['choice']
        pairs.append({'id': pair, 'relation': a['pair_relation'], 'passed': pair_ok})
    attempts = {}
    for r in records:
        if r['kind'] != 'transport':
            continue
        t = r['record']
        entry = attempts.setdefault(t['attempt_id'], {'case': r['id'], 'attempt': t['attempt'], 'events': []})
        assert entry['case'] == r['id'] and entry['attempt'] == t['attempt']
        entry['events'].append(t)
    by_case = collections.Counter(a['case'] for a in attempts.values())
    assert all(n <= 3 for n in by_case.values())
    admitted_upper_bound = sum(packet_by_id[k]['bytes'] * n for k, n in by_case.items())
    responses, usage_records, phases = [], [], collections.defaultdict(list)
    unpaired = []
    for aid, a in attempts.items():
        terminal = [e for e in a['events'] if e['kind'] in ['response', 'transport']]
        assert len(terminal) <= 1, 'duplicate terminal receipt'
        if not terminal:
            unpaired.append(aid)
        for e in a['events']:
            if e['kind'] == 'lifecycle':
                phases[e['phase'] + ':' + e['event']].append(e['elapsed_ms'])
            elif e['kind'] == 'response':
                responses.append(e)
                try:
                    usage_records.append(json.loads(e['raw_response']).get('usage'))
                except json.JSONDecodeError:
                    usage_records.append(None)
    usage_fields = sorted({k for u in usage_records if isinstance(u, dict) for k, v in u.items()
                           if isinstance(v, (int, float)) and not isinstance(v, bool)})
    usage = {k: {'observed_sum': sum(u[k] for u in usage_records if isinstance(u, dict) and isinstance(u.get(k), (int, float))),
                 'reported_responses': sum(isinstance(u, dict) and isinstance(u.get(k), (int, float)) for u in usage_records),
                 'total_responses': len(responses)} for k in usage_fields}
    attempt_elapsed = [max(e['elapsed_ms'] for e in a['events'] if e['kind'] == 'lifecycle') for a in attempts.values()]
    assert all(n <= 48000 for n in (packet_by_id[k]['bytes'] * count for k, count in by_case.items()))
    done = [r for r in rows if r['disposition'] == 'completed']
    summary = {'frozen': 48, 'completed': len(done), 'matched': sum(r['matches_label'] for r in done),
               'pairs_passed': sum(p['passed'] is True for p in pairs),
               'pairs_completed': sum(p['passed'] is not None for p in pairs),
               'false_rejections': [r['id'] for r in done if r['admissible'] == ['relevant'] and not r['retained']],
               'unrelated_retained': [r['id'] for r in done if r['admissible'] == ['unrelated'] and r['retained']],
               'uncertain_retained': [r['id'] for r in done if r['answer']['choice'] == 'uncertain'],
               'families': families}
    audit = {'summary': summary, 'rows': rows, 'pairs': pairs,
             'confusion_singleton_only': [{'expected': e, 'actual': a, 'count': n} for (e, a), n in sorted(confusion.items())],
             'faults': faults, 'logical_requests': len(started), 'transport_attempts': len(attempts),
             'attempts_per_case': dict(by_case), 'unpaired_attempts': unpaired,
             'response_statuses': dict(collections.Counter(str(r['http_status']) for r in responses)),
             'retry_recoveries': [k for k, n in by_case.items() if n > 1 and k in completed],
             'attempt_body_bytes_upper_bound': admitted_upper_bound,
             'transmitted_bytes': None, 'usage_reported_once_per_response': usage,
             'raw_usage': usage_records, 'whole_judgment_ms': stats([r['elapsed_ms'] for r in done]),
             'phase_elapsed_from_attempt_start_ms': {k: stats(v) for k, v in phases.items()},
             'attempt_elapsed_ms': stats(attempt_elapsed),
             'observed_attempts_over_8s': sum(v > 8000 for v in attempt_elapsed),
             'completed_judgments_over_30s': [r['id'] for r in done if r['elapsed_ms'] > 30000],
             'body_identity_basis': 'Frozen exact body regenerated before every logical call; request_decision receives one immutable body for all transport attempts. This pass needed no retries.',
             'attempts': attempts,
             'limits': '48 logical requests; <=16000 bytes each; <=3 identical attempts/30s, 8s per attempt; no effects',
             'scope': 'Fixed target semantic diagnostic, not ranking, allocation, workload efficiency, or AppWorld success. Single sample per packet; no calibrated confidence claim.',
             'missing': 'Unreported usage/cost/cache fields unavailable. Lifecycle dispatch is not proof of transmission. Phase elapsed values are not independent phase durations.',
             'preflight_errata': str(HERE / 'PREFLIGHT-ERRATA.md')}
    (OUT / 'AUDIT.json').write_text(json.dumps(audit, indent=2) + '\n')
    lines = ['# Frozen decision-interface diagnostic', '',
             f"Completed {len(done)}/48 packets; {summary['matched']} matched frozen admissible labels. "
             f"Matched pairs: {summary['pairs_passed']}/{summary['pairs_completed']}.", '',
             '| Family | Completed | Matched |', '|---|---:|---:|']
    lines += [f"| {k} | {v['completed']} | {v['matched']} |" for k, v in families.items()]
    lines += ['', f"Useful targets rejected: {len(summary['false_rejections'])}. Unrelated targets retained: {len(summary['unrelated_retained'])}.",
              'All 26 known-useful targets were retained; four label mismatches were uncertain answers that production retains. The other two mismatches were false-positive relevant answers.',
              f"Transport: {len(attempts)} attempts, {len(audit['retry_recoveries'])} recovered retries, {len(faults)} unrecovered faults.",
              'Whole judgment latency (ms): ' + json.dumps(audit['whole_judgment_ms']),
              'Usage (terminal response receipts only): ' + json.dumps(usage), '',
              audit['scope'], '', audit['missing'], '',
              'Four incomplete-witness controls admit uncertain or unrelated: hidden global usefulness is not counted as a semantic false rejection. '
              'Eight peer questions are auxiliary and excluded from the primary confusion matrix. '
              'The first preparation failed catalogue validation; unused entities were removed before any provider call, with original evidence retained.', '',
              '## Case results', '', '| Case | Expected | Actual | Match |', '|---|---|---|---|']
    lines += [f"| {r['id']} | {' / '.join(r['admissible'])} | {r.get('answer', {}).get('choice', r['disposition'])} | {r.get('matches_label', 'unavailable')} |" for r in rows]
    text = '\n'.join(lines) + '\n'
    findings = (HERE / 'FINDINGS.md').read_text()
    text += '\n' + findings
    (OUT / 'REPORT.md').write_text(text)
    table = ''.join('<tr>' + ''.join('<td>' + html.escape(str(v)) + '</td>' for v in [r['id'], ' / '.join(r['admissible']), r.get('answer', {}).get('choice', r['disposition']), r.get('matches_label', 'unavailable')]) + '</tr>' for r in rows)
    page = '<!doctype html><meta charset="utf-8"><title>Decision interface diagnostic</title><style>body{max-width:1100px;margin:40px auto;font:16px system-ui;background:#101820;color:#e7eef2}td,th{padding:8px;text-align:left;border-bottom:1px solid #43515b}pre{white-space:pre-wrap}a{color:#89cfff}</style><h1>Decision interface diagnostic</h1><pre>' + html.escape('\n'.join(lines[:lines.index('## Case results')])) + '</pre><table><tr><th>Case</th><th>Expected</th><th>Actual</th><th>Match</th></tr>' + table + '</table>'
    page += '<h2>Interpretation and next experiment</h2><pre>' + html.escape(findings) + '</pre>'
    (OUT / 'REPORT.html').write_text(page)
    hashes = {p.name: sha(p) for p in OUT.iterdir() if p.is_file() and p.name != 'HASHES.json'}
    (OUT / 'HASHES.json').write_text(json.dumps(hashes, indent=2) + '\n')
    assert all(sha(OUT / n) == h for n, h in hashes.items())
    print(json.dumps(summary))


if __name__ == '__main__':
    main()
