# AppWorld amazon — shopping simulation.

Task-critical surface: product search, cart line items, wish list (+ cart↔wish moves), orders, addresses, payment cards. Auth is taught (`login` → Bearer), not pre-injected.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/amazon
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/amazon/openapi.json apis/appworld/amazon
```

OpenAPI: `openapi.json` (pinned AppWorld). Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- Free-text product/order lists use `kind: search`.
- Cart totals via `cart_get`; line items via `cart_item_query` / `Cart.cart_items`.
- Wish list is first-class (`WishListItem` + mutators + move caps). Rating-gated cart↔wish needs `product_get` on each line (no composed `views:` yet — list rows omit rating).
- Product reviews via `ProductReview` entity (`product_review_query`, `product_review_create`, `product_review_update`, `product_review_delete`).
