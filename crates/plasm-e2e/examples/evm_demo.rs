use alloy::primitives::U256;
use plasm_core::{loader, Expr, GetExpr, QueryExpr, QueryPagination};
use plasm_e2e::evm_support::{make_engine, EvmTestContext, ANVIL_ADDRESS_1, ANVIL_ADDRESS_2};
use plasm_runtime::{ExecuteOptions, ExecutionMode, SessionMaterialization, StreamConsumeOpts};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("Starting local Anvil via testcontainers");
    let ctx = EvmTestContext::new().await;
    println!("RPC endpoint: {}", ctx.endpoint);

    let token = ctx.deploy_test_erc20().await;
    println!("Deployed SimpleERC20 at {}", token);

    ctx.mint(token, ANVIL_ADDRESS_1, U256::from(42_u64)).await;
    ctx.mint(token, ANVIL_ADDRESS_2, U256::from(9_u64)).await;
    println!("Minted 42 tokens to {}", ANVIL_ADDRESS_1);
    println!("Minted 9 tokens to {}", ANVIL_ADDRESS_2);

    let schema_dir = ctx.write_schema_dir(token);
    let cgs = loader::load_schema_dir(schema_dir.path()).map_err(std::io::Error::other)?;
    let engine = make_engine(&ctx.endpoint);
    let mut mat = SessionMaterialization::new();

    let balance = engine
        .execute(
            &Expr::Get(GetExpr::new("Balance", ANVIL_ADDRESS_1.to_string())),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await?;
    assert_eq!(balance.count, 1, "expected one balance entity");

    println!();
    println!("Balance GET result:");
    println!("{}", serde_json::to_string_pretty(&balance)?);

    let latest_block = ctx.latest_block().await;
    let mut query = QueryExpr::all("Transfer");
    query.pagination = Some(QueryPagination {
        from_block: Some(0),
        to_block: Some(latest_block),
        ..QueryPagination::default()
    });

    let transfers = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut mat,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: false,
            },
            ExecuteOptions::default(),
        )
        .await?;
    assert_eq!(transfers.count, 2, "expected two transfer logs");
    assert!(
        transfers.stats.network_requests >= 2,
        "expected block-range pagination to span multiple requests"
    );

    println!();
    println!("Transfer QUERY result:");
    println!("{}", serde_json::to_string_pretty(&transfers)?);

    Ok(())
}
