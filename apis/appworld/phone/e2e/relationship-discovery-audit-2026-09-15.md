# Contact relationship discovery audit

## Contract evidence

The OpenAPI `GET /phone/contacts` operation accepts an optional `relationship`
query parameter: "Relationship with the person in the contacts list to filter
by." Its response exposes contact relationship labels. The vendor implementation
filters membership in the account holder's stored contact relationships, with
case normalization and singularization of the requested label.

CGS already declares a relationship enum, a list of those labels on Contact,
and an optional relationship selection parameter on `contact_query`. CML already
forwards that filter to the documented parameter. No transport change is needed.

## Annotation change

Phone version 12 gives Contact its account-holder relationship role and makes
`contact_query` describe identification by recorded relationship. Read discovery
terms now include friends, coworkers, relatives, family members, roommates,
partners, managers and subordinates. These are domain vocabulary, not an
instruction to execute any particular cross-catalog workflow. Family terms are
discovery vocabulary; the actual filter still requires a supported label.

Before the change, the generated document said only "Search contacts by
name/relationship" and exposed `relationship: Select` and `relationships: Array`.
It did not include the enum's label vocabulary. After the change, the document
contains the explicit evidence role and the new read aliases.

## Validation

From the OSS root:

```sh
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/phone
python3 scripts/check_catalog_description_hygiene.py --catalog appworld/phone --fail-on error
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/phone/openapi.json apis/appworld/phone
```

- Schema validation: passed, 8 entities and 21 capabilities.
- Description hygiene: 0 errors and 0 warnings.
- Existing Hermit OpenAPI validation: 30 passed, 0 warnings, 0 failures, 0 skips.
- Offline document inspection through the existing `selector_contract` example's
  `documents` command: the new relationship purpose and aliases are rendered.

Schema loading reports existing mapping-anchor leftovers. Hermit emits
identity-divergence diagnostics for its unrelated Alarm and TextMessage example
bodies; its validation checks pass. These are not relationship behavior proofs.

## Attribution limits

The retained evaluation evidence establishes omitted contact teaching but does
not retain the selector's offered candidates or its requirement-level assessment.
Therefore retrieval versus selection origin is unresolved. This audit proves a
concrete annotation gap and its removal, not the cause of a prior score or a
successful future selector decision. No embeddings were refreshed, no AppWorld
run was performed, and no grade or generalisation claim is made.
