# ERC-20 Token — Plasm CGS Schema

A [Plasm](../../README.md) domain model for any [ERC-20](https://eips.ethereum.org/EIPS/eip-20) token on any EVM-compatible chain. Covers the two most useful read operations: querying the token balance of an address and scanning Transfer event logs over a block range.

The default contract address is [USDC on Ethereum mainnet](https://etherscan.io/address/0xa0b86991c6218b36c1d19d4a2e9eb0ce3606eb48). Swap the `value` field in `mappings.yaml` for any other ERC-20 without touching the domain model.

```bash
# Read USDC balance for an address (Infura RPC)
export ETH_RPC_URL=https://mainnet.infura.io/v3/<PROJECT_ID>
cargo run -p plasm-repl --features evm -- \
  --schema apis/evm-erc20 \
  --backend "$ETH_RPC_URL"
```

Inside the REPL, use teaching-table programs (wire names + `e#` / `m#`), for example:

```text
e1(address="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")
e2{from_block=21000000, to_block=21001000}
```

> **EVM feature flag** — this schema requires the `evm` Cargo feature on `plasm-repl`:
> `cargo run -p plasm-repl --features evm -- --schema apis/evm-erc20 --backend "$ETH_RPC_URL"`

---

## What the CGS design is

A CGS (Capability Graph Schema) describes business objects and the operations available on them — explicitly **not** a mirror of the ABI. Where a Solidity ABI describes function selectors and event signatures, a CGS describes the entities those calls produce and the capabilities available for querying them.

The two files:

**`domain.yaml`** — the semantic model. Declares entities (`Balance`, `Transfer`), their fields, and capability signatures. No Solidity or EVM details.

**`mappings.yaml`** — the EVM wiring. Declares how each capability compiles to an `eth_call` or `eth_getLogs` request using CML (Capability Mapping Language) with the `evm_call` / `evm_logs` transports.

### Transports

**`evm_call`** — compiles to an `eth_call` JSON-RPC request. Used for view functions (no state change, no gas). The response is ABI-decoded and projected into entity fields via the `decode` block.

**`evm_logs`** — compiles to `eth_getLogs` over a block range. Indexed event parameters become filter topics; data parameters are decoded from the log body. The `pagination.location: block_range` config splits the range into chunks of `range_size` blocks per request.

### RPC authentication

Pass the JSON-RPC endpoint as `--backend`. For authenticated endpoints (Alchemy, Infura, QuickNode) include the key in the URL:

```bash
--backend https://eth-mainnet.g.alchemy.com/v2/<ALCHEMY_API_KEY>
--backend https://mainnet.infura.io/v3/<INFURA_PROJECT_ID>
```

For header-based auth (e.g. a self-hosted node), the runtime's `auth` config accepts bearer tokens and custom headers — see the Plasm auth documentation.

### Block-range pagination

ERC-20 Transfer logs can span millions of blocks. `transfer_query` paginates automatically by splitting the requested range into chunks of `range_size` blocks (default: 1 000) and issuing one `eth_getLogs` call per chunk.

```bash
# Scan a 10 000-block range — issues 10 requests of 1 000 blocks each
transfer query --from-block 21000000 --to-block 21010000 --all

# Without --all, only the first chunk is fetched
transfer query --from-block 21000000 --to-block 21010000
```

The `range_size` param in `mappings.yaml` controls chunk size. Increase it for less-active tokens; decrease it for very dense tokens where `eth_getLogs` responses exceed the node's result size limit.

---

## Entities

### `Balance`

| Field | Type | Description |
|-------|------|-------------|
| `account` | `string` | Wallet address queried via `balanceOf` (also the entity ID) |
| `balance` | `string` | Token balance as a decimal `uint256` string |

### `Transfer`

| Field | Type | Description |
|-------|------|-------------|
| `event_id` | `string` | Stable log identity — `<tx_hash>:<log_index>` (entity ID) |
| `tx_hash` | `string` | Transaction hash that emitted the log |
| `log_index` | `integer` | Log index within the block |
| `from` | `string` | Sender address (indexed topic) |
| `to` | `string` | Recipient address (indexed topic) |
| `value` | `string` | Transferred amount as a decimal `uint256` string |

> Amounts are returned as raw `uint256` decimal strings. USDC has 6 decimals, so divide by `1e6` to get human-readable USDC. WETH has 18 decimals.

---

## Capabilities

| Capability | Kind | CLI | EVM call |
|------------|------|-----|----------|
| `balance_get` | get | `balance <address>` | `eth_call` → `balanceOf(address)` |
| `transfer_query` | query | `transfer query` | `eth_getLogs` → `Transfer(from, to, value)` |

---

## REPL examples

```bash
export RPC=https://mainnet.infura.io/v3/<PROJECT_ID>
cargo run -p plasm-repl --features evm -- --schema apis/evm-erc20 --backend "$RPC"
```

Illustrative programs (substitute session symbols from teaching TSV):

```text
# Balance of an address
e1(address="0xd8dA6BF26964aF9D7eEd9e03E53415D37aA96045")

# Transfer logs over a block range (wire filter keys from teaching TSV)
e2{from_block=21500000, to_block=21500000}
```

---

## Adapting for other tokens

Change the `contract.value` in `mappings.yaml` and update `chain`:

```yaml
# WETH on mainnet
contract:
  type: const
  value: "0xc02aaa39b223fe8d0a0e5c4f27ead9083c756cc2"

# USDC on Base (chain 8453)
chain: 8453
contract:
  type: const
  value: "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913"

# USDT on Polygon (chain 137)
chain: 137
contract:
  type: const
  value: "0xc2132D05D31c914a87C6611C10748AEb04B58e8F"
```

The domain model (`domain.yaml`) is identical for every standard ERC-20 — only the address and chain in `mappings.yaml` change.

---

## Testing status

Tested against a local [Anvil](https://book.getfoundry.sh/reference/anvil/) node with a deployed test ERC-20 contract via the `plasm-e2e` crate:

```bash
# Unit tests (compile-time validation, no network)
cargo test -p plasm-compile -p plasm-runtime -p plasm \
  --lib --features plasm-compile/evm,plasm-runtime/evm,plasm/evm

# Auth e2e (mock RPC server, no Docker)
cargo test -p plasm-e2e --test evm_auth_e2e --features plasm-e2e/evm

# Full e2e with Anvil (requires Docker)
cargo test -p plasm-e2e --test evm_e2e --features plasm-e2e/evm
```

Live mainnet testing against a public RPC has not been performed. The ABI signatures and decode mappings are verified against the canonical ERC-20 ABI.

---

## Known limitations

**`value` is a raw `uint256` string** — no decimal scaling is applied. Divide by `10 ** decimals` in your downstream code. The `decimals()` view function is not modelled here.

**Indexed topic filters not yet exposed on the CLI** — `eth_getLogs` supports filtering by indexed topics (`from`, `to`). The mapping supports a `topics` field for compile-time filters; runtime topic filtering via CLI flags is not yet wired.

**Open-ended block ranges** — omitting `--to-block` fetches until the current chain head chunk is empty. For high-activity tokens this may produce very large result sets; always set `--to-block` on mainnet.
