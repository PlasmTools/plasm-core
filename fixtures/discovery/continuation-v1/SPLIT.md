# Split admission experiment, frozen before execution

The existing reformulation fixtures and six-call / 16,000-byte budgets remain
unchanged. Add a third arm: the highest current recall candidate gets the first
root opportunity, followed by candidates ordered by session visit count and
original recall order. The production allocator constructs the exact packet.
No relevance decisions are inherited. No model calls or task-specific boosts.

Report target arrival, unique/repeated judgments and maximum packet bytes for
all existing scenarios and both distractor sizes. Require changed-constraint and
new-outcome arrival on call one, and reformulation arrival by call six.
These are necessary, not sufficient, promotion gates: the independent-operation
fixture does not validate connected paths, live retrieval, or single-root packet
capacity. Passing this experiment does not authorize production promotion.

Keep both original arms. A repeated first root is an explicit measured cost.
