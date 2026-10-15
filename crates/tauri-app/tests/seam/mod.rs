// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! A synthetic phone and the loopback TLS server it talks to, shared by every
//! suite that drives the seam. The phone is synthetic but its bytes are the
//! protocol's — the same client certificate, signatures and headers
//! the protocol describes, so the mutual-TLS match, the nonce set and the
//! signing string are exercised.
#![allow(dead_code)]
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use http_body_util::BodyExt;
use rand::RngCore;
use sha2::{Digest, Sha256};
use tauri_app_core::commands::lifecycle::init_vault_impl;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};
use tauri_app_core::sync::pairing::{confirm, desktop_claim, pair_mac, parse_key, phone_proof, transcript, PhoneClaim};
use tauri_app_core::sync::server::{serve, ImportProgress, SyncStates};
use tauri_app_core::sync::signing::signing_string;
use x25519_dalek::{PublicKey, StaticSecret};

pub const PASS: &str = "lantern-copper-walrus-orbit-2026";

/// Serializes the tests in this module: each runs a server, a vault and hundreds of megabytes of file IO.
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
    /// Each version-gate refusal the app would have been told of, as
    /// (device, the side out of date).
    pub gated: std::sync::Mutex<Vec<(String, String)>>,
    /// How long telling the app its sync status changed takes, in
    /// milliseconds: 0 returns at once.
    pub sync_changed_takes_ms: std::sync::atomic::AtomicU64,
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
        let takes = self.sync_changed_takes_ms.load(std::sync::atomic::Ordering::SeqCst);
        std::thread::sleep(std::time::Duration::from_millis(takes));
    }
    fn sync_gated(&self, device_id: &str, _platform: &str, outdated: tauri_app_core::sync::gate::Outdated) {
        self.gated.lock().unwrap().push((device_id.to_string(), format!("{outdated:?}")));
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
            gated: std::sync::Mutex::new(Vec::new()),
            progress: std::sync::Mutex::new(Vec::new()),
            sync_changed_takes_ms: std::sync::atomic::AtomicU64::new(0),
        }),
        td,
    )
}

/// The key and where to find the desktop, as a phone reads it off the QR.
#[derive(Clone)]
pub struct Key {
    pub key: [u8; 10],
    pub host: String,
    pub port: u16,
}

/// Parses `daisy-pair:2:<key>@<host>:<port>`, the text the QR carries.
pub fn read_qr(text: &str) -> Key {
    let rest = text.strip_prefix("daisy-pair:2:").expect("a v2 QR");
    let (key_text, addr) = rest.split_once('@').expect("a host after @");
    let (host, port) = addr.rsplit_once(':').expect("a port after the last colon");
    Key { key: parse_key(key_text).expect("a valid key"), host: host.to_string(), port: port.parse().expect("a port") }
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

    /// The 16 bytes behind this phone's hex device id.
    pub fn device_id_bytes(&self) -> [u8; 16] {
        hex16(&self.device_id)
    }

    /// This phone's half of the pairing transcript: its identity and the
    /// SHA-256 of the certificate it presents.
    pub fn claim(&self) -> PhoneClaim {
        PhoneClaim {
            device_id: self.device_id_bytes(),
            ed25519_pub: self.key.verifying_key().to_bytes(),
            cert_sha256: Sha256::digest(&self.tls_cert_der).into(),
        }
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

/// Accepts the desktop's own certificate and nothing else. A phone pins the SHA-256 of its SubjectPublicKeyInfo; this verifier pins the certificate itself.
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
    send(port, pin, phone, method, path, headers, http_body_util::Full::new(hyper::body::Bytes::from(body))).await
}

/// A request that declares a `len`-byte body and sends none of it, so the
/// reply shows what the server decides from the headers alone.
pub async fn request_declaring(
    port: u16,
    pin: &[u8],
    phone: Option<&Phone>,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    len: u64,
) -> Reply {
    send(port, pin, phone, method, path, headers, Held(len)).await
}

/// A body of a declared length whose bytes never arrive.
struct Held(u64);

impl hyper::body::Body for Held {
    type Data = hyper::body::Bytes;
    type Error = std::convert::Infallible;

    fn poll_frame(
        self: std::pin::Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Option<Result<hyper::body::Frame<Self::Data>, Self::Error>>> {
        std::task::Poll::Pending
    }

    fn size_hint(&self) -> hyper::body::SizeHint {
        hyper::body::SizeHint::with_exact(self.0)
    }
}

async fn send<B>(
    port: u16,
    pin: &[u8],
    phone: Option<&Phone>,
    method: &str,
    path: &str,
    headers: Vec<(String, String)>,
    body: B,
) -> Reply
where
    B: hyper::body::Body + Send + Unpin + 'static,
    B::Data: Send,
    B::Error: Into<Box<dyn std::error::Error + Send + Sync>>,
{
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
    let req = req.body(body).unwrap();
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

/// The 16 bytes of a 32-character lowercase hex device id.
fn hex16(hex: &str) -> [u8; 16] {
    let mut out = [0u8; 16];
    for (i, b) in out.iter_mut().enumerate() {
        *b = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).expect("a hex device id");
    }
    out
}

/// The `/pair/start` body a phone sends to prove it holds `key`, claiming
/// `cert_der` as its certificate. A fresh ephemeral key and nonce every call.
pub fn start_body_with_cert(phone: &Phone, key: &[u8; 10], cert_der: &[u8]) -> Vec<u8> {
    let mut rng = rand::rng();
    let mut eph_bytes = [0u8; 32];
    rng.fill_bytes(&mut eph_bytes);
    let eph_pub = PublicKey::from(&StaticSecret::from(eph_bytes)).to_bytes();
    let mut nonce = [0u8; 32];
    rng.fill_bytes(&mut nonce);
    let claim = PhoneClaim { device_id: phone.device_id_bytes(), ed25519_pub: phone.key.verifying_key().to_bytes(), cert_sha256: Sha256::digest(cert_der).into() };
    let cp = phone_proof(key, &claim.bytes(), &eph_pub, &nonce);
    serde_json::to_vec(&serde_json::json!({
        "v": 2,
        "device_id": phone.device_id,
        "ed25519_pub_b64": phone.pub_b64(),
        "tls_cert_der_b64": B64.encode(cert_der),
        "eph_pub_b64": B64.encode(eph_pub),
        "nonce_b64": B64.encode(nonce),
        "cP_b64": B64.encode(cp),
    }))
    .unwrap()
}

/// The `/pair/start` body `phone` sends for its own certificate.
pub fn start_body(phone: &Phone, key: &[u8; 10]) -> Vec<u8> {
    start_body_with_cert(phone, key, &phone.tls_cert_der)
}

/// What `/pair/start` settles: the session a `/pair` names, the secret S both
/// sides now derive, and the desktop's Ed25519 key, proven by `cD`.
pub struct Started {
    pub session_id: String,
    pub secret: [u8; 32],
    pub desktop_ed25519_pub: [u8; 32],
}

/// Runs `/pair/start` for `key` over a connection presenting `phone`'s own
/// certificate, and confirms `cD` the way a real phone would: rebuilding B
/// and T from the reply and checking the MAC before trusting anything in it.
pub async fn pair_start(port: u16, server_cert: &[u8], phone: &Phone, key: &Key) -> Started {
    let mut rng = rand::rng();
    let mut eph_bytes = [0u8; 32];
    rng.fill_bytes(&mut eph_bytes);
    let eph = StaticSecret::from(eph_bytes);
    let eph_pub = PublicKey::from(&eph).to_bytes();
    let mut nonce = [0u8; 32];
    rng.fill_bytes(&mut nonce);

    let claim = phone.claim();
    let cp = phone_proof(&key.key, &claim.bytes(), &eph_pub, &nonce);
    let body = serde_json::to_vec(&serde_json::json!({
        "v": 2,
        "device_id": phone.device_id,
        "ed25519_pub_b64": phone.pub_b64(),
        "tls_cert_der_b64": B64.encode(&phone.tls_cert_der),
        "eph_pub_b64": B64.encode(eph_pub),
        "nonce_b64": B64.encode(nonce),
        "cP_b64": B64.encode(cp),
    }))
    .unwrap();

    let reply = request(port, server_cert, Some(phone), "POST", "/pair/start", Vec::new(), body).await;
    assert_eq!(reply.status, 200, "the key opens /pair/start: {}", String::from_utf8_lossy(&reply.body));
    let r = reply.json();
    let arr = |f: &str| -> [u8; 32] { B64.decode(r[f].as_str().unwrap()).unwrap().try_into().unwrap() };
    let desktop_pub = arr("ed25519_pub_b64");
    let b = desktop_claim(
        &hex16(r["device_id"].as_str().unwrap()),
        &desktop_pub,
        &arr("x25519_pub_b64"),
        &arr("tls_spki_sha256_b64"),
        r["name"].as_str().unwrap(),
        r["host"].as_str().unwrap(),
        r["port"].as_u64().unwrap() as u16,
    );
    let eph_desktop = arr("eph_pub_b64");
    let t = transcript(&claim.bytes(), &b, &eph_pub, &eph_desktop, &nonce, &arr("nonce_b64"));
    let z = eph.diffie_hellman(&PublicKey::from(eph_desktop)).to_bytes();
    let (cd, secret) = confirm(&key.key, &z, &t);
    assert_eq!(cd, arr("cD_b64"), "the desktop proved it holds the key");
    Started { session_id: r["session_id"].as_str().unwrap().to_string(), secret, desktop_ed25519_pub: desktop_pub }
}

/// The v1 `/pair` body a phone sends once it holds S, and its MAC.
pub fn pair_request(phone: &Phone, secret: &[u8; 32]) -> (Vec<u8>, String) {
    let body = serde_json::to_vec(&serde_json::json!({
        "v": 1,
        "device_id": phone.device_id,
        "name": "Rowan's phone",
        "platform": "ios",
        "ed25519_pub_b64": phone.pub_b64(),
        "tls_cert_der_b64": B64.encode(&phone.tls_cert_der),
    }))
    .unwrap();
    let mac = B64.encode(pair_mac(secret, &body));
    (body, mac)
}

/// Runs the whole pairing round, leaving the phone paired.
pub async fn pair(port: u16, server_cert: &[u8], phone: &Phone, key: &Key) {
    let started = pair_start(port, server_cert, phone, key).await;
    let (body, mac) = pair_request(phone, &started.secret);
    let reply = request(
        port,
        server_cert,
        Some(phone),
        "POST",
        "/pair",
        vec![("X-Daisy-Pair-Mac".into(), mac), ("X-Daisy-Pair-Session".into(), started.session_id)],
        body,
    )
    .await;
    assert_eq!(reply.status, 200, "pairing accepted: {}", String::from_utf8_lossy(&reply.body));
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
