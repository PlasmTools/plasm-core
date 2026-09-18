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

Friend lists belong to an owner: omit `owner_email` to read the account holder's
own friends, or supply it to read that person's friends. To find a peer within
the account holder's list, use `query` and confirm the returned email, or use the
peer-identity Get. `owner_email` maps to AppWorld's query parameter `user_email`;
add/remove operations act on the peer rather than the list owner.
