use super::*;
use crate::secret_store::MemorySecretStore;
use std::io::{BufRead, BufReader};

struct Fixture {
    process: std::process::Child,
    _directory: tempfile::TempDir,
    address: SocketAddr,
    certificate: reqwest::Certificate,
}
impl Fixture {
    fn start() -> Self {
        let directory = tempfile::Builder::new()
            .prefix("plasm-cimd-tests-")
            .tempdir()
            .unwrap();
        let script =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/support/cimd_https.py");
        let mut process = std::process::Command::new("python3")
            .arg(script)
            .arg(directory.path())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("start real HTTPS fixture");
        let mut line = String::new();
        BufReader::new(process.stdout.take().unwrap())
            .read_line(&mut line)
            .unwrap();
        let settings: serde_json::Value =
            serde_json::from_str(&line).expect("fixture TLS settings");
        Self {
            process,
            _directory: directory,
            address: SocketAddr::from(([127, 0, 0, 1], settings["port"].as_u64().unwrap() as u16)),
            certificate: reqwest::Certificate::from_pem(
                settings["certificate"].as_str().unwrap().as_bytes(),
            )
            .unwrap(),
        }
    }
    fn loader(&self) -> MetadataLoader {
        MetadataLoader {
            fixture: Some((self.address, self.certificate.clone())),
            ..Default::default()
        }
    }
    fn id(&self, path: &str) -> String {
        format!("https://client.invalid:{}{path}", self.address.port())
    }
    async fn counts(&self) -> serde_json::Value {
        reqwest::Client::builder()
            .no_proxy()
            .add_root_certificate(self.certificate.clone())
            .resolve_to_addrs("client.invalid", &[self.address])
            .build()
            .unwrap()
            .get(self.id("/counts"))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap()
    }
}
impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

#[tokio::test]
async fn real_https_fetch_pins_dns_and_caches_only_valid_documents() {
    let fixture = Fixture::start();
    let loader = fixture.loader();
    let storage = MemorySecretStore::new();
    let id = fixture.id("/valid");
    assert_eq!(loader.load(&storage, &id).await.unwrap().client_id, id);
    assert_eq!(loader.load(&storage, &id).await.unwrap().client_id, id);
    assert_eq!(fixture.counts().await["/valid"], 1);
    for path in [
        "/redirect",
        "/missing",
        "/html",
        "/mismatch",
        "/large",
        "/stream-large",
    ] {
        assert!(
            loader.load(&storage, &fixture.id(path)).await.is_err(),
            "accepted {path}"
        );
        assert!(loader.load(&storage, &fixture.id(path)).await.is_err());
        assert_eq!(
            fixture.counts().await[path],
            2,
            "invalid metadata was cached"
        );
    }
    // Following a redirect would have increased this count.
    assert_eq!(fixture.counts().await["/valid"], 1);
}

#[tokio::test]
async fn retrieval_obeys_cache_control_expiry_and_response_deadline() {
    let fixture = Fixture::start();
    let mut loader = fixture.loader();
    let storage = MemorySecretStore::new();
    for path in ["/no-store", "/max-age-zero"] {
        for _ in 0..2 {
            loader.load(&storage, &fixture.id(path)).await.unwrap();
        }
        assert_eq!(fixture.counts().await[path], 2);
    }
    loader.cache_ttl = Duration::from_millis(20);
    loader
        .load(&storage, &fixture.id("/expires"))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(30)).await;
    loader
        .load(&storage, &fixture.id("/expires"))
        .await
        .unwrap();
    assert_eq!(fixture.counts().await["/expires"], 2);
    loader.request_timeout = Duration::from_millis(30);
    assert!(loader.load(&storage, &fixture.id("/slow")).await.is_err());
}

#[tokio::test]
async fn timed_out_dns_retains_capacity_until_resolver_finishes() {
    let fixture = Fixture::start();
    let mut loader = fixture.loader();
    loader.slots = Arc::new(tokio::sync::Semaphore::new(1));
    loader.lookup_timeout = Duration::from_millis(20);
    let (release, resolver) = std::sync::mpsc::channel();
    loader.lookup_release = Some(Arc::new(std::sync::Mutex::new(resolver)));
    let storage = MemorySecretStore::new();
    assert!(loader.load(&storage, &fixture.id("/valid")).await.is_err());
    assert_eq!(loader.slots.available_permits(), 0);
    assert!(matches!(
        loader.load(&storage, &fixture.id("/valid")).await,
        Err(McpOAuthError::MetadataRetrieval { source }) if source.is_capacity_failure()
    ));
    release.send(()).unwrap();
    let permit = tokio::time::timeout(Duration::from_secs(5), loader.slots.acquire())
        .await
        .expect("resolver must release its capacity")
        .unwrap();
    drop(permit);
    assert_eq!(loader.slots.available_permits(), 1);
    assert!(fixture.counts().await.get("/valid").is_none());
}

#[test]
fn cache_lifetime_honours_age_and_explicit_prohibitions() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert(
        reqwest::header::CACHE_CONTROL,
        "public, max-age=60".parse().unwrap(),
    );
    headers.insert(reqwest::header::AGE, "55".parse().unwrap());
    assert_eq!(
        cache_duration(&headers, Duration::from_secs(300)),
        Duration::from_secs(5)
    );
    for directive in ["no-store", "no-cache", "max-age=0", "max-age=invalid"] {
        headers.insert(reqwest::header::CACHE_CONTROL, directive.parse().unwrap());
        assert_eq!(
            cache_duration(&headers, Duration::from_secs(300)),
            Duration::ZERO
        );
    }
}

#[tokio::test]
async fn untrusted_tls_and_private_dns_are_rejected_before_metadata_acceptance() {
    let fixture = Fixture::start();
    let unrelated = Fixture::start();
    let mut loader = fixture.loader();
    loader.fixture = Some((fixture.address, unrelated.certificate.clone()));
    let storage = MemorySecretStore::new();
    assert!(loader.load(&storage, &fixture.id("/valid")).await.is_err());
    assert!(fixture.counts().await.get("/valid").is_none());
    let production = MetadataLoader::default();
    assert!(production
        .load(
            &storage,
            &format!("https://127.0.0.1:{}/valid", fixture.address.port())
        )
        .await
        .is_err());
    assert!(fixture.counts().await.get("/valid").is_none());
    assert!(!loader.allowed_address(SocketAddr::from((
        [127, 0, 0, 1],
        fixture.address.port().saturating_add(1)
    ))));
    for id in [
        "http://client.invalid/document",
        "https://client.invalid/",
        "https://user:secret@client.invalid/document",
        "https://client.invalid/document#fragment",
    ] {
        assert!(production.load(&storage, id).await.is_err());
    }
}

#[tokio::test]
async fn mixed_private_empty_and_excessive_dns_answers_never_connect() {
    let fixture = Fixture::start();
    let mut loader = fixture.loader();
    let storage = MemorySecretStore::new();
    for addresses in [
        vec![],
        vec![fixture.address; 17],
        vec![
            fixture.address,
            SocketAddr::from(([169, 254, 169, 254], 443)),
        ],
    ] {
        loader.resolved_addresses = Some(addresses);
        assert!(loader.load(&storage, &fixture.id("/valid")).await.is_err());
    }
    assert!(fixture.counts().await.get("/valid").is_none());
}

#[tokio::test]
async fn followup_cimd_timeout_preserves_typed_network_cause() {
    let fixture = Fixture::start();
    let mut loader = fixture.loader();
    loader.request_timeout = Duration::from_millis(100);
    let error = loader
        .load(&MemorySecretStore::new(), &fixture.id("/slow"))
        .await
        .err()
        .unwrap();
    assert_eq!(fixture.counts().await["/slow"], 1);
    let cause = std::error::Error::source(&error).unwrap().source().unwrap();
    assert!(cause.downcast_ref::<reqwest::Error>().unwrap().is_timeout());
    assert_eq!(error.oauth_error_code(), "invalid_client_metadata");
    assert_eq!(error.description(), "client metadata retrieval failed");
}

#[tokio::test]
async fn dns_failure_preserves_io_cause_and_releases_capacity() {
    let fixture = Fixture::start();
    let mut loader = fixture.loader();
    loader.resolver_error = Some(std::io::ErrorKind::NotFound);
    let error = loader
        .load(&MemorySecretStore::new(), &fixture.id("/valid"))
        .await
        .err()
        .unwrap();
    let cause = std::error::Error::source(&error).unwrap().source().unwrap();
    assert_eq!(
        cause.downcast_ref::<std::io::Error>().unwrap().kind(),
        std::io::ErrorKind::NotFound
    );
    assert_eq!(error.oauth_error_code(), "invalid_client_metadata");
    assert_eq!(loader.slots.available_permits(), 16);
    assert!(fixture.counts().await.get("/valid").is_none());
}
