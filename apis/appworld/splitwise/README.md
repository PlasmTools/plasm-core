# AppWorld splitwise

Simulated Splitwise (groups, expenses, payments, settle-up). Auth: `login` → `access_token` → Bearer.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/splitwise
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
