use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::Arc;

use axum::{Router, routing::get};
use axum_server::Handle;

use super::server::serve_tls;
use crate::health::Readiness;

/// Writes a self-signed certificate for `localhost` and its PKCS#8 key as PEM
/// files into a directory unique to `name`.
fn write_self_signed_pem(name: &str) -> (PathBuf, PathBuf, PathBuf) {
    let certified = rcgen::generate_simple_self_signed(vec!["localhost".to_string()]).unwrap();
    let dir = std::env::temp_dir().join(format!("epa-{name}-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let cert_path = dir.join("tls.crt");
    let key_path = dir.join("tls.key");
    std::fs::write(&cert_path, certified.cert.pem()).unwrap();
    std::fs::write(&key_path, certified.signing_key.serialize_pem()).unwrap();
    (dir, cert_path, key_path)
}

#[tokio::test]
async fn test_serve_tls_serves_https() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let (dir, cert_path, key_path) = write_self_signed_pem("serve-tls");

    let app = Router::new().route("/", get(|| async { "ok" }));
    let handle = Handle::new();
    let readiness = Arc::new(Readiness::default());
    let server = tokio::spawn({
        let handle = handle.clone();
        let readiness = readiness.clone();
        async move {
            let addr: SocketAddr = "127.0.0.1:0".parse().unwrap();
            serve_tls(addr, &cert_path, &key_path, app, handle, &readiness).await
        }
    });

    let addr = handle.listening().await.expect("server did not bind");
    assert!(!readiness.pending().contains(&"tls"));
    let body = reqwest::Client::builder()
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap()
        .get(format!("https://localhost:{}/", addr.port()))
        .send()
        .await
        .unwrap()
        .text()
        .await
        .unwrap();
    assert_eq!(body, "ok");

    handle.shutdown();
    server.await.unwrap().unwrap();
    std::fs::remove_dir_all(dir).unwrap();
}

#[tokio::test]
async fn test_serve_tls_missing_certificate() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let dir = std::env::temp_dir().join(format!("epa-serve-tls-missing-{}", std::process::id()));

    let readiness = Readiness::default();
    let err = serve_tls(
        "127.0.0.1:0".parse().unwrap(),
        &dir.join("tls.crt"),
        &dir.join("tls.key"),
        Router::new(),
        Handle::new(),
        &readiness,
    )
    .await
    .unwrap_err();

    assert!(
        err.to_string()
            .contains("Failed to load TLS certificates for Webhook server"),
        "unexpected error: {err:#}"
    );
    assert!(readiness.pending().contains(&"tls"));
}
