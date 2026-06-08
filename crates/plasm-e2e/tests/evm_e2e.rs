#![cfg(feature = "evm")]

mod common;

use alloy::primitives::{Address, U256};
use common::evm::{make_engine, EvmTestContext, ANVIL_ADDRESS_1, ANVIL_ADDRESS_2};
use plasm_core::{loader, Expr, GetExpr, QueryExpr, QueryPagination};
use plasm_runtime::{ExecuteOptions, ExecutionMode, SessionMaterialization, StreamConsumeOpts};

#[tokio::test]
async fn evm_call_get_token_balance_works() {
    let ctx = EvmTestContext::new().await;
    let token = ctx.deploy_test_erc20().await;
    ctx.mint(token, ANVIL_ADDRESS_1, U256::from(42_u64)).await;

    let schema_dir = ctx.write_schema_dir(token);
    let cgs = loader::load_schema_dir(schema_dir.path()).unwrap();
    let engine = make_engine(&ctx.endpoint);
    let mut cache = SessionMaterialization::new();

    let result = engine
        .execute(
            &Expr::Get(GetExpr::new("Balance", ANVIL_ADDRESS_1.to_string())),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts::default(),
            ExecuteOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(result.count, 1);
    let entity = &result.entities[0];
    let expected_account = ANVIL_ADDRESS_1.to_string();
    assert_eq!(
        entity.fields.get("account").and_then(|v| v.as_str()),
        Some(expected_account.as_str())
    );
    assert_eq!(
        entity.fields.get("balance").and_then(|v| v.as_str()),
        Some("42")
    );
}

#[tokio::test]
async fn evm_logs_query_transfer_events_works() {
    let ctx = EvmTestContext::new().await;
    let token = ctx.deploy_test_erc20().await;
    ctx.mint(token, ANVIL_ADDRESS_1, U256::from(7_u64)).await;
    ctx.mint(token, ANVIL_ADDRESS_2, U256::from(9_u64)).await;

    let schema_dir = ctx.write_schema_dir(token);
    let cgs = loader::load_schema_dir(schema_dir.path()).unwrap();
    let engine = make_engine(&ctx.endpoint);
    let latest_block = ctx.latest_block().await;
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("Transfer");
    query.pagination = Some(QueryPagination {
        from_block: Some(0),
        to_block: Some(latest_block),
        ..QueryPagination::default()
    });

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: false,
                ..Default::default()
            },
            ExecuteOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(result.count, 2);
    assert!(contains_transfer(&result.entities, ANVIL_ADDRESS_1, "7"));
    assert!(contains_transfer(&result.entities, ANVIL_ADDRESS_2, "9"));
}

#[tokio::test]
async fn evm_logs_block_range_pagination_works() {
    let ctx = EvmTestContext::new().await;
    let token = ctx.deploy_test_erc20().await;
    ctx.mint(token, ANVIL_ADDRESS_1, U256::from(1_u64)).await;
    ctx.mint(token, ANVIL_ADDRESS_2, U256::from(2_u64)).await;
    ctx.mint(token, ANVIL_ADDRESS_1, U256::from(3_u64)).await;

    let schema_dir = ctx.write_schema_dir(token);
    let cgs = loader::load_schema_dir(schema_dir.path()).unwrap();
    let engine = make_engine(&ctx.endpoint);
    let latest_block = ctx.latest_block().await;
    let mut cache = SessionMaterialization::new();

    let mut query = QueryExpr::all("Transfer");
    query.pagination = Some(QueryPagination {
        from_block: Some(0),
        to_block: Some(latest_block),
        ..QueryPagination::default()
    });

    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut cache,
            Some(ExecutionMode::Live),
            StreamConsumeOpts {
                fetch_all: true,
                max_items: None,
                one_page: false,
                graph_backed_result: false,
                ..Default::default()
            },
            ExecuteOptions::default(),
        )
        .await
        .unwrap();

    assert_eq!(result.count, 3);
    assert!(
        result.stats.network_requests >= 2,
        "expected multiple paginated eth_getLogs requests, got {}",
        result.stats.network_requests
    );
}

fn contains_transfer(entities: &[plasm_runtime::CachedEntity], to: Address, value: &str) -> bool {
    let expected_to = to.to_string();
    entities.iter().any(|entity| {
        entity.fields.get("to").and_then(|v| v.as_str()) == Some(expected_to.as_str())
            && entity.fields.get("value").and_then(|v| v.as_str()) == Some(value)
    })
}
