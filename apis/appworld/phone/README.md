# AppWorld phone

Simulated phone (contacts, alarms, SMS/voice). Auth: `login` → `access_token` → Bearer.

Contacts ground who a person is to the account holder, including friends,
coworkers, relatives and roommates. `contact_query` can search by name or
filter by a relationship label; returned contacts retain their relationship
labels for classification. Relationship discovery vocabulary belongs to this
read capability, not to cross-catalog task instructions.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/phone
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
