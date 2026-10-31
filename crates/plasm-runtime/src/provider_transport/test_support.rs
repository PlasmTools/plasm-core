//! Minimal HTTP peer for transport proofs; no third-party test server dependency.
use std::{future::Future, sync::Arc};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub(super) async fn spawn_server<F, Fut>(handler: F) -> (String, tokio::task::JoinHandle<()>)
where
    F: Fn(String) -> Fut + Send + Sync + 'static,
    Fut: Future<Output = (u16, String)> + Send + 'static,
{
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/", listener.local_addr().unwrap());
    let handler = Arc::new(handler);
    let server = tokio::spawn(async move {
        let mut connections = tokio::task::JoinSet::new();
        loop {
            let (mut stream, _) = listener.accept().await.unwrap();
            let handler = handler.clone();
            connections.spawn(async move {
                let mut bytes = Vec::new();
                let (offset, length) = loop {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    if count == 0 { return; }
                    bytes.extend_from_slice(&chunk[..count]);
                    if let Some(end) = bytes.windows(4).position(|w| w == b"\r\n\r\n") {
                        let headers = String::from_utf8_lossy(&bytes[..end]);
                        let length = headers.lines().find_map(|l| l.to_lowercase().strip_prefix("content-length:").map(|v| v.trim().parse::<usize>().unwrap())).unwrap();
                        break (end + 4, length);
                    }
                };
                while bytes.len() < offset + length {
                    let mut chunk = [0; 4096];
                    let count = stream.read(&mut chunk).await.unwrap();
                    if count == 0 { return; }
                    bytes.extend_from_slice(&chunk[..count]);
                }
                let (status, body) = handler(String::from_utf8(bytes[offset..offset+length].to_vec()).unwrap()).await;
                let response = format!("HTTP/1.1 {status} Response\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
                let _ = stream.write_all(response.as_bytes()).await;
            });
        }
    });
    (endpoint, server)
}
