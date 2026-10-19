# Catalog publication

`plasm-pack-catalogs` compiles catalog sources and packages discovery documents and
embeddings. Build it with `cargo build -p plasm-catalog --bin plasm-pack-catalogs`.

This crate depends on schema and compilation, not `plasm-agent-core`, the runtime,
or MCP. The host uses its embedding client for query retrieval; publication must
never depend on the host. Existing vector validation, cache locking and atomic
publication remain shared here.

The default `evm` feature preserves the packer's existing EVM catalog support,
previously enabled indirectly by the runtime dependency.

The executable name and artifact format are unchanged. There is no packer binary
or embedding-module re-export in the host compatibility crate.
