import os,json
from pathlib import Path
os.environ["APPWORLD_ROOT"]="/world"
Path("/world").mkdir(exist_ok=True)
Path("/world/data").symlink_to("/dataset")
from appworld import AppWorld
with AppWorld(task_id="18670a5_1",experiment_name="catalog_contract_probe",load_ground_truth=False) as world:
    result=world.execute("""
profile=apis.supervisor.show_profile()
passwords=apis.supervisor.show_account_passwords()
password=next(row['password'] for row in passwords if row['account_name']=='gmail')
token=apis.gmail.login(username=profile['email'],password=password)['access_token']
draft=apis.gmail.create_draft(access_token=token,recipient_email_addresses=[profile['email']],subject='Contract probe',body='Transport verification',scheduled_send_at='2030-01-02|03:04:05')
print(draft)
""")
    print(result)
