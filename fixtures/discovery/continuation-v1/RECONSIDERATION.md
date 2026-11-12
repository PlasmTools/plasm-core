# Frozen continuation follow-up (before behavior changes)

Development controls only; no holdout or agent-success claim. No model calls.

Factor 1: scope exploration visits to the exact self-contained current request,
while retaining persisted request-specific progress. Do not clear any records.
Session, generation, authorization and model remain identity inputs. Exact
completed or failed work retains its ownership key and spent budget. Ancestry
remains part of search-work identity, allowing deliberate continuation within
the same request; it must not carry rejection priority into a different request.

Controls: inspect -> inspect continuation advances; inspect -> calibrate begins
with no inherited rejection priority; calibrate -> inspect restores prior
inspection progress. Exact-envelope replay across independent stores and after
reconnection must make zero additional calls. Existing cancelled/failed-owner
proofs must pass. Test an authorization change independently of request text.

Factor 2, only if demonstrated: independent recalled operations A and B compete
with 8 and 64 consumers of an exposed producer. Each contract fits alone but
the packet cannot fit all operations. With fixed recall order, retain judged
relevant operations, record judged visits, and require both A and B within two
searches. Existing connected-continuation controls remain required. Do not
increase root count, bytes, search horizon or transport budget to pass.

Run the independent-outcome control twice: first root retained, and first root
rejected. A rejection must advance the leading recall opportunity to B rather
than let every connected neighbour run ahead of B.

Judges: request frontier identity, per-request persisted visit counts, provider
callback counts, native planner question membership and exact packet bytes.
Fixtures and assertions are frozen before observing these new results. Labels
are assertions only, never supplied as decision instructions.
