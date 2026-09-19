// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A synthetic phone and the loopback TLS server it talks to, shared by every
//! suite that drives the seam. The phone is synthetic but its bytes are the
//! protocol's — the same client certificate, signatures and headers
//! `docs/sync/protocol.md` sections 3-8 describe, so the mutual-TLS match,
//! the nonce set and the signing string are exercised rather than assumed.
#![allow(dead_code)]
use std::sync::Arc;

use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use tauri_app_core::commands::lifecycle::init_vault_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};
use tauri_app_core::sync::pairing::pair_mac;
use tauri_app_core::sync::server::{serve, ImportProgress, SyncStates};
use tauri_app_core::sync::signing::signing_string;

pub const PASS: &str = "lantern-copper-walrus-orbit-2026";

/// Each test here runs a server, a vault and hundreds of megabytes of file
/// IO. Run together they measure each other rather than the code, and the
/// hello-latency test measures nothing at all.
pub static ONE_AT_A_TIME: std::sync::Mutex<()> = std::sync::Mutex::new(());

pub fn alone() -> std::sync::MutexGuard<'static, ()> {
    ONE_AT_A_TIME.lock().unwrap_or_else(|e| e.into_inner())
}

/// The two states a Tauri app manages, owned by the test instead.
pub struct States {
    pub app: AppState,
    pub vault: VaultState,
    /// What the app would have been told about the library, as (kind, session).
    pub library: std::sync::Mutex<Vec<(String, String)>>,
    /// How often the app would have been told its sync status changed.
    pub sync_changes: std::sync::atomic::AtomicUsize,
    /// What the app would have been told about a phone session on its way in.
    pub progress: std::sync::Mutex<Vec<ProgressRow>>,
}

/// One `sync:import` the app would have emitted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgressRow {
    pub stage: String,
    pub session: String,
    pub device_name: String,
    pub received: Option<usize>,
    pub total: Option<usize>,
    pub title: Option<String>,
}

impl SyncStates for States {
    fn app(&self) -> &AppState {
        &self.app
    }
    fn vault(&self) -> &VaultState {
        &self.vault
    }
    fn library_changed(&self, kind: tauri_app_core::library_events::LibraryChangeKind, session_id: &str) {
        self.library.lock().unwrap().push((format!("{kind:?}"), session_id.to_string()));
    }
    fn sync_changed(&self) {
        self.sync_changes.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
    }
    fn import_progress(&self, ev: ImportProgress) {
        self.progress.lock().unwrap().push(ProgressRow {
            stage: format!("{:?}", ev.stage),
            session: ev.session.to_string(),
            device_name: ev.device_name.to_string(),
            received: ev.received,
            total: ev.total,
            title: ev.title.clone(),
        });
    }
}

impl States {
    pub fn library_events(&self) -> Vec<(String, String)> {
        self.library.lock().unwrap().clone()
    }

    pub fn sync_changes(&self) -> usize {
        self.sync_changes.load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn progress_events(&self) -> Vec<ProgressRow> {
        self.progress.lock().unwrap().clone()
    }

    /// The stages reported for one session, in the order they were reported.
    pub fn progress_stages(&self, session: &str) -> Vec<String> {
        self.progress_events().into_iter().filter(|r| r.session == session).map(|r| r.stage).collect()
    }
}

pub fn unlocked() -> (Arc<States>, tempfile::TempDir) {
    let td = tempfile::tempdir().unwrap();
    let app = AppState::new(ProfileDir::at(td.path().join("daisy")).unwrap());
    let vault = VaultState::new();
    init_vault_impl(&app, &vault, PASS).unwrap();
    (
        Arc::new(States {
            app,
            vault,
            library: std::sync::Mutex::new(Vec::new()),
            sync_changes: std::sync::atomic::AtomicUsize::new(0),
            progress: std::sync::Mutex::new(Vec::new()),
        }),
        td,
    )
}

/// What the phone reads out of the code the desktop shows.
pub struct Code {
    pub secret: [u8; 32],
    pub desktop_pub: [u8; 32],
    pub port: u16,
}

pub fn read_code(text: &str) -> Code {
    let json = B64URL.decode(text.strip_prefix("daisy-pair:1:").unwrap()).unwrap();
    let code: serde_json::Value = serde_json::from_slice(&json).unwrap();
    Code {
        secret: B64.decode(code["secret_b64"].as_str().unwrap()).unwrap().try_into().unwrap(),
        desktop_pub: B64.decode(code["ed25519_pub_b64"].as_str().unwrap()).unwrap().try_into().unwrap(),
        port: code["port"].as_u64().unwrap() as u16,
    }
}

/// The certificate this desktop serves, which is what a phone ends up
/// pinning.
pub fn desktop_certificate(states: &States) -> Vec<u8> {
    let id = tauri_app_core::sync::identity::load_or_create(&states.app, &states.vault).unwrap();
    B64.decode(id.tls_cert_der_b64).unwrap()
}

/// A phone: an Ed25519 identity, a self-signed TLS client certificate, and
/// the device id it signs under.
pub struct Phone {
    pub device_id: String,
    pub key: SigningKey,
    pub tls_cert_der: Vec<u8>,
    pub tls_key_der: Vec<u8>,
}

impl Phone {
    pub fn new() -> Self {
        Self::with("4a68f72f5a22110b2bfbe71b6588833c", 21)
    }

    /// A second phone, or the same phone after a factory reset: its own
    /// device id, signing key and client certificate.
    pub fn with(device_id: &str, key_seed: u8) -> Self {
        let key = rcgen::KeyPair::generate().unwrap();
        let cert = rcgen::CertificateParams::new(vec!["daisy-phone".into()])
            .unwrap()
            .self_signed(&key)
            .unwrap();
        Self {
            device_id: device_id.into(),
            key: SigningKey::from_bytes(&[key_seed; 32]),
            tls_cert_der: cert.der().to_vec(),
            tls_key_der: key.serialize_der(),
        }
    }

    pub fn pub_b64(&self) -> String {
        B64.encode(self.key.verifying_key().to_bytes())
    }

    /// The signed-request headers for one call, under a nonce the caller
    /// chooses so a replay can be sent deliberately.
    pub fn headers(&self, method: &str, path: &str, nonce_b64: &str, body: &[u8]) -> Vec<(String, String)> {
        let s = signing_string(method, path, &self.device_id, nonce_b64, body);
        vec![
            ("X-Daisy-Device".into(), self.device_id.clone()),
            ("X-Daisy-Nonce".into(), nonce_b64.to_string()),
            ("X-Daisy-Signature".into(), B64.encode(self.key.sign(s.as_bytes()).to_bytes())),
        ]
    }
}

/// Accepts the desktop's own certificate and nothing else. A phone pins the
/// SHA-256 of its SubjectPublicKeyInfo; that hash is asserted against this
/// certificate by the phone-side vector suite, so pinning the certificate
/// itself here keeps X.509 parsing out of the test.
#[derive(Debug)]
pub struct PinnedServer {
    pub cert_der: Vec<u8>,
}

impl rustls::client::danger::ServerCertVerifier for PinnedServer {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() == self.cert_der {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General("pin mismatch".into()))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &rustls::crypto::ring::default_provider().signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        rustls::crypto::ring::default_provider().signature_verification_algorithms.supported_schemes()
    }
}

pub struct Reply {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Reply {
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }

    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("a JSON body")
    }
}

/// One request over a fresh TLS connection, the way a phone that woke for
/// ten seconds would make it.
pub async fn request(
    port: u16,
    pin: &[u8],
    phone: Option<&Phone>,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
) -> Reply {
    let verifier = Arc::new(PinnedServer { cert_der: pin.to_vec() });
    let builder = rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .unwrap()
        .dangerous()
        .with_custom_certificate_verifier(verifier);
    let config = match phone {
        Some(p) => builder
            .with_client_auth_cert(
                vec![rustls::pki_types::CertificateDer::from(p.tls_cert_der.clone())],
                rustls::pki_types::PrivateKeyDer::try_from(p.tls_key_der.clone()).unwrap(),
            )
            .unwrap(),
        None => builder.with_no_client_auth(),
    };
    let connector = tokio_rustls::TlsConnector::from(Arc::new(config));
    let tcp = tokio::net::TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let server_name = rustls::pki_types::ServerName::try_from("daisy.local").unwrap();
    let tls = connector.connect(server_name, tcp).await.expect("TLS handshake");

    let (mut sender, conn) = hyper::client::conn::http1::handshake(hyper_util::rt::TokioIo::new(tls)).await.unwrap();
    tokio::spawn(async move {
        let _ = conn.await;
    });
    let mut req = hyper::Request::builder().method(method).uri(path);
    for (k, v) in headers {
        req = req.header(k, v);
    }
    let req = req.body(http_body_util::Full::new(hyper::body::Bytes::from(body))).unwrap();
    let resp = sender.send_request(req).await.expect("a reply");
    let status = resp.status().as_u16();
    let headers = resp
        .headers()
        .iter()
        .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or_default().to_string()))
        .collect();
    let body = resp.into_body().collect().await.unwrap().to_bytes().to_vec();
    Reply { status, headers, body }
}

/// Runs the pairing round, leaving the phone paired.
pub async fn pair(port: u16, server_cert: &[u8], phone: &Phone, code: &Code) {
    let body = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "device_id": phone.device_id,
        "name": "Rowan's phone",
        "platform": "ios",
        "ed25519_pub_b64": phone.pub_b64(),
        "tls_cert_der_b64": B64.encode(&phone.tls_cert_der),
    }))
    .unwrap();
    let mac = B64.encode(pair_mac(&code.secret, &body));
    let reply = request(
        port,
        server_cert,
        Some(phone),
        "POST",
        "/pair",
        vec![("X-Daisy-Pair-Mac".into(), mac)],
        body,
    )
    .await;
    assert_eq!(reply.status, 200, "pairing accepted");
}

/// Starts the server on an ephemeral port and returns it.
pub async fn start(states: Arc<States>) -> u16 {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        serve(listener, states, std::future::pending()).await;
    });
    port
}
