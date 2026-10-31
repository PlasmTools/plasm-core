"""Frozen paired endpoints; no provider access or label changes."""
import collections
import html
import json
import math
from run import HERE, OUT, sha, verify


def stats(xs):
    xs = sorted(xs)
    return None if not xs else {'n': len(xs), 'sum': sum(xs), **{
        name: xs[max(0, math.ceil(q * len(xs)) - 1)]
        for name, q in [('median', .5), ('p90', .9), ('p95', .95), ('max', 1)]}}


def main():
    verify(json.loads((HERE / 'LOCK.json').read_text()))
    verify(json.loads((OUT / 'SOURCES.json').read_text()))
    assert sha(OUT / 'PACKETS.json') == json.loads((OUT / 'PACKET-LOCK.json').read_text())['sha256']
    packets = json.loads((OUT / 'PACKETS.json').read_text())
    labels = json.loads((HERE / 'labels.json').read_text())
    records = [json.loads(l) for l in (OUT / 'TRACE.jsonl').read_text().splitlines()]
    completions = [r for r in records if r['kind'] == 'logical_complete']
    done = {r['id']: r for r in completions}
    starts = [r for r in records if r['kind'] == 'logical_start']
    faults = [r for r in records if r['kind'] == 'fault']
    assert len(done) == len(completions)
    assert len({r['id'] for r in starts}) == len(starts)
    pmap = {p['id']: p for p in packets}
    for r in starts:
        assert r['sha256'] == pmap[r['id']]['sha256'] and r['bytes'] == pmap[r['id']]['bytes'] <= 16000
    rows, families, confusion = [], {}, collections.Counter()
    for p in packets:
        label = labels[p['pair']]
        row = {k: p[k] for k in ['id', 'pair', 'family', 'arm', 'bytes', 'sha256']}
        row.update(label)
        if p['id'] in done:
            response = json.loads(done[p['id']]['raw'])
            answer = response['answers'][p['primary_question']]
            row.update(answer=answer, disposition='completed', matched=answer['choice'] in label['admissible'],
                       retained=answer['choice'] != 'unrelated', elapsed_ms=done[p['id']]['elapsed_ms'])
            f = families.setdefault(p['family'], {}).setdefault(p['arm'], {'completed': 0, 'matched': 0, 'relevant': 0, 'retained': 0})
            f['completed'] += 1
            f['matched'] += row['matched']
            f['relevant'] += answer['choice'] == 'relevant'
            f['retained'] += row['retained']
            if len(label['admissible']) == 1:
                confusion[(p['arm'], label['admissible'][0], answer['choice'])] += 1
        else:
            row['disposition'] = 'fault' if any(f['id'] == p['id'] for f in faults) else ('interrupted' if any(s['id'] == p['id'] for s in starts) else 'unstarted')
        rows.append(row)
    comparisons = []
    for pair in labels:
        arms = {r['arm']: r for r in rows if r['pair'] == pair}
        b, c = arms['baseline'], arms['inline']
        comp = {'id': pair, 'family': b['family'], 'complete': b['disposition'] == c['disposition'] == 'completed',
                'identical_bodies': b['sha256'] == c['sha256']}
        if comp['complete']:
            comp.update(baseline=b['answer']['choice'], inline=c['answer']['choice'],
                        strict_recovery=not b['matched'] and c['matched'], strict_regression=b['matched'] and not c['matched'],
                        lost_retention=b['retained'] and not c['retained'])
        comparisons.append(comp)
    support = [p for p in comparisons if p['complete'] and p['family'] == 'support']
    recoveries = [p['id'] for p in support if p['baseline'] != 'relevant' and p['inline'] == 'relevant']
    regressions = [p['id'] for p in support if p['baseline'] == 'relevant' and p['inline'] != 'relevant']
    unsupported = [p['id'] for p in comparisons if p['complete'] and p['family'] not in ['support', 'direct'] and p['baseline'] != 'relevant' and p['inline'] == 'relevant']
    useful_loss = [r['id'] for r in rows if r['disposition'] == 'completed' and r['family'] in ['support', 'direct'] and r['arm'] == 'inline' and not r['retained']]
    direct_ok = all(r.get('answer', {}).get('choice') == 'relevant' for r in rows if r['family'] == 'direct')
    signal = len(done) == 48 and len(recoveries) >= 2 and not regressions and not unsupported and not useful_loss and direct_ok
    attempts = {}
    for r in records:
        if r['kind'] == 'transport':
            t = r['record']
            a = attempts.setdefault(t['attempt_id'], {'case': r['id'], 'attempt': t['attempt'], 'events': []})
            assert a['case'] == r['id'] and a['attempt'] == t['attempt']
            a['events'].append(t)
    counts = collections.Counter(a['case'] for a in attempts.values())
    assert all(n <= 3 and n * pmap[k]['bytes'] <= 48000 for k, n in counts.items())
    usage, response_status, unpaired, elapsed = [], collections.Counter(), [], []
    for aid, a in attempts.items():
        terminal = [e for e in a['events'] if e['kind'] in ['response', 'transport']]
        assert len(terminal) <= 1
        if not terminal:
            unpaired.append(aid)
        elapsed.append(max(e['elapsed_ms'] for e in a['events'] if e['kind'] == 'lifecycle'))
        for e in terminal:
            if e['kind'] == 'response':
                response_status[str(e['http_status'])] += 1
                try:
                    usage.append({'case': a['case'], 'usage': json.loads(e['raw_response']).get('usage')})
                except json.JSONDecodeError:
                    usage.append({'case': a['case'], 'usage': None})
    numeric = sorted({k for u in usage if isinstance(u['usage'], dict) for k, v in u['usage'].items() if isinstance(v, (int, float)) and not isinstance(v, bool)})
    costs = {}
    for arm in ['baseline', 'inline']:
        us = [u['usage'] for u in usage if pmap[u['case']]['arm'] == arm]
        costs[arm] = {k: {'observed_sum': sum(u[k] for u in us if isinstance(u, dict) and isinstance(u.get(k), (int, float))),
                         'reported': sum(isinstance(u, dict) and isinstance(u.get(k), (int, float)) for u in us), 'responses': len(us)} for k in numeric}
    audit = {'completed': len(done), 'frozen': 48, 'adoption_signal': signal,
             'support_recoveries': recoveries, 'support_regressions': regressions,
             'new_unsupported_relevant': unsupported, 'useful_retention_loss': useful_loss,
             'direct_controls_relevant': direct_ok, 'families': families, 'rows': rows, 'pairs': comparisons,
             'singleton_confusion': [{'arm': a, 'expected': e, 'actual': c, 'count': n} for (a, e, c), n in sorted(confusion.items())],
             'attempts': attempts, 'transport_attempts': len(attempts), 'logical_requests': len(starts),
             'faults': faults, 'unpaired': unpaired, 'response_statuses': dict(response_status),
             'recovered_retries': [k for k, n in counts.items() if n > 1 and k in done],
             'usage_once_per_response': costs, 'raw_usage': usage,
             'latency_ms': {arm: stats([r['elapsed_ms'] for r in rows if r['arm'] == arm and r['disposition'] == 'completed']) for arm in ['baseline', 'inline']},
             'attempt_latency_ms': stats(elapsed), 'attempts_over_8s': sum(x > 8000 for x in elapsed),
             'body_bytes_per_arm': {arm: sum(p['bytes'] for p in packets if p['arm'] == arm) for arm in ['baseline', 'inline']},
             'attempt_body_bytes_upper_bound': sum(pmap[k]['bytes'] * n for k, n in counts.items()),
             'transmitted_bytes': None,
             'limits': '48 logical requests; 16000 bytes/body; three identical transient attempts/30s/8s attempt; no effects',
             'scope': 'Fixed-target diagnostic A/B; single samples, small repeated structures; no retrieval, allocation, AppWorld or generalization score.'}
    (OUT / 'AUDIT.json').write_text(json.dumps(audit, indent=2) + '\n')
    lines = ['# Verified connection rendering: paired diagnostic', '',
             f'Completed {len(done)}/48 requests. Pre-registered adoption signal: {signal}.',
             f'Support recoveries: {len(recoveries)}; regressions: {len(regressions)}. New unsupported relevance: {len(unsupported)}. Useful retention loss: {len(useful_loss)}.', '',
             '| Family | Baseline matched | Inline matched | Baseline relevant | Inline relevant |', '|---|---:|---:|---:|---:|']
    for family, arms in families.items():
        b, c = arms.get('baseline', {}), arms.get('inline', {})
        lines.append(f"| {family} | {b.get('matched',0)}/{b.get('completed',0)} | {c.get('matched',0)}/{c.get('completed',0)} | {b.get('relevant',0)} | {c.get('relevant',0)} |")
    lines += ['', audit['scope'], '',
              'Relevant and uncertain both retain. Admissible-set matches are separate from strict singleton labels. Missing-witness global utility is deliberately not treated as local positive evidence.',
              'Transport attempts: ' + str(len(attempts)) + '; faults: ' + str(len(faults)) + '; recovered retries: ' + str(len(audit['recovered_retries'])),
              'Whole judgment latency ms: ' + json.dumps(audit['latency_ms']),
              'Provider usage, counted once per response: ' + json.dumps(costs),
              'Cache and other unreported fields unavailable; dispatch is not proof of transmission. No confidence threshold or post-result tuning.', '',
              '## All paired outcomes', '', '| Case | Baseline | Inline | Identical bodies |', '|---|---|---|---|']
    lines += [f"| {p['id']} | {p.get('baseline','unavailable')} | {p.get('inline','unavailable')} | {p['identical_bodies']} |" for p in comparisons]
    text = (HERE / 'FINDINGS.md').read_text() + '\n\n' + '\n'.join(lines) + '\n'
    (OUT / 'REPORT.md').write_text(text)
    (OUT / 'REPORT.html').write_text('<!doctype html><meta charset="utf-8"><title>Connection rendering diagnostic</title><style>body{background:#101820;color:#e7eef2;max-width:1200px;margin:40px auto;font:16px system-ui}pre{white-space:pre-wrap;line-height:1.6}</style><pre>' + html.escape(text) + '</pre>')
    hashes = {p.name: sha(p) for p in OUT.iterdir() if p.is_file() and p.name != 'HASHES.json'}
    (OUT / 'HASHES.json').write_text(json.dumps(hashes, indent=2) + '\n')
    assert all(sha(OUT / n) == h for n, h in hashes.items())
    print(json.dumps({k: audit[k] for k in ['completed', 'adoption_signal', 'support_recoveries', 'support_regressions', 'new_unsupported_relevant', 'useful_retention_loss', 'families']}))


if __name__ == '__main__':
    main()
