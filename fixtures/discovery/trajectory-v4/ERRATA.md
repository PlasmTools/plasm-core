# Harness corrections before quality measurement

The first baseline invocation on 2026-10-30 stopped on `Response(UnexpectedProvider)` at its first fixture response. The native parser requires `provider: OpenAI`; the offline adapter had emitted `fixture`. Corrected only the response envelope, retaining `fixture_response: true`, all frozen requests/contracts/labels and every selection limit. No provider was called and no quality results were produced by that attempt. Its TEST.log and source manifest remain in `eval-reports/discovery-trajectory-v4-baseline-20261030`.

The corrected run uses a fresh directory, `eval-reports/discovery-trajectory-v4-baseline-20261030-r2`.
