"""Preserve the invalid attempt and repair only the abstract proof setup."""
from pathlib import Path
import hashlib
import json

ROOT = Path(__file__).resolve().parents[3]
OLD = ROOT / 'eval-reports/catalog-breadth-v1-20261030'
NEW = ROOT / 'eval-reports/catalog-breadth-v1-20261030-r2'
HERE = ROOT / 'fixtures/discovery/catalog-breadth-v1-r2'
lock = json.loads((OLD / 'LOCK.json').read_text())
files = {'connected': ROOT / 'crates/plasm-agent-core/src/discovery_matcher/online/connected.rs',
         'online': ROOT / 'crates/plasm-agent-core/src/discovery_matcher/online.rs'}
for key, path in files.items():
    assert hashlib.sha256(path.read_bytes()).hexdigest() == lock['baseline'][key]
NEW.mkdir(); HERE.mkdir()
for name in ['run.py', 'report.py', 'PROTOCOL.md']:
    text = (Path(__file__).parent / name).read_text()
    if name == 'run.py':
        text = text.replace("eval-reports/catalog-breadth-v1-20261030'", "eval-reports/catalog-breadth-v1-20261030-r2'")
    if name == 'PROTOCOL.md':
        text += '\nPreflight repair r2: the previous attempt ran no matrix cases. Add three explicitly matching abstract reads to the single-catalogue proof so it exercises unvisited, once-visited and repeatedly visited roots. Scheduling, inherited fixtures, labels, budgets and acceptance rules are unchanged.\n'
    (HERE / name).write_text(text)
for key, path in files.items():
    (NEW / f'{key}.baseline.rs').write_bytes(path.read_bytes())
    text = (OLD / f'{key}.candidate.rs').read_text()
    if key == 'connected':
        before = '''        let catalogs = super::super::tests::fixture();
        let (_, lexical) = orders(&catalogs, &BTreeMap::new());'''
        after = '''        let mut catalogs = super::super::tests::fixture();
        let cgs = catalogs.get_mut("matrix").unwrap();
        for name in ["calibration_alpha", "calibration_beta", "calibration_gamma"] {
            let mut read = cgs.capabilities["source_read"].clone();
            read.name = name.into();
            read.description = "Inspect calibration; preserve originals.".into();
            cgs.capabilities.insert(read.name.clone(), read);
        }
        let (_, lexical) = orders(&catalogs, &BTreeMap::new());'''
        assert text.count(before) == 1
        text = text.replace(before, after)
        assert text.split('#[cfg(test)]\nmod catalog_breadth_proofs')[0] == (OLD / f'{key}.candidate.rs').read_text().split('#[cfg(test)]\nmod catalog_breadth_proofs')[0]
    path.write_text(text)
print('Prepared r2; production scheduling candidate unchanged, only proof fixture setup repaired.')
