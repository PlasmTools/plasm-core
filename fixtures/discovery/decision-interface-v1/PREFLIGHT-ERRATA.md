# Preparation correction, before any model calls

The first native export rejected an unused Token entity without a capability.
The original frozen inputs, schemas, source hashes and failed export are preserved
in eval-reports/decision-interface-v1-20261030. Revision 2 removes only the unused
Token entity from the 40 non-witness catalogues. Requests, labels, operations,
descriptions, semantic slots and the eight witness catalogues are unchanged.
LOCK-r2.json freezes corrected inputs before a second native preparation.
No provider request occurred in the rejected preparation.
