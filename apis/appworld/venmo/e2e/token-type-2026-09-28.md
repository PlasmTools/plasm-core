# Login token type correction — 2026-09-28

The pinned AppWorld worker implementation in `appworld/apps/lib/apis/authentication.py`
returns `{"access_token": access_token, "token_type": "Bearer"}`. Its response model
declares `token_type: str`. Venmo CGS previously declared only lowercase `bearer`;
strict decoding correctly rejected the actual response. The authored enum now
matches the pinned implementation's exact literal. No case normalization or
relaxation of the decoder was introduced.

Validation: all ten AppWorld catalogs pack; only Venmo's manifest changes.
A packaged NAPI test derives the login symbols from teaching, admits a Python
Program, invokes the real CML/decoder through a controlled HTTP callback, accepts
`Bearer`, and rejects `bearer` with `cause=response_contract`. Synthetic credentials
are fixture values. This is focused transport/codec evidence, not an independent
attestation system or proof of every catalog capability.

Hermit was not run for this focused check: the relevant source response schema
admits a string and cannot establish its capitalization. The pinned implementation
and controlled real-decoder test establish this regression directly. Live evidence
uses isolated AppWorld development worlds, not the external Venmo service.

Diagnostic run evidence: `target/appworld-failure-smoke-20260928T222627Z/build3.log`
and `build-context/token-contract-proof.cjs` in the parent workspace. Native
smoke outcomes are recorded separately in that run's `REPORT.md`.
