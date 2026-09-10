# AppWorld todoist

Simulated Todoist. Auth: `login` → `access_token` → Bearer. Covers projects, tasks, labels, sections, comments, sub-tasks (skips signup/verify/password-reset).

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/todoist
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/todoist/openapi.json apis/appworld/todoist
```

Backend: `http://127.0.0.1:9000` after `appworld serve apis`.
