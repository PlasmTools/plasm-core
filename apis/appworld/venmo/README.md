# AppWorld Venmo

Task-oriented catalog for AppWorld's simulated Venmo service. It covers login and
logout, users and friends, balance management, transactions, sent and received
payment requests, and linked payment cards.

Call `login` with the Venmo username and password, then pass its `access_token`
to authenticated capabilities. Mappings send it as a Bearer token.

Backend: `http://127.0.0.1:9000`

```bash
cargo run -q -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/venmo
```

The pinned source specification is `openapi.json`.
