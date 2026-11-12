# v2.1 schema-validity erratum

v2 was frozen with two Get operations on Source but no primary_read. Every case
failed schema validation before indexing or selection. Those results and the v2
lock are retained. No retrieval quality observations informed this correction.

Declare Source.primary_read = source_a in the local and cross base schemas.
Both supplier capabilities remain present and explicitly nullary in the fixture
builder. Remove the source entity's primary-read reference when splitting the
engine catalogue, because that catalogue does not own either source read.
This resolves schema validity; it is not a ranking boost or supplier-selection
rule. Cross-provider choice still comes only from the explicit deployment binding.

cases.json and noise.json are byte-identical to v2. Requests, contribution labels,
path alternatives, growth levels, deadlines and production budgets are unchanged.
Freeze this revision before running it. v2 remains immutable invalid evidence.
