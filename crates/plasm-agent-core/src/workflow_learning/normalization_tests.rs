use super::*;
fn request() -> NormalizationRequest {
    NormalizationChat::configured(None)
        .unwrap()
        .request(
            serde_json::json!({"subtask":"inspect"}),
            BTreeSet::from(["known".into()]),
        )
        .unwrap()
}
fn proposal() -> serde_json::Value {
    serde_json::json!({"intent":{"purpose":"inspect","facets":[],"aliases":[]},"family":{"kind":"new"}})
}
fn envelope(proposal: serde_json::Value) -> String {
    serde_json::json!({"choices":[{"message":{"role":"assistant","content":proposal.to_string()}}],"usage":{"prompt_tokens":17,"completion_tokens":9}}).to_string()
}
#[test]
fn strict_normalization_schema_matches_domain_bounds_and_family_alternatives() {
    let request = request();
    assert_eq!(
        request.endpoint(),
        "https://openrouter.ai/api/v1/chat/completions"
    );
    let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
    assert_eq!(body["model"], "openai/gpt-6-luna");
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
    let schema = &body["response_format"]["json_schema"]["schema"];
    assert_eq!(schema["additionalProperties"], false);
    assert_eq!(
        schema["properties"]["intent"]["properties"]["facets"]["maxItems"],
        FACET_COUNT
    );
    assert_eq!(
        schema["properties"]["intent"]["properties"]["aliases"]["maxItems"],
        ALIAS_COUNT
    );
    assert_eq!(
        schema["properties"]["family"]["anyOf"][2]["properties"]["family"]["enum"],
        serde_json::json!(["known"])
    );
    assert!(matches!(
        request.decode(&envelope(proposal())).unwrap().family(),
        FamilyMatch::New
    ));
    let mut data = proposal();
    data["family"] = serde_json::json!({"kind":"existing","family":"known"});
    let normalized = request.decode(&envelope(data)).unwrap();
    assert!(
        matches!(normalized.family(),FamilyMatch::Existing {family} if family.as_str()=="known")
    );
}
#[test]
fn normalization_rejections_identify_exact_predicates() {
    let request = request();
    let mut data = proposal();
    data["intent"]["purpose"] = "  ".into();
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::BlankText {
            field: NormalizationField::Purpose,
            ..
        })
    ));
    let mut data = proposal();
    data["intent"]["facets"] = serde_json::json!(vec!["x"; 33]);
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::TooManyItems {
            field: NormalizationField::Facet,
            actual: 33,
            limit: 32
        })
    ));
    let mut data = proposal();
    data["intent"]["aliases"] = serde_json::json!(["x".repeat(ALIAS_LIMIT + 1)]);
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::TextTooLong {
            field: NormalizationField::Alias,
            ..
        })
    ));
    let mut data = proposal();
    data["family"] = serde_json::json!({"kind":"existing","family":"invented"});
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::UnknownFamily)
    ));
    let mut data = proposal();
    data["family"] = "new".into();
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::Proposal(_))
    ));
    let mut data = proposal();
    data["extra"] = true.into();
    assert!(matches!(
        request.decode(&envelope(data)),
        Err(NormalizationError::Proposal(_))
    ));
    assert!(matches!(
        request.decode(r#"{"answers":{}}"#),
        Err(NormalizationError::Envelope(_))
    ));
    assert!(matches!(
        request.decode(r#"{"choices":[]}"#),
        Err(NormalizationError::MissingChoice)
    ));
}
#[test]
fn explicit_model_and_no_existing_family_have_exact_contracts() {
    assert!(NormalizationChat::configured(Some(" ".into())).is_err());
    let request = NormalizationChat::configured(Some("offline/model".into()))
        .unwrap()
        .request(serde_json::json!({}), BTreeSet::new())
        .unwrap();
    let body: serde_json::Value = serde_json::from_str(request.body()).unwrap();
    assert_eq!(request.model(), "offline/model");
    assert_eq!(
        body["response_format"]["json_schema"]["schema"]["properties"]["family"]["anyOf"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
}
#[tokio::test]
async fn normalization_chat_uses_actual_transport_with_strict_schema() {
    use plasm_runtime::provider_transport::{request_attempt, ReqwestProviderTransport};
    let request = request();
    let response = envelope(proposal());
    let route = reqwest::Url::parse(request.endpoint()).unwrap();
    let router = axum::Router::new().route(
        route.path(),
        axum::routing::post(
            move |headers: axum::http::HeaderMap,
                  axum::Json(body): axum::Json<serde_json::Value>| {
                let response = response.clone();
                async move {
                    assert_eq!(headers["authorization"], "Bearer offline-fixture");
                    assert_eq!(body["response_format"]["type"], "json_schema");
                    assert_eq!(body["response_format"]["json_schema"]["strict"], true);
                    response
                }
            },
        ),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}{}", listener.local_addr().unwrap(), route.path());
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let transport = ReqwestProviderTransport::new(
        reqwest::Client::builder().no_proxy().build().unwrap(),
        endpoint,
        "offline-fixture",
    );
    let raw = request_attempt(
        &transport,
        request.body(),
        0,
        std::time::Duration::from_secs(5),
        |_| Ok(()),
    )
    .await
    .unwrap();
    assert_eq!(request.decode(&raw).unwrap().intent().purpose, "inspect");
    server.abort();
}
