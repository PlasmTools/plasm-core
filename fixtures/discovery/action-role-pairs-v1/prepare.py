import json
from pathlib import Path
HERE=Path(__file__).resolve().parent
specs=[('vessel','seal','inspect'),('document','encrypt','archive'),('component','demagnetize','weigh'),('specimen','desiccate','photograph')]
cases=[];profiles=[]
for noun,left,right in specs:
    profiles.append({'id':noun,'left_effect':f'{left.capitalize()} the {noun}.','right_effect':f'{right.capitalize()} the {noun}.',
      'distractor':f'Record historical instructions to {left} or {right} the {noun}, preserve its identifier and retain its location. This records text and does not change the {noun}.'})
    for side,positive,negative,target in [('left',left,right,'routine'),('right',right,left,'record')]:
        cases.append({'id':f'{noun}/{side}','requests':[f'{positive.capitalize()} the {noun}; do not {negative} the {noun}. Preserve its identifier and retain its location.'],
          'labels':{target:'relevant'},'outcomes':[[target]],'relation':False})
for name,value in [('cases.json',{'version':1,'horizon':24,'gate':4,'growth':[0,100],'cases':cases}),('profiles.json',profiles)]:
    with (HERE/name).open('x') as f:json.dump(value,f,indent=2)
print('Eight paired conditions / sixteen variants frozen as fixture files before measurement.')
