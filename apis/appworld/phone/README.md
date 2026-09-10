# AppWorld phone

Simulated phone (contacts, alarms, SMS/voice). Auth: `login` → `access_token` → Bearer.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/phone
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
