//! Span-graph contracts for `plasm-otel` HTTP MakeSpan + capture helper.

#![cfg(test)]

use crate::span_capture::{find_span, is_child_of, with_captured_spans};
use crate::tower_http_trace_parent_span;
use axum::http::Request;

#[test]
fn http_request_span_name_and_semantic_fields() {
    let req = Request::builder()
        .method("POST")
        .uri("/execute/abc/session-1")
        .body(())
        .unwrap();
    let (_, spans) = with_captured_spans(|| {
        let span = tower_http_trace_parent_span(&req);
        let _g = span.entered();
    });
    let http = find_span(&spans, "plasm_agent.http.request").expect("http request span");
    assert!(
        http.attributes
            .iter()
            .any(|kv| kv.key.as_str() == "http.method"),
        "expected http.method attribute, got {:?}",
        http.attributes
    );
    assert!(
        http.attributes
            .iter()
            .any(|kv| kv.key.as_str() == "http.route"),
        "expected http.route attribute, got {:?}",
        http.attributes
    );
}

#[test]
fn capture_helper_records_parent_child() {
    let (_, spans) = with_captured_spans(|| {
        let parent = tracing::info_span!("test.parent");
        let _g = parent.enter();
        let child = tracing::info_span!("test.child");
        let _c = child.enter();
    });
    let parent = find_span(&spans, "test.parent").expect("parent");
    let child = find_span(&spans, "test.child").expect("child");
    assert!(is_child_of(child, parent));
}
