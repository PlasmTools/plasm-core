"""Author abstract mechanism controls, never production catalogs."""
import json
from pathlib import Path
HERE=Path(__file__).resolve().parent
goal='Process the prepared material and retain the original source.'
needs=[goal,'Read the source needed for processing.','Prepare the source for processing.','Process the prepared material.']
cases=[]
for name,size,sequence in [('small',1,[goal]),('large_fit',45,[goal]),('large_path',90,[goal]),('oversized',260,[goal]),('paraphrased',45,needs),('long_history',45,[x+' Preserve the original record and its attribution. '*8 for x in needs])]:
    cases.append(dict(id=name,repeat=size,needs=sequence,labels={'source_read':'relevant','prepare':'relevant','process':'relevant','opaque':'uncertain'},expected=['source_read','prepare','process'],denied=[],reject=False,positive=name!='oversized'))
cases.append(dict(cases[0],id='independent',needs=[goal+' Also apply the configured routine if it contributes; its effect is unspecified.'],expected=['source_read','prepare','process','opaque']))
cases.append(dict(cases[0],id='denied',denied=['process'],positive=False))
cases.append(dict(cases[0],id='judgment_rejection',reject=True,positive=False))
document={'version':3,'searches':12,'distractors':20,'description_clause':'Process prepared material into the final result. Preserve the source, attribution and audit fields. Readiness is unknown until the prepared material is observed. ', 'cases':cases,'exposure_schedules':[['source_read','prepare'],['source_read'],['prepare']]}
(HERE/'cases.json').write_text(json.dumps(document,indent=2)+'\n')
