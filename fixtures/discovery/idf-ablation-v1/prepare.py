import json
from pathlib import Path
HERE=Path(__file__).resolve().parent
profiles=[
 {'id':'demagnetize','effect':'Demagnetize the component.','distractor':'Inspect component transport packaging identifier condition and dispatch readiness.'},
 {'id':'desiccate','effect':'Desiccate the specimen.','distractor':'Inspect specimen storage container identifier condition and shipment readiness.'},
 {'id':'notarize','effect':'Notarize the document.','distractor':'Inspect document delivery envelope identifier condition and submission readiness.'},
 {'id':'inspect','effect':'Inspect the component.','distractor':'Record component transport packaging identifier condition and dispatch readiness.'}]
requests={
 'demagnetize':['Demagnetize the component before transport in its packaging; preserve the identifier and dispatch readiness.','The component still needs demagnetization before transport. Preserve its identifier and packaging.'],
 'desiccate':['Desiccate the specimen before storage in its container; preserve the identifier and shipment readiness.','The specimen still needs desiccation before storage. Preserve its identifier and container.'],
 'notarize':['Notarize the document before delivery in its envelope; preserve the identifier and submission readiness.','The document still needs notarization before delivery. Preserve its identifier and envelope.'],
 'inspect':['Inspect the component before transport in its packaging; preserve the identifier and dispatch readiness.','The component still needs inspection before transport. Preserve its identifier and packaging.']}
cases={'version':1,'horizon':24,'gate':4,'growth':[0,100],'cases':[{'id':p['id'],'requests':requests[p['id']],'labels':{'routine':'relevant'},'outcomes':[['routine']],'relation':False} for p in profiles]}
for name,value in [('profiles.json',profiles),('cases.json',cases)]:
 with (HERE/name).open('x') as f:json.dump(value,f,indent=2)
print('16 fresh controls materialized before measurement.')
