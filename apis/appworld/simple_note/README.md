# AppWorld simple_note

Simulated notes app. Auth: `login` → `access_token` → Bearer on note ops.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/simple_note
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/simple_note/openapi.json apis/appworld/simple_note
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
