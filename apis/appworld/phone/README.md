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

Message reads have checked variants: `shelf=list` accepts ranked `query`,
`only_latest_per_contact`, and `sort_by`, but forbids datetime bounds and
`pagination_order`. `shelf=window` requires `phone_number`, accepts inclusive
`min_datetime` / `max_datetime` and `pagination_order`, and forbids list-only
inputs. CGS conditional cross-field rules reject incompatible combinations before
HTTP dispatch. Search ranks matches without excluding nonmatches; verify content
and timestamps before actions.
