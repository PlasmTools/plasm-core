use super::*;
use std::{sync::Mutex, time::Duration};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn peer(
    replies: Vec<(u16, String)>,
) -> (
    EmbeddingClient,
    Arc<Mutex<Vec<String>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/", listener.local_addr().unwrap());
    let bodies = Arc::new(Mutex::new(Vec::new()));
    let captured = bodies.clone();
    let server = tokio::spawn(async move {
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut bytes = Vec::new();
            let (offset, length) = loop {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                if n == 0 {
                    return;
                }
                bytes.extend_from_slice(&chunk[..n]);
                if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                    let length = String::from_utf8_lossy(&bytes[..end])
                        .lines()
                        .find_map(|l| {
                            l.to_lowercase()
                                .strip_prefix("content-length:")
                                .map(|v| v.trim().parse::<usize>().unwrap())
                        })
                        .unwrap();
                    break (end + 4, length);
                }
            };
            while bytes.len() < offset + length {
                let mut chunk = [0; 4096];
                let n = stream.read(&mut chunk).await.unwrap();
                assert_ne!(n, 0);
                bytes.extend_from_slice(&chunk[..n]);
            }
            let i = {
                let mut seen = captured.lock().unwrap();
                let i = seen.len();
                seen.push(String::from_utf8(bytes[offset..offset + length].to_vec()).unwrap());
                i
            };
            let (status, body) = &replies[i.min(replies.len() - 1)];
            stream.write_all(format!("HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
        }
    });
    let client = EmbeddingClient::new(
        Arc::new(ReqwestProviderTransport::new(
            reqwest::Client::new(),
            url,
            "abstract",
        )),
        ProviderRetryPolicy {
            backoff: Duration::ZERO,
            ..Default::default()
        },
    );
    (client, bodies, server)
}

fn response() -> serde_json::Value {
    let mut vector = vec![0.0; 1536];
    vector[0] = 1.0;
    serde_json::json!({"model":EmbeddingProfile::default().model,
        "data":[{"index":0,"embedding":vector}], "usage":{"prompt_tokens":7,"total_tokens":7}})
}

#[tokio::test]
async fn embeddings_do_not_dispatch_after_slow_dispatch_receipt() {
    let (mut client, bodies, server) = peer(vec![(200, response().to_string())]).await;
    client.policy.attempts = 1;
    client.policy.attempt_timeout = Duration::from_millis(5);
    let error = client
        .embed(&["abstract material".into()], |event| {
            if matches!(
                event,
                ProviderAttempt::Lifecycle {
                    phase: plasm_runtime::provider_transport::AttemptPhase::Dispatch,
                    event: "started",
                    ..
                }
            ) {
                std::thread::sleep(Duration::from_millis(20));
            }
            Ok(())
        })
        .await
        .unwrap_err();
    server.abort();
    assert!(matches!(
        error,
        EmbeddingAcquisitionError::Transport(ProviderTransportError::AttemptTimeout { .. })
    ));
    assert!(bodies.lock().unwrap().is_empty());
}

#[tokio::test]
async fn embeddings_retry_identical_bytes_with_shared_durable_receipts() {
    let (client, bodies, server) =
        peer(vec![(503, "{}".into()), (200, response().to_string())]).await;
    let directory = tempfile::tempdir().unwrap();
    let mut sink = ProviderReceiptSink::new("test_embedding", Some(directory.path().into()));
    let vectors = client
        .embed(&["abstract material".into()], |event| sink.record(event))
        .await
        .unwrap();
    server.abort();
    assert_eq!(vectors.len(), 1);
    assert_eq!(bodies.lock().unwrap().len(), 2);
    {
        let bodies = bodies.lock().unwrap();
        assert_eq!(bodies[0], bodies[1]);
    }
    let receipts: Vec<serde_json::Value> = std::fs::read_dir(directory.path())
        .unwrap()
        .map(|entry| {
            serde_json::from_slice(&std::fs::read(entry.unwrap().path()).unwrap()).unwrap()
        })
        .collect();
    let responses: Vec<_> = receipts
        .iter()
        .filter(|r| r["kind"] == "response")
        .collect();
    assert_eq!(responses.len(), 2);
    assert_eq!(responses[0]["request_hash"], responses[1]["request_hash"]);
    assert_eq!(
        responses[0]["logical_request_id"],
        responses[1]["logical_request_id"]
    );
    let success = responses.iter().find(|r| r["http_status"] == 200).unwrap();
    let raw: serde_json::Value =
        serde_json::from_str(success["raw_response"].as_str().unwrap()).unwrap();
    assert_eq!(raw["usage"]["prompt_tokens"], 7);
    assert!(
        raw["usage"].get("cost").is_none(),
        "missing cost must not be invented"
    );
}

#[tokio::test]
async fn embeddings_stop_at_three_attempts_and_retain_last_failure() {
    let (client, bodies, server) = peer(vec![(503, "{}".into())]).await;
    let error = client
        .embed(&["abstract material".into()], |_| Ok(()))
        .await
        .unwrap_err();
    server.abort();
    assert!(matches!(
        error,
        EmbeddingAcquisitionError::Transport(ProviderTransportError::RetryExhausted {
            attempts: 3,
            ..
        })
    ));
    assert_eq!(bodies.lock().unwrap().len(), 3);
}

#[tokio::test]
async fn embeddings_never_retry_authorization_or_semantic_corruption() {
    let mut wrong_model = response();
    wrong_model["model"] = "foreign".into();
    let mut wrong_count = response();
    wrong_count["data"] = serde_json::json!([]);
    let mut wrong_vector = response();
    wrong_vector["data"][0]["embedding"] = serde_json::json!([1.0]);
    for (status, body) in [
        (401, "{}".into()),
        (403, "{}".into()),
        (200, "malformed".into()),
        (200, wrong_model.to_string()),
        (200, wrong_count.to_string()),
        (200, wrong_vector.to_string()),
    ] {
        let (client, bodies, server) =
            peer(vec![(status, body), (200, response().to_string())]).await;
        assert!(client
            .embed(&["abstract material".into()], |_| Ok(()))
            .await
            .is_err());
        server.abort();
        assert_eq!(bodies.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn embeddings_fail_closed_when_admission_receipts_cannot_be_saved() {
    let (client, bodies, server) = peer(vec![(200, response().to_string())]).await;
    let error = client
        .embed(&["abstract material".into()], |_| {
            Err(ProviderRecordError::Io(std::io::Error::other(
                "abstract disk failure",
            )))
        })
        .await
        .unwrap_err();
    server.abort();
    assert!(matches!(
        error,
        EmbeddingAcquisitionError::Transport(ProviderTransportError::Record(_))
    ));
    assert!(bodies.lock().unwrap().is_empty());
}
