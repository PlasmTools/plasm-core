"""Authoring provenance only. Refuses to replace frozen fixtures."""
import json
from pathlib import Path
HERE=Path(__file__).resolve().parent
SCHEMAS=HERE.parent.parent/'schemas/discovery_live_v2'
assert not (HERE/'cases.json').exists()
def put(p,v):
    p.parent.mkdir(parents=True,exist_ok=True)
    p.write_text(json.dumps(v,indent=2)+'\n')
def schema():
    return dict(version=1,entry_id='matrix',http_backend='http://127.0.0.1:9',auth={'scheme':'none'},values={},entities={},capabilities={})
def entity(s,name,meaning):
    s['values'][name.lower()]={'type':'string','description':meaning}
    s['entities'][name]={'id_field':'value','description':meaning,'fields':{'value':{'value_ref':name.lower(),'required':True}}}
def cap(s,name,out,meaning,inputs=(),effect=None):
    c=dict(entity=out,kind='action' if inputs or effect else 'get',description=meaning)
    if not inputs and not effect:c['receiver']={'kind':'none'}
    if inputs:c['arguments']={'input_type':{'type':'object','fields':[{'name':'input'+str(i),'value_ref':v.lower(),'required':True} for i,v in enumerate(inputs)]}}
    if effect:c['output']={'type':'side_effect','description':effect}
    else:c.update(provides=['value'],output={'type':'entity','entity_type':out})
    s['capabilities'][name]=c
def negative(s):
    entity(s,'Invoice','Accounting invoice number identifying a billed service.')
    cap(s,'invoice_read','Invoice','Read accounting invoice numbers for billed services.')
def phase(request,labels,paths):return dict(request=request,labels=labels,paths=paths)
def save_case(name,catalogs,phases,binding=None):
    for key,s in catalogs.items():
        s['entry_id']=key
        put(SCHEMAS/name/key/'domain.yaml',s)
        put(SCHEMAS/name/key/'mappings.yaml',{k:{'method':'GET' if c['kind']=='get' else 'POST','path':[{'type':'literal','value':k}]} for k,c in s['capabilities'].items()})
    return dict(id=name,catalogs=list(catalogs),phases=phases,binding=binding)
cases=[]
s=schema();entity(s,'Level','Measured liquid depth in the reservoir, in millimetres.');cap(s,'depth_read','Level','Read the current measured reservoir liquid depth without changing the reservoir.');negative(s)
cases.append(save_case('observation',{'matrix':s},[phase('Read the reservoir liquid depth. Preserve the reservoir state.',{'matrix/depth_read':'relevant','matrix/invoice_read':'unrelated'},[['matrix/depth_read']])]))
s=schema()
for n,d in [('Blank','Blank plate material for preparing an embossing pattern.'),('Pattern','Embossing pattern consumed by the embossing operation.'),('Plate','Finished embossed plate.')]:entity(s,n,d)
cap(s,'blank_read','Blank','Read blank plate material for embossing.');cap(s,'pattern_prepare','Pattern','Prepare an embossing pattern from blank plate material; preserve the original material.',['Blank']);cap(s,'plate_emboss','Plate','Emboss a finished plate using an embossing pattern.',['Pattern']);negative(s)
cases.append(save_case('chain',{'matrix':s},[phase('Emboss a finished plate from blank material while preserving the original material.',{'matrix/blank_read':'relevant','matrix/pattern_prepare':'relevant','matrix/plate_emboss':'relevant','matrix/invoice_read':'unrelated'},[['matrix/blank_read','matrix/pattern_prepare','matrix/plate_emboss']])]))
s=schema()
for n,d in [('Certificate','A calibration certificate accepted for validating a sensor.'),('Validation','Sensor validation result.'),('Paint','Paint shade label; not a calibration certificate.')]:entity(s,n,d)
cap(s,'certificate_primary','Certificate','Read a sensor calibration certificate from the primary register.');cap(s,'certificate_secondary','Certificate','Read an equally valid sensor calibration certificate from the secondary register.');cap(s,'sensor_validate','Validation','Validate the sensor using a calibration certificate.',['Certificate']);cap(s,'paint_read','Paint','Read paint shade labels.')
cases.append(save_case('suppliers',{'matrix':s},[phase('Validate the sensor using a calibration certificate from either register.',{'matrix/certificate_primary':'relevant','matrix/certificate_secondary':'relevant','matrix/sensor_validate':'relevant','matrix/paint_read':'unrelated'},[['matrix/certificate_primary','matrix/sensor_validate'],['matrix/certificate_secondary','matrix/sensor_validate']])]))
# Cross-catalogue acquisition is declared, never inferred from common strings.
s=schema();entity(s,'Sample','Reference sample used to calibrate a spectrometer.');entity(s,'Calibration','Completed spectrometer calibration.');cap(s,'sample_read','Sample','Read a reference sample for spectrometer calibration.');cap(s,'calibrate','Calibration','Calibrate the spectrometer using a reference sample.',['Sample'])
contract={'version':1,'inputs':{},'outputs':{'sample':'sample'}}
import copy
source=copy.deepcopy(s);source['capabilities'].pop('calibrate');source['prerequisites']={'contracts':{'reference_feed':contract},'providers':{'reference':{'contract':'reference_feed','capability':'sample_read','outputs':{'sample':'value'}}}}
sink=copy.deepcopy(s);sink['capabilities'].pop('sample_read');negative(sink);sink['prerequisites']={'contracts':{'reference_feed':contract},'requirements':{'calibrate':[{'id':'reference','contract':'reference_feed','bindings':[{'input':{'lane':'arguments','path':['input0']},'output':'sample'}]}]}}
cases.append(save_case('federation',{'supply':source,'lab':sink},[phase('Calibrate the spectrometer with a reference sample.',{'supply/sample_read':'relevant','lab/calibrate':'relevant','lab/invoice_read':'unrelated'},[['supply/sample_read','lab/calibrate']])],dict(consumer='lab/calibrate',requirement='reference',provider_catalog='supply',provider='reference')))
s=schema()
for n,d in [('Humidity','Current greenhouse relative humidity.'),('Sound','Current machinery sound pressure.')]:entity(s,n,d)
cap(s,'humidity_read','Humidity','Read greenhouse relative humidity.');cap(s,'sound_read','Sound','Read machinery sound pressure.');negative(s)
cases.append(save_case('independent',{'matrix':s},[phase('Read greenhouse humidity and machinery sound pressure without changing either environment.',{'matrix/humidity_read':'relevant','matrix/sound_read':'relevant','matrix/invoice_read':'unrelated'},[['matrix/humidity_read'],['matrix/sound_read']])]))
s=schema()
for n,d in [('Rows','Tabular survey observations.'),('File','Durable survey export file.'),('Preview','Transient preview of survey observations.')]:entity(s,n,d)
cap(s,'survey_read','Rows','Read tabular survey observations.');cap(s,'survey_export','File','Write a durable export file from survey observations.',['Rows']);cap(s,'survey_preview','Preview','Render a transient preview from survey observations without writing an export file.',['Rows']);negative(s)
cases.append(save_case('conditional',{'matrix':s},[phase('Export the survey observations to a durable file. If durable export is unavailable, produce a transient preview instead. Do not change the observations.',{'matrix/survey_read':'relevant','matrix/survey_export':'relevant','matrix/survey_preview':'relevant','matrix/invoice_read':'unrelated'},[['matrix/survey_read','matrix/survey_export'],['matrix/survey_read','matrix/survey_preview']])]))
s=schema();entity(s,'Condition','Current pump vibration measurement.');cap(s,'vibration_read','Condition','Read current pump vibration without changing the pump.');cap(s,'routine_apply','Condition','Apply a configured routine whose domain effect is not specified.',effect='Performs an unspecified configured change.');negative(s)
cases.append(save_case('opaque',{'matrix':s},[phase('Read current pump vibration and discover whether the configured routine could contribute. Its readiness and effect are unknown; do not execute changes.',{'matrix/vibration_read':'relevant','matrix/routine_apply':'uncertain','matrix/invoice_read':'unrelated'},[['matrix/vibration_read']])]))
s=schema();entity(s,'Gear','Current gearbox rotational speed.');entity(s,'Ambient','Current room air temperature.');cap(s,'gear_read','Gear','Read gearbox rotational speed.');cap(s,'ambient_read','Ambient','Read room air temperature.');negative(s)
cases.append(save_case('revision',{'matrix':s},[phase('Read gearbox rotational speed.',{'matrix/gear_read':'relevant','matrix/ambient_read':'unrelated','matrix/invoice_read':'unrelated'},[['matrix/gear_read']]),phase('The gearbox request is complete. Now read room air temperature; do not change any settings.',{'matrix/gear_read':'unrelated','matrix/ambient_read':'relevant','matrix/invoice_read':'unrelated'},[['matrix/ambient_read']])]))
put(HERE/'cases.json',cases)
