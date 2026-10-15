// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! Connections that open and then stop talking: at the TLS handshake, before
//! the request headers, and part-way through a body. The server serves a
//! bounded number of connections at once, and each of these gives its place
//! back within the stall bound, so a phone that arrives behind a full set of
//! them is served.
mod seam;

use std::sync::Arc;
use std::time::{Duration, Instant};

use seam::*;
use tauri_app_core::commands::sync::sync_pairing_begin_impl;
use tauri_app_core::now_unix;
use tokio::io::AsyncWriteExt;

/// The number of connections the server serves at once.
const MAX_CONNECTIONS: usize = 16;

/// How long a probe waits for the server to take its place back: the
/// server's stall bound with room for a loaded machine.
const RECOVERY: Duration = Duration::from_secs(20);

/// A client that accepts only `pin`, presenting no certificate.
fn pinned_client(pin: &[u8]) -> tokio_rustls::TlsConnector {
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(PinnedServer { cert_der: pin.to_vec() }))
        .with_no_client_auth();
    tokio_rustls::TlsConnector::from(Arc::new(config))
}

async fn tls(port: u16, pin: &[u8]) -> tokio_rustls::client::TlsStream<tokio::net::TcpStream> {
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let name = rustls::pki_types::ServerName::try_from("daisy.local").unwrap();
    pinned_client(pin).connect(name, tcp).await.expect("TLS handshake")
}

/// One unauthenticated request, the status it got or why it got none.
async fn probe(port: u16, pin: &[u8]) -> Result<u16, String> {
    let attempt = async {
        let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.map_err(|e| e.to_string())?;
        let name = rustls::pki_types::ServerName::try_from("daisy.local").unwrap();
        let tls = pinned_client(pin).connect(name, tcp).await.map_err(|e| format!("TLS: {e}"))?;
        let (mut sender, conn) =
            hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls)).await.map_err(|e| e.to_string())?;
        tokio::spawn(async move {
            let _ = conn.await;
        });
        let req = hyper::Request::builder()
            .method("POST")
            .uri("/hello")
            .body(http_body_util::Full::new(hyper::body::Bytes::new()))
            .unwrap();
        let resp = sender.send_request(req).await.map_err(|e| e.to_string())?;
        Ok(resp.status().as_u16())
    };
    tokio::time::timeout(Duration::from_secs(3), attempt).await.unwrap_or_else(|_| Err("no reply in 3 s".into()))
}

/// Fills every place with connections `open` makes, shows the next caller is
/// turned away, then measures how long until a caller is served again.
async fn stalled_connections_give_their_place_back<F, Fut, T>(open: F)
where
    F: Fn(u16, Vec<u8>) -> Fut,
    Fut: std::future::Future<Output = T>,
{
    let (states, _td) = unlocked();
    sync_pairing_begin_impl(&states.app, &states.vault, "studio-mac.local".into(), now_unix()).unwrap();
    let pin = desktop_certificate(&states);
    let port = start(states.clone()).await;

    let mut held = Vec::new();
    for _ in 0..MAX_CONNECTIONS {
        held.push(open(port, pin.clone()).await);
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let filled = Instant::now();
    assert!(probe(port, &pin).await.is_err(), "every place is taken, so the next caller is turned away");

    let mut last = String::new();
    while filled.elapsed() < RECOVERY {
        match probe(port, &pin).await {
            Ok(status) => {
                assert_eq!(status, 401, "an unpaired caller is answered, and refused");
                eprintln!("served again {:.1} s after the places filled", filled.elapsed().as_secs_f64());
                drop(held);
                return;
            }
            Err(e) => last = e,
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    panic!("still turned away {} s after the places filled: {last}", RECOVERY.as_secs());
}

#[test]
fn connections_silent_at_the_tls_handshake_give_their_place_back() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(stalled_connections_give_their_place_back(|port, _pin| async move {
        tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap()
    }));
}

#[test]
fn connections_silent_before_their_headers_give_their_place_back() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(stalled_connections_give_their_place_back(|port, pin| async move { tls(port, &pin).await }));
}

#[test]
fn connections_silent_part_way_through_a_body_give_their_place_back() {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().unwrap();
    rt.block_on(stalled_connections_give_their_place_back(|port, pin| async move {
        let mut s = tls(port, &pin).await;
        s.write_all(b"POST /pair/start HTTP/1.1\r\nhost: daisy.local\r\ncontent-length: 1000\r\n\r\n{\"v\":2,").await.unwrap();
        s.flush().await.unwrap();
        s
    }));
}
