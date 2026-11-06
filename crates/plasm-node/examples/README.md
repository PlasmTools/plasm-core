# Diagnostic controller contract

`system_harness` and `system_ablation` are standalone JSON-line diagnostic
bridges. They are not the AppWorld agent pipeline or production discovery.

For every non-ceiling `discover` command, the controller must now supply a
`recall` object serialized from the public `DiscoveryStore` `RetrievalReceipt`
for the pinned generation. The initialization message must also supply
`generation` identifying that same pinned generation. The bridge calls
`online::select_retrieved`, which validates the generation, authorization,
bounded candidate set, and canonical catalog documents before judgment.
Missing `recall` is rejected with `discover requires pinned recall receipt`.
The controller still supplies `intent` and handles decision/HTTP events.

The former implicit lexical enumeration is test-only and is no longer exposed
to these binaries. Historical controller scripts that send only `intent` must
acquire and send a real pinned retrieval receipt before they can run a
non-ceiling arm. No such controller is present in this repository. This is an
explicit diagnostic protocol change; it does not claim parity with historical
lexical diagnostic results. The separately labeled ceiling arm remains an
explicit controller-selected diagnostic ceiling.
