# Product semantic-value audit

Replaced generic value references in Product and Product Query with authored domain values: unit price, product rating, seller rating, category, name, description, color, search text, delivery time, stock and individual dimensions. Price/rating bounds share their returned-value domains; seller selection shares Seller identity. Related product projections reuse the same name/category/color domains. Catalog version is 20.

Evidence source: local pinned `amazon/openapi.json`, especially `GET /amazon/products` parameter descriptions and `GET /amazon/products/{product_id}` response schema. Existing documented weight/dimension units were preserved. No new currency assumption was introduced. Product sort prose preserves the documented within-page ordering when text search is present; this change does not add an ordering enum or change runtime behavior.

Validation: `target/debug/plasm-cgs schema validate plasm-oss/apis/appworld/amazon` passed (existing unused mapping-helper warnings remain). Temporary native audit source is retained in `audit.rs`; it loaded the authored catalog, serialized/deserialized CGS, compared native discovery documents, rendered Product first-wave teaching, and checked the distinct price/product-rating/seller-rating descriptions. `generated-surfaces.txt` captures the inspected results. No generic “Numeric amount or rating” or anonymous “text (optional)” remains in Product Query/Get documents.

No backend mutation, new provider evaluation or index deployment was performed. No transport mappings or scalar wire types changed. Repacking/reindexing is required for deployed discovery to use these new descriptions. This is semantic authoring validation, not evidence that the earlier Jev miss is resolved.
