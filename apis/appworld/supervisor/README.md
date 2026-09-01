# AppWorld supervisor — task principal helpers (no auth).

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/supervisor
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/supervisor/openapi.json apis/appworld/supervisor
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
