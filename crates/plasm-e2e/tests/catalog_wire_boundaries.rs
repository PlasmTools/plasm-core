//! Intentional packaged-catalog integration smokes, not language conformance tests.
//! Request-expression tests do not certify runtime pagination injection or decoding.
use plasm_compile::{
    compile_operation, CapabilityTemplate, CmlRequest, CompiledOperation, CompiledRequest,
    PaginationParam, PaginationParamRole, ResponsePreprocess,
};
use plasm_core::{
    Expr, FieldType, GetExpr, Predicate, QueryExpr, RelationMaterialization, Value, CGS,
};
use plasm_runtime::{
    ExecuteOptions, ExecutionConfig, ExecutionEngine, SessionMaterialization, StreamConsumeOpts,
};
use serde_json::json;
use std::{collections::HashSet, path::PathBuf};

fn root(api: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../apis")
        .join(api)
}

fn catalog(api: &str) -> CGS {
    plasm_core::load_schema_dir(&root(api)).unwrap()
}

fn request(api: &str, capability: &str, input: serde_json::Value) -> CompiledRequest {
    let cgs = catalog(api);
    let compiled = plasm_compile::compile_cgs_capability_templates(&cgs).unwrap();
    let env = serde_json::from_value(input).unwrap();
    match compile_operation(compiled.capability(capability).unwrap(), &env).unwrap() {
        CompiledOperation::Http(request) => request,
        other => panic!("expected HTTP request, got {other:?}"),
    }
}

fn json_value(value: &Option<Value>) -> serde_json::Value {
    serde_json::to_value(value.as_ref().unwrap()).unwrap()
}

fn template(api: &str, capability: &str) -> CmlRequest {
    let compiled = plasm_compile::compile_cgs_capability_templates(&catalog(api)).unwrap();
    match compiled.capability(capability).unwrap() {
        CapabilityTemplate::Http(request) => request.clone(),
        other => panic!("expected HTTP template, got {other:?}"),
    }
}

#[test]
fn owned_catalogs_have_no_duplicate_yaml_keys_and_compile() {
    // serde_yaml::Value rejects duplicate mapping keys rather than silently
    // accepting the final OAuth declaration as an IndexMap loader can do.
    for api in [
        "linkedin",
        "tavily",
        "twitter",
        "jira",
        "slack",
        "tau3_banking",
    ] {
        for file in ["domain.yaml", "mappings.yaml"] {
            let text = std::fs::read_to_string(root(api).join(file)).unwrap();
            serde_yaml::from_str::<serde_yaml::Value>(&text)
                .unwrap_or_else(|error| panic!("{api}/{file}: {error}"));
        }
        plasm_compile::compile_cgs_capability_templates(&catalog(api)).unwrap();
    }
}

#[test]
fn linkedin_author_shelves_bind_the_native_author_query() {
    for (capability, input, expected) in [
        (
            "ugc_post_query_for_member",
            json!({"member":"42"}),
            "List(urn%3Ali%3Aperson%3A42)",
        ),
        (
            "organization_posts_query",
            json!({"organization":"7"}),
            "List(urn%3Ali%3Aorganization%3A7)",
        ),
        (
            "ugc_posts_query",
            json!({"authors":"List(urn%3Ali%3Aperson%3A9)"}),
            "List(urn%3Ali%3Aperson%3A9)",
        ),
    ] {
        let wire = request("linkedin", capability, input);
        assert_eq!(wire.path, "/v2/ugcPosts");
        assert_eq!(json_value(&wire.query)["authors"], expected);
    }
}

#[test]
fn tavily_crawl_preserves_its_fields_without_extract_body_leakage() {
    let input = json!({"shelf":"site_crawl","url":"https://example.com",
        "instructions":"Follow documentation","max_depth":2,"max_breadth":4,"limit":8,
        "urls":["https://wrong.example"],"query":"extract only"});
    let wire = request("tavily", "url_extract", input);
    assert_eq!(wire.path, "/crawl");
    assert_eq!(
        json_value(&wire.body),
        json!({"url":"https://example.com",
        "instructions":"Follow documentation","max_depth":2,"max_breadth":4,"limit":8})
    );
    let wire = request(
        "tavily",
        "url_extract",
        json!({"shelf":"url_extract",
        "urls":["https://example.com"],"query":"facts","url":"https://wrong.example"}),
    );
    assert_eq!(wire.path, "/extract");
    assert_eq!(
        json_value(&wire.body),
        json!({"urls":["https://example.com"],"query":"facts"})
    );
}

#[test]
fn twitter_readers_select_native_paths_and_queries() {
    // Foreign inputs deliberately probe isolation; pagination is checked below.
    for (capability, path, expected) in [
        ("tweet_lookup_query", "/2/tweets", json!({"ids":"1,2"})),
        (
            "user_posts_query",
            "/2/users/7/tweets",
            json!({"exclude":"replies","since_id":"9"}),
        ),
        (
            "user_mentions_query",
            "/2/users/7/mentions",
            json!({"since_id":"9"}),
        ),
        (
            "user_liked_posts_query",
            "/2/users/7/liked_tweets",
            json!({}),
        ),
        ("list_posts_query", "/2/lists/8/tweets", json!({})),
    ] {
        let wire = request(
            "twitter",
            capability,
            json!({"user":"7","list":"8","ids":["1","2"],"exclude":["replies"],"since_id":"9"}),
        );
        assert_eq!(wire.path, path);
        assert_eq!(json_value(&wire.query), expected);
    }
}

#[test]
fn slack_readers_do_not_leak_inputs_between_native_contracts() {
    for (capability, path, expected) in [
        (
            "channel_members_query",
            "/conversations.members",
            json!({"channel":"C1"}),
        ),
        (
            "workspace_users_query",
            "/users.list",
            json!({"include_locale":true}),
        ),
        (
            "usergroup_members_query",
            "/usergroups.users.list",
            json!({"usergroup":"S1","include_disabled":true}),
        ),
    ] {
        let wire = request(
            "slack",
            capability,
            json!({"channel":"C1","usergroup":"S1","include_locale":true,"include_disabled":true}),
        );
        assert_eq!(wire.path, path);
        assert_eq!(json_value(&wire.query), expected);
    }
    for (capability, path, expected_ts) in [
        ("channel_history", "/conversations.history", None),
        (
            "thread_messages_query",
            "/conversations.replies",
            Some("123.456"),
        ),
    ] {
        let wire = request("slack", capability, json!({"channel":"C1","ts":"123.456"}));
        assert_eq!(wire.path, path);
        let query = json_value(&wire.query);
        assert_eq!(
            query.get("ts").and_then(|value| value.as_str()),
            expected_ts
        );
    }
}

#[test]
fn banking_name_lookup_and_locked_facet_use_declared_wire_inputs() {
    for (input, suffix, expected) in [
        (
            json!({"shelf":"by_name","customer_name":"Alex","email":"wrong@example.com"}),
            "get_user_information_by_name",
            json!({"customer_name":"Alex"}),
        ),
        (
            json!({"shelf":"by_email","customer_name":"wrong","email":"alex@example.com"}),
            "get_user_information_by_email",
            json!({"email":"alex@example.com"}),
        ),
    ] {
        let wire = request("tau3_banking", "get_user_information_by_email", input);
        assert_eq!(wire.path, format!("/v1/tau3/banking/{suffix}"));
        assert_eq!(json_value(&wire.body), expected);
    }
    let wire = request(
        "tau3_banking",
        "CreditCardTransactionLocked_list_by_user",
        json!({"user_id":"7"}),
    );
    assert_eq!(
        wire.path,
        "/v1/tau3/banking/get_credit_card_transactions_by_user"
    );
    assert_eq!(json_value(&wire.body), json!({"user_id":"7"}));
}

#[test]
fn named_relations_bind_every_required_source_input() {
    for (api, entity, relation, capability, param, target) in [
        (
            "linkedin",
            "Member",
            "feed_posts",
            "ugc_post_query_for_member",
            "member",
            "MemberPost",
        ),
        (
            "linkedin",
            "Organization",
            "feed_posts",
            "organization_posts_query",
            "organization",
            "OrganizationPost",
        ),
        (
            "twitter",
            "User",
            "tweets",
            "user_posts_query",
            "user",
            "AuthoredPost",
        ),
        (
            "twitter",
            "User",
            "mentions",
            "user_mentions_query",
            "user",
            "MentionedPost",
        ),
        (
            "twitter",
            "User",
            "liked_tweets",
            "user_liked_posts_query",
            "user",
            "LikedPost",
        ),
        (
            "twitter",
            "List",
            "tweets",
            "list_posts_query",
            "list",
            "ListPost",
        ),
        (
            "jira",
            "Issue",
            "watchers",
            "issue_watcher_query",
            "issueIdOrKey",
            "User",
        ),
        (
            "slack",
            "Channel",
            "members",
            "channel_members_query",
            "channel",
            "ChannelMember",
        ),
        (
            "slack",
            "Channel",
            "messages",
            "channel_history",
            "channel",
            "Message",
        ),
        (
            "slack",
            "UserGroup",
            "members",
            "usergroup_members_query",
            "usergroup",
            "UserGroupMember",
        ),
    ] {
        let cgs = catalog(api);
        let edge = &cgs.entities[entity].relations[relation];
        assert_eq!(edge.target_resource.as_str(), target);
        match edge.materialize.as_ref().unwrap() {
            RelationMaterialization::QueryScoped {
                capability: actual,
                param: binding,
            } => {
                assert_eq!(actual.as_str(), capability);
                assert_eq!(binding.as_str(), param);
            }
            other => panic!("{api}/{entity}.{relation}: unexpected binding {other:?}"),
        }
        let reader = &cgs.capabilities[capability];
        assert_eq!(reader.domain.as_str(), target);
        let required: Vec<_> = reader
            .inputs
            .query_source_fields()
            .filter(|field| field.required)
            .map(|field| field.name.as_str())
            .collect();
        assert_eq!(
            required,
            vec![param],
            "{api}/{capability}: unbound selector"
        );
    }
}

#[test]
fn native_readers_and_relations_reject_incomplete_or_wrong_grants() {
    for (api, capability, relation, required) in [
        (
            "linkedin",
            "ugc_post_query_for_member",
            "Member.feed_posts",
            vec!["r_member_social"],
        ),
        (
            "linkedin",
            "organization_posts_query",
            "Organization.feed_posts",
            vec!["r_organization_social"],
        ),
        (
            "twitter",
            "user_posts_query",
            "User.tweets",
            vec!["tweet.read", "users.read"],
        ),
        (
            "twitter",
            "user_mentions_query",
            "User.mentions",
            vec!["tweet.read", "users.read"],
        ),
        (
            "twitter",
            "user_liked_posts_query",
            "User.liked_tweets",
            vec!["tweet.read", "users.read", "like.read"],
        ),
        (
            "twitter",
            "list_posts_query",
            "List.tweets",
            vec!["tweet.read", "users.read", "list.read"],
        ),
        (
            "jira",
            "issue_watcher_query",
            "Issue.watchers",
            vec!["read:jira-work"],
        ),
        (
            "slack",
            "channel_members_query",
            "Channel.members",
            vec!["channels:read"],
        ),
        (
            "slack",
            "usergroup_members_query",
            "UserGroup.members",
            vec!["usergroups:read"],
        ),
    ] {
        let cgs = catalog(api);
        let grants: HashSet<String> = required.iter().map(|scope| (*scope).into()).collect();
        assert_eq!(
            cgs.oauth_capability_satisfied(capability, &grants),
            Some(true)
        );
        assert_eq!(cgs.oauth_relation_satisfied(relation, &grants), Some(true));
        for scope in &required {
            let mut incomplete = grants.clone();
            incomplete.remove(*scope);
            assert_eq!(
                cgs.oauth_capability_satisfied(capability, &incomplete),
                Some(false),
                "{api}/{capability} missing {scope}"
            );
            assert_eq!(
                cgs.oauth_relation_satisfied(relation, &incomplete),
                Some(false),
                "{api}/{relation} missing {scope}"
            );
        }
    }
    let slack = catalog("slack");
    for (granted, accepted, rejected) in [
        (
            "users:read",
            "workspace_users_query",
            vec!["channel_members_query", "usergroup_members_query"],
        ),
        (
            "groups:read",
            "channel_members_query",
            vec!["workspace_users_query", "usergroup_members_query"],
        ),
        (
            "usergroups:read",
            "usergroup_members_query",
            vec!["workspace_users_query", "channel_members_query"],
        ),
    ] {
        let grants = HashSet::from([granted.to_string()]);
        assert_eq!(
            slack.oauth_capability_satisfied(accepted, &grants),
            Some(true)
        );
        for capability in rejected {
            assert_eq!(
                slack.oauth_capability_satisfied(capability, &grants),
                Some(false)
            );
        }
    }
    let jira = catalog("jira");
    assert_eq!(
        jira.oauth_capability_satisfied("user_myself", &HashSet::from(["read:jira-user".into()])),
        Some(true)
    );
    assert_eq!(
        jira.oauth_capability_satisfied("user_myself", &HashSet::from(["read:jira-work".into()])),
        Some(false)
    );
}

#[test]
fn slack_membership_navigation_is_a_typed_foreign_key_not_a_duplicate_relation() {
    let cgs = catalog("slack");
    assert_eq!(
        cgs.entities["User"].primary_read.as_deref(),
        Some("user_info")
    );
    for (entity, capability) in [
        ("ChannelMember", "channel_members_query"),
        ("UserGroupMember", "usergroup_members_query"),
    ] {
        let member = &cgs.entities[entity];
        assert!(!member.relations.contains_key("user"));
        let user = &member.fields["user"];
        assert!(user.required);
        assert_eq!(
            user.wire_path.as_deref(),
            Some(["id".to_string()].as_slice())
        );
        match &user.named_value(&cgs).unwrap().field_type {
            FieldType::EntityRef { target, .. } => assert_eq!(target.as_str(), "User"),
            other => panic!("{entity}.user must navigate to User, got {other:?}"),
        }
        assert!(cgs.capabilities[capability]
            .provides
            .iter()
            .any(|field| field == "user"));
        let navigation = format!("{entity}.user");
        assert_eq!(
            cgs.oauth_relation_satisfied(&navigation, &HashSet::from(["users:read".into()])),
            None
        );
        assert_eq!(
            cgs.oauth_capability_satisfied("user_info", &HashSet::from(["users:read".into()])),
            Some(true)
        );
        assert_eq!(
            cgs.oauth_capability_satisfied("user_info", &HashSet::from(["channels:read".into()])),
            Some(false)
        );
    }
}

#[test]
fn native_paging_and_preprocess_contracts_are_separate() {
    assert!(template("twitter", "tweet_lookup_query")
        .pagination
        .is_none());
    assert!(template("slack", "usergroup_members_query")
        .pagination
        .is_none());
    for (api, capability, size_key, size, cursor_key, cursor_path, prefix) in [
        (
            "twitter",
            "user_posts_query",
            "max_results",
            25,
            "pagination_token",
            "next_token",
            vec!["meta"],
        ),
        (
            "twitter",
            "user_mentions_query",
            "max_results",
            25,
            "pagination_token",
            "next_token",
            vec!["meta"],
        ),
        (
            "twitter",
            "user_liked_posts_query",
            "max_results",
            25,
            "pagination_token",
            "next_token",
            vec!["meta"],
        ),
        (
            "twitter",
            "list_posts_query",
            "max_results",
            25,
            "pagination_token",
            "next_token",
            vec!["meta"],
        ),
        (
            "slack",
            "workspace_users_query",
            "limit",
            200,
            "cursor",
            "next_cursor",
            vec!["response_metadata"],
        ),
        (
            "slack",
            "channel_members_query",
            "limit",
            200,
            "cursor",
            "next_cursor",
            vec!["response_metadata"],
        ),
        (
            "slack",
            "channel_history",
            "limit",
            100,
            "cursor",
            "response_metadata.next_cursor",
            vec![],
        ),
        (
            "slack",
            "thread_messages_query",
            "limit",
            100,
            "cursor",
            "response_metadata.next_cursor",
            vec![],
        ),
    ] {
        let paging = template(api, capability).pagination.unwrap();
        assert_eq!(
            serde_json::to_value(paging.location).unwrap(),
            json!("query")
        );
        assert_eq!(paging.response_prefix.unwrap_or_default(), prefix);
        match &paging.params[size_key] {
            PaginationParam::Fixed { fixed, role } => {
                assert_eq!(*fixed, json!(size));
                assert_eq!(*role, Some(PaginationParamRole::PageSize));
            }
            other => panic!("{api}/{capability}: wrong page-size contract {other:?}"),
        }
        match &paging.params[cursor_key] {
            PaginationParam::FromResponse { from_response } => {
                assert_eq!(from_response, cursor_path)
            }
            other => panic!("{api}/{capability}: wrong cursor contract {other:?}"),
        }
    }
    for (capability, items) in [
        ("channel_members_query", "members"),
        ("usergroup_members_query", "users"),
    ] {
        let response = template("slack", capability).response.unwrap();
        assert_eq!(response.items.as_deref(), Some(items));
        assert_eq!(
            response.response_preprocess,
            Some(ResponsePreprocess::StringIdsToFieldObjects {
                path: vec![items.into()],
                field: "id".into(),
            })
        );
        assert!(!response.single);
    }
    let directory = template("slack", "workspace_users_query").response.unwrap();
    assert_eq!(directory.items.as_deref(), Some("members"));
    assert!(directory.response_preprocess.is_none());
    let myself = template("jira", "user_myself").response.unwrap();
    assert!(myself.single);
    assert_eq!(
        catalog("jira").entities["User"].primary_read.as_deref(),
        Some("user_get")
    );
    let watchers = template("jira", "issue_watcher_query").response.unwrap();
    assert!(!watchers.single);
    assert_eq!(watchers.items.as_deref(), Some("watchers"));
}

#[tokio::test]
async fn slack_native_directory_and_memberships_decode_without_invented_profiles() {
    let app = axum::Router::new()
        .route("/users.list", axum::routing::get(|| async {
            axum::Json(json!({"ok":true,"members":[{"id":"U1","name":"alice"}],"response_metadata":{"next_cursor":""}}))
        }))
        .route("/users.info", axum::routing::get(
            |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                axum::Json(json!({"ok":true,"user":{"id":query["user"],"name":"observed profile"}}))
            },
        ))
        .route("/conversations.members", axum::routing::get(|| async {
            axum::Json(json!({"ok":true,"members":["U1","U2"],"response_metadata":{"next_cursor":""}}))
        }))
        .route("/usergroups.users.list", axum::routing::get(
            |axum::extract::Query(query): axum::extract::Query<std::collections::HashMap<String, String>>| async move {
                // This native endpoint is not paginated: runtime must not inject paging controls.
                assert!(!query.contains_key("cursor"));
                assert!(!query.contains_key("limit"));
                let users = if query["usergroup"] == "S0" { json!([]) } else { json!(["U2","U1"]) };
                axum::Json(json!({"ok":true,"users":users}))
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cgs = catalog("slack");
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    for (entity, capability, predicate, expected) in [
        ("User", "workspace_users_query", None, vec!["U1"]),
        (
            "ChannelMember",
            "channel_members_query",
            Some(("channel", "C1")),
            vec!["U1", "U2"],
        ),
        (
            "UserGroupMember",
            "usergroup_members_query",
            Some(("usergroup", "S1")),
            vec!["U2", "U1"],
        ),
        (
            "UserGroupMember",
            "usergroup_members_query",
            Some(("usergroup", "S0")),
            vec![],
        ),
    ] {
        let query = match predicate {
            Some((field, value)) => {
                QueryExpr::filtered(entity, Predicate::eq(field, Value::String(value.into())))
            }
            None => QueryExpr::all(entity),
        }
        .with_capability(capability);
        let result = engine
            .execute(
                &Expr::Query(query.clone()),
                &cgs,
                &mut SessionMaterialization::new(),
                None,
                StreamConsumeOpts {
                    fetch_all: true,
                    ..Default::default()
                },
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        let ids: Vec<_> = result
            .entities()
            .iter()
            .map(|row| row.fields["id"].to_value())
            .collect();
        assert_eq!(
            ids,
            expected
                .iter()
                .map(|id| Value::String((*id).into()))
                .collect::<Vec<_>>()
        );
        for row in result.entities() {
            if entity == "User" {
                assert_eq!(row.fields["name"].to_value(), Value::String("alice".into()));
            } else {
                assert!(!row.fields.contains_key("name"));
                assert!(
                    row.fields.contains_key("user"),
                    "{entity}: observed fields {:?}",
                    row.fields.keys().collect::<Vec<_>>()
                );
                assert_eq!(row.fields["user"].to_value(), row.fields["id"].to_value());
            }
        }
        if entity != "User" && !expected.is_empty() {
            // Storage captures decoded scalar values; the CGS owns the target
            // type. Exercise actual FK navigation instead of assuming an
            // internal TypedFieldValue variant at this storage boundary.
            let users = engine
                .execute(
                    &Expr::Chain(plasm_core::ChainExpr::auto_get(Expr::Query(query), "user")),
                    &cgs,
                    &mut SessionMaterialization::new(),
                    None,
                    StreamConsumeOpts {
                        fetch_all: true,
                        ..Default::default()
                    },
                    ExecuteOptions::for_catalog(&cgs).unwrap(),
                )
                .await
                .unwrap();
            assert_eq!(users.entities().len(), expected.len());
            for user in users.entities() {
                assert_eq!(user.reference.entity_type, "User");
                assert_eq!(
                    user.fields["name"].to_value(),
                    Value::String("observed profile".into())
                );
            }
        }
    }
    server.abort();
}

#[tokio::test]
async fn native_cursor_readers_advance_but_twitter_lookup_does_not_page() {
    use std::{
        collections::HashMap,
        sync::{Arc, Mutex},
    };
    type Requests = Arc<Mutex<Vec<(String, HashMap<String, String>)>>>;
    async fn respond(
        axum::extract::State(requests): axum::extract::State<Requests>,
        uri: axum::http::Uri,
        axum::extract::Query(query): axum::extract::Query<HashMap<String, String>>,
    ) -> axum::Json<serde_json::Value> {
        let path = uri.path().to_string();
        requests.lock().unwrap().push((path.clone(), query.clone()));
        if path == "/2/tweets" {
            return axum::Json(json!({"data":[{"id":"1","text":"lookup"}]}));
        }
        let twitter = path.starts_with("/2/");
        let second = query.contains_key(if twitter {
            "pagination_token"
        } else {
            "cursor"
        });
        let page_size = if twitter { 25 } else { 200 };
        let range = if second {
            page_size..page_size + 1
        } else {
            0..page_size
        };
        let rows: Vec<_> = range
            .map(|id| {
                if path == "/conversations.members" {
                    json!(format!("U{id}"))
                } else if twitter {
                    json!({"id":id.to_string(),"text":"post"})
                } else {
                    json!({"id":format!("U{id}"),"name":format!("user{id}")})
                }
            })
            .collect();
        axum::Json(if twitter {
            if second {
                json!({"data":rows,"meta":{"result_count":1}})
            } else {
                json!({"data":rows,"meta":{"result_count":25,"next_token":"next"}})
            }
        } else {
            json!({"ok":true,"members":rows,"response_metadata":{"next_cursor":if second { "" } else { "next" }}})
        })
    }
    let requests: Requests = Arc::new(Mutex::new(Vec::new()));
    let mut app = axum::Router::new();
    for path in [
        "/2/tweets",
        "/2/users/{user}/tweets",
        "/2/users/{user}/mentions",
        "/2/users/{user}/liked_tweets",
        "/2/lists/{list}/tweets",
        "/users.list",
        "/conversations.members",
    ] {
        app = app.route(path, axum::routing::get(respond));
    }
    let app = app.with_state(requests.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    for (api, entity, capability, scope, path, size) in [
        (
            "twitter",
            "AuthoredPost",
            "user_posts_query",
            Some(("user", "7")),
            "/2/users/7/tweets",
            25,
        ),
        (
            "twitter",
            "MentionedPost",
            "user_mentions_query",
            Some(("user", "7")),
            "/2/users/7/mentions",
            25,
        ),
        (
            "twitter",
            "LikedPost",
            "user_liked_posts_query",
            Some(("user", "7")),
            "/2/users/7/liked_tweets",
            25,
        ),
        (
            "twitter",
            "ListPost",
            "list_posts_query",
            Some(("list", "8")),
            "/2/lists/8/tweets",
            25,
        ),
        (
            "slack",
            "User",
            "workspace_users_query",
            None,
            "/users.list",
            200,
        ),
        (
            "slack",
            "ChannelMember",
            "channel_members_query",
            Some(("channel", "C1")),
            "/conversations.members",
            200,
        ),
    ] {
        requests.lock().unwrap().clear();
        let cgs = catalog(api);
        let query = match scope {
            Some((field, id)) => {
                QueryExpr::filtered(entity, Predicate::eq(field, Value::String(id.into())))
            }
            None => QueryExpr::all(entity),
        }
        .with_capability(capability);
        let result = engine
            .execute(
                &Expr::Query(query),
                &cgs,
                &mut SessionMaterialization::new(),
                None,
                StreamConsumeOpts {
                    fetch_all: true,
                    ..Default::default()
                },
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        let expected: Vec<_> = (0..=size)
            .map(|id| {
                Value::String(if api == "twitter" {
                    id.to_string()
                } else {
                    format!("U{id}")
                })
            })
            .collect();
        let actual: Vec<_> = result
            .entities()
            .iter()
            .map(|row| row.fields["id"].to_value())
            .collect();
        assert_eq!(
            actual, expected,
            "{capability}: native page order and identity"
        );
        let seen = requests.lock().unwrap().clone();
        assert_eq!(seen.len(), 2, "{capability}: must follow cursor once");
        let (size_key, cursor_key) = if api == "twitter" {
            ("max_results", "pagination_token")
        } else {
            ("limit", "cursor")
        };
        for (actual_path, query) in &seen {
            assert_eq!(actual_path, path);
            assert_eq!(query[size_key], size.to_string());
            if let Some((field, id)) = scope {
                // Twitter context lives in the path; Slack context stays in the query.
                if api == "slack" {
                    assert_eq!(query[field], id);
                }
            }
        }
        assert!(!seen[0].1.contains_key(cursor_key));
        assert_eq!(seen[1].1[cursor_key], "next");
    }
    requests.lock().unwrap().clear();
    let cgs = catalog("twitter");
    let query = QueryExpr::filtered(
        "Tweet",
        Predicate::eq("ids", Value::Array(vec![Value::String("1".into())])),
    )
    .with_capability("tweet_lookup_query");
    let result = engine
        .execute(
            &Expr::Query(query),
            &cgs,
            &mut SessionMaterialization::new(),
            None,
            StreamConsumeOpts {
                fetch_all: true,
                ..Default::default()
            },
            ExecuteOptions::for_catalog(&cgs).unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(result.entities().len(), 1);
    let seen = requests.lock().unwrap().clone();
    assert_eq!(seen.len(), 1);
    assert_eq!(seen[0].0, "/2/tweets");
    assert_eq!(seen[0].1, HashMap::from([("ids".into(), "1".into())]));
    server.abort();
}

#[tokio::test]
async fn jira_self_and_watchers_decode_native_single_plural_and_empty_responses() {
    let app = axum::Router::new()
        .route("/rest/api/3/myself", axum::routing::get(|| async {
            axum::Json(json!({"accountId":"self","displayName":"Current user","active":true}))
        }))
        .route("/rest/api/3/issue/{issue}/watchers", axum::routing::get(
            |axum::extract::Path(issue): axum::extract::Path<String>| async move {
                let watchers = if issue == "TEST-0" { json!([]) } else {
                    json!([{"accountId":"one","displayName":"One","active":true},
                           {"accountId":"two","displayName":"Two","active":false}])
                };
                axum::Json(json!({"isWatching":false,"watchCount":watchers.as_array().unwrap().len(),"watchers":watchers}))
            },
        ));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let cgs = catalog("jira");
    let engine = ExecutionEngine::new(ExecutionConfig {
        base_url: Some(base_url),
        hydrate: false,
        ..Default::default()
    })
    .unwrap();
    for (expr, expected) in [
        (
            Expr::Get(GetExpr::pathless_nullary("User").with_capability("user_myself")),
            vec!["self"],
        ),
        (
            Expr::Query(
                QueryExpr::filtered(
                    "User",
                    Predicate::eq("issueIdOrKey", Value::String("TEST-1".into())),
                )
                .with_capability("issue_watcher_query"),
            ),
            vec!["one", "two"],
        ),
        (
            Expr::Query(
                QueryExpr::filtered(
                    "User",
                    Predicate::eq("issueIdOrKey", Value::String("TEST-0".into())),
                )
                .with_capability("issue_watcher_query"),
            ),
            vec![],
        ),
    ] {
        let result = engine
            .execute(
                &expr,
                &cgs,
                &mut SessionMaterialization::new(),
                None,
                StreamConsumeOpts::default(),
                ExecuteOptions::for_catalog(&cgs).unwrap(),
            )
            .await
            .unwrap();
        let ids: Vec<_> = result
            .entities()
            .iter()
            .map(|row| row.fields["accountId"].to_value())
            .collect();
        assert_eq!(
            ids,
            expected
                .into_iter()
                .map(|id| Value::String(id.into()))
                .collect::<Vec<_>>()
        );
    }
    server.abort();
}
