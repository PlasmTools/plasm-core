# Discovery response candidates — not deployed

The unit of wording should be **what this call established**, followed by the
unchanged native declarations. The current prefix is a generic bounded-search
warning; it does not communicate why a declaration was absent.

| Recorded state | Candidate factual wording | What it must not imply |
|---|---|---|
| New declarations | “This search added N capability declarations. Previously exposed declarations remain available.” | The task has executed or is complete. |
| No declaration delta | “This search added no declarations. Existing declarations remain available.” | Existing declarations satisfy the current request. |
| Negative judgments, unfinished search | “None of the N capabilities judged in this packet was retained. M capabilities remain unexplored.” | No suitable capability exists in the catalogue. |
| Byte deferral | “N candidates were deferred because their evidence did not fit this packet. They were not judged.” | They were rejected, or will necessarily fit on the next call. |
| Repeated judgments | “This call judged N capabilities already examined earlier; K previously unexamined capabilities were judged.” | Previous judgments remain valid after intent changes. |
| Changed intent | Report current judgments and new exploration separately from prior declaration availability. | Previously exposed operations are newly authorised effects. |

Counts must come from typed receipts, not inference from declaration text. Historical
judgment overlap requires explicit history; the current response formatter cannot
invent it. “No progress” should describe a measured absence of new examinations or
declarations, never an inferred lack of usefulness to the caller.

Avoid blanket “search again” or “execute now” advice. A candidate interface may
describe an available continuation only if the system implements it and can state
what advances. Wording cannot compensate for a search that resets its frontier.

Keep the distinction between capabilities and data, but test whether the current
imperative “extend discovery when capabilities are missing” causes futile repeats.
That is an empirical caller-behaviour hypothesis, not a deterministic test result.

Later caller comparison: identical neutral tasks, packets, declaration histories,
judgments, model and budget; baseline versus receipt-derived wording only. Score
unnecessary discovery calls, appropriate use of available reads, false catalogue
absence claims and premature completion. Include a genuine changed-intent case and
a case where another search is useful, so reduced call count alone cannot win.
