# AppWorld amazon — shopping simulation.

Task-critical surface: product search, cart line items (+ gift wrap), wish list (+ cart↔wish moves), orders, addresses, payment cards, returns, Q&A, Prime. Auth is taught (`login` → Bearer), not pre-injected.

```bash
cargo run -p plasm-cli --bin plasm-cgs -- schema validate apis/appworld/amazon
cargo run -p plasm-cli --bin plasm-cgs -- validate --spec apis/appworld/amazon/openapi.json apis/appworld/amazon
```

OpenAPI: `openapi.json` (pinned AppWorld). Backend: `http://127.0.0.1:9000` after `appworld serve apis`.

## Scope notes

- Free-text product/order lists use `kind: search` / `query` with filters.
- Cart totals via `cart_get`; line items via `cart_item_query` / `Cart.cart_items`.
- Gift wrap: `cart_add_gift_wrap` / `cart_remove_gift_wrap` on a cart line (`Product` receiver); line field `gift_wrap_quantity`, cart/order totals `gift_wrap_fee`.
- Wish list is first-class (`WishListItem` + mutators + move caps). Rating-gated cart↔wish needs `product_get` on each line (no composed `views:` yet — list rows omit rating).
- Product reviews via `ProductReview`; Q&A via `ProductQuestion` / `ProductQuestionAnswer` (list + write).
- Account signup/verify/password-reset are mapped; public profile remains `profile_get` (`/profile`).
