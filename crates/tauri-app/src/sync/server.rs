// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The LAN listener a phone talks to: TLS with the desktop's own
//! certificate, mutual TLS on every route but `/pair/start` and `/pair`, and
//! a signature over each request. Sections 3-8 of the sync protocol.
//!
//! The server binds every interface, so no route is reachable without either
//! a live pairing key or a client certificate that a pairing put in the
//! identity file.
use std::future::Future;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};

use crate::commands::sync::{sync_pairing_accept_impl, sync_pairing_start_impl};
use crate::state::{AppState, VaultState};
use crate::sync::exchange::{accept_tombstone, HelloError, TombstoneRecord, Tombstoned};
use crate::sync::identity::{load_or_create, PairedPhone};
use crate::sync::inbox::{Inbox, RefusalReason, StoreError};
use crate::sync::pairing::PairError;
use crate::sync::signing::{accept_nonce, reply_signing_string, signing_string, verify_request};

/// The two pieces of app state the routes read. A Tauri app holds them in its
/// manager; a test owns them directly.
pub trait SyncStates: Send + Sync + 'static {
    fn app(&self) -> &AppState;
    fn vault(&self) -> &VaultState;

    /// Tells the rest of the app a session arrived or changed.
    fn library_changed(&self, _kind: crate::library_events::LibraryChangeKind, _session_id: &str) {}

    /// Tells the rest of the app this desktop's sync status changed: a phone paired, or one that is paired was heard from.
    fn sync_changed(&self) {}

    /// Tells the rest of the app a paired device's hello was refused at the
    /// version gate, the first time and whenever the side at fault changes.
    fn sync_gated(&self, _device_id: &str, _platform: &str, _outdated: crate::sync::gate::Outdated) {}

    /// Wakes a phone over Bluetooth once there is something for it. False on
    /// a desktop with no radio, where the phone collects at its next hello.
    fn wake_phone(&self, _device_id: &str) -> bool {
        false
    }

    /// Tells the rest of the app a phone's session was given up on: this
    /// desktop cannot open it, and the person has to act for it to arrive.
    fn import_refused(&self, _ev: RefusedEvent<'_>) {}

    /// Tells the rest of the app a phone deleted a session whose files or
    /// claim were still waiting here: nothing more of it is coming.
    fn import_withdrawn(&self, _device_id: &str, _session_id: &str) {}

    /// Tells the rest of the app how far a phone's session has come, from the first file landing in the inbox to the last step of the pipeline.
    fn import_progress(&self, _ev: ImportProgress) {}
}

/// Where a phone's session has got to. `Receiving` repeats as each file
/// lands; the rest fire once each, in this order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ImportStage {
    /// Files are still arriving over the LAN.
    Receiving,
    /// Every file is here; the session is being written into the library.
    Importing,
    /// Building `meeting.opus` from the phone's chunks.
    Audio,
    Transcript,
    Summary,
    Chapters,
    /// The session is in the library with everything this desktop builds.
    Done,
}

/// The `sync:import` event payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ImportProgress<'a> {
    pub device_id: &'a str,
    /// The phone's name as the pairing recorded it, for the words a person
    /// reads.
    pub device_name: &'a str,
    pub session: &'a str,
    /// The id the library holds the session under, from the moment it is
    /// there. It is the phone's own id unless this desktop had to mint
    /// another; the meeting a person opens from the toast is this one.
    pub library_session: Option<&'a str>,
    pub stage: ImportStage,
    /// Chunks here and chunks the manifest names, while `stage` is
    /// `Receiving`. Absent until the manifest has arrived.
    pub received: Option<usize>,
    pub total: Option<usize>,
    /// The meeting's title, once the session is in the library.
    pub title: Option<String>,
}

/// The `sync:refused` event payload.
#[derive(Debug, Clone, serde::Serialize)]
pub struct RefusedEvent<'a> {
    pub device_id: &'a str,
    pub session: &'a str,
    pub reason: RefusalReason,
    /// The title a person gave the recording, when it has one this desktop
    /// can read.
    pub title: Option<String>,
    /// When the device started the recording.
    pub recorded_at_unix_seconds: Option<u64>,
}

/// The `sync:withdrawn` event payload: a session a phone deleted, by the id
/// the phone knows it by.
#[derive(Debug, Clone, serde::Serialize)]
pub struct WithdrawnEvent<'a> {
    pub device_id: &'a str,
    pub session: &'a str,
}

/// The `sync:gated` event payload: a paired device whose hello the version
/// gate refused, and the side that is out of date.
#[derive(Debug, Clone, serde::Serialize)]
pub struct GatedEvent<'a> {
    pub device_id: &'a str,
    /// The device's platform from pairing: `ios` or `android`.
    pub platform: &'a str,
    pub out_of_date: crate::sync::gate::Outdated,
}

/// This desktop's `voices.json`, from the vault; `None` while it is locked
/// or holds nothing.
fn voices_bytes(vs: &VaultState) -> Option<Vec<u8>> {
    vs.keys.lock().ok()?.as_ref().and_then(crate::sync::voices::bytes)
}

/// Where this machine stages phone uploads: beside the identity, named for the machine.
pub fn inbox_of(app: &AppState) -> Inbox {
    let name = crate::hardware::sanitized_machine_name();
    Inbox::at(app.profile.root().join(format!("sync.{name}.inbox")))
}

/// How many connections this desktop serves at once.
const MAX_CONNECTIONS: usize = 16;

/// How long a connection may go without progress before it is closed: the
/// TLS handshake, each request's headers, and each piece of a body. A pairing
/// body, read before anything about its sender is known, has this long in
/// total.
const STALL: std::time::Duration = std::time::Duration::from_secs(10);

/// What a pairing body holds: an identity, a self-signed certificate and the exchange values. Capped far below the file cap every other route works to.
const PAIR_BODY_CAP_BYTES: usize = 64 * 1024;

/// Serves until `shutdown` resolves. Each connection is handled on its own
/// task; a phone opens one per request.
pub async fn serve<S: SyncStates>(
    listener: tokio::net::TcpListener,
    states: Arc<S>,
    shutdown: impl Future<Output = ()>,
) {
    let config = match server_config(states.as_ref()) {
        Ok(c) => Arc::new(c),
        Err(e) => {
            log::warn!("sync: no TLS configuration, not serving: {e}");
            return;
        }
    };
    let acceptor = tokio_rustls::TlsAcceptor::from(config);
    let permits = Arc::new(tokio::sync::Semaphore::new(MAX_CONNECTIONS));
    tokio::pin!(shutdown);
    loop {
        let accepted = tokio::select! {
            _ = &mut shutdown => return,
            accepted = listener.accept() => accepted,
        };
        let Ok((tcp, _peer)) = accepted else { continue };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            log::warn!("sync: {MAX_CONNECTIONS} connections are already open; this one is dropped");
            continue;
        };
        let acceptor = acceptor.clone();
        let states = states.clone();
        tokio::spawn(async move {
            let _permit = permit;
            let tls = match tokio::time::timeout(STALL, acceptor.accept(tcp)).await {
                Ok(Ok(t)) => t,
                Ok(Err(e)) => {
                    log::debug!("sync: TLS handshake refused: {e}");
                    return;
                }
                Err(_) => {
                    log::debug!("sync: TLS handshake stalled; the connection is closed");
                    return;
                }
            };
            // The certificate the phone presented, kept for the pairing match.
            let peer_cert = tls.get_ref().1.peer_certificates().and_then(|c| c.first()).map(|c| c.as_ref().to_vec());
            let service = hyper::service::service_fn(move |req| {
                let states = states.clone();
                let peer_cert = peer_cert.clone();
                async move { Ok::<_, std::convert::Infallible>(route(states, peer_cert.as_deref(), req).await) }
            });
            let io = hyper_util::rt::TokioIo::new(tls);
            let served = hyper::server::conn::http1::Builder::new()
                .timer(hyper_util::rt::TokioTimer::new())
                .header_read_timeout(STALL)
                .serve_connection(io, service)
                .await;
            if let Err(e) = served {
                log::debug!("sync: connection ended: {e}");
            }
        });
    }
}

/// TLS with this machine's certificate. Client certificates are accepted
/// unverified at the handshake — a phone's is self-signed, and the pairing
/// table is what decides whether it belongs.
fn server_config<S: SyncStates>(states: &S) -> Result<rustls::ServerConfig, crate::error::AppError> {
    let id = load_or_create(states.app(), states.vault())?;
    let cert = B64
        .decode(&id.tls_cert_der_b64)
        .map_err(|e| crate::error::AppError::Config(format!("sync: certificate: {e}")))?;
    let key = B64
        .decode(&id.tls_key_der_b64)
        .map_err(|e| crate::error::AppError::Config(format!("sync: key: {e}")))?;
    let key = rustls::pki_types::PrivateKeyDer::try_from(key)
        .map_err(|e| crate::error::AppError::Config(format!("sync: key: {e}")))?;
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|e| crate::error::AppError::Config(format!("sync: TLS: {e}")))?
        .with_client_cert_verifier(Arc::new(AnyClientCert))
        .with_single_cert(vec![rustls::pki_types::CertificateDer::from(cert)], key)
        .map_err(|e| crate::error::AppError::Config(format!("sync: TLS: {e}")))
}

/// Takes any client certificate, or none. Whether the phone holding it is
/// paired is decided per request against the identity file, which is the only
/// place a pairing exists.
#[derive(Debug)]
struct AnyClientCert;

impl rustls::server::danger::ClientCertVerifier for AnyClientCert {
    fn root_hint_subjects(&self) -> &[rustls::DistinguishedName] {
        &[]
    }

    fn client_auth_mandatory(&self) -> bool {
        false
    }

    fn verify_client_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::server::danger::ClientCertVerified, rustls::Error> {
        Ok(rustls::server::danger::ClientCertVerified::assertion())
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

type Body = Full<Bytes>;

fn reply(status: StatusCode, body: Vec<u8>) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .body(Full::new(Bytes::from(body)))
        .expect("a valid response")
}

fn empty(status: StatusCode) -> Response<Body> {
    reply(status, Vec::new())
}

fn header<'a>(req: &'a Request<hyper::body::Incoming>, name: &str) -> Option<&'a str> {
    req.headers().get(name).and_then(|v| v.to_str().ok())
}

fn status_of(e: PairError) -> StatusCode {
    match e {
        PairError::BadRequest | PairError::BadPoint => StatusCode::BAD_REQUEST,
        PairError::BadKey | PairError::BadMac | PairError::CertMismatch => StatusCode::FORBIDDEN,
        PairError::Closed | PairError::DeviceTaken => StatusCode::CONFLICT,
        PairError::Expired => StatusCode::GONE,
        PairError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What a refused pairing request answers: the status, and the reason a phone branches on. A device id already paired under another key also carries the action required.
fn pair_refusal(e: PairError) -> Response<Body> {
    let body = match e {
        PairError::DeviceTaken => serde_json::json!({
            "error": e.reason(),
            "message": "A phone is already paired with this computer under that device id. \
                        Unpair it on the computer, then show a new key.",
        }),
        _ => serde_json::json!({ "error": e.reason() }),
    };
    reply(status_of(e), serde_json::to_vec(&body).unwrap_or_default())
}

/// The whole route table. `/pair/start` and `/pair` are open while a key is
/// live and need no paired client certificate; everything else needs a paired
/// one and a signature.
async fn route<S: SyncStates>(
    states_arc: Arc<S>,
    peer_cert: Option<&[u8]>,
    req: Request<hyper::body::Incoming>,
) -> Response<Body> {
    let states = states_arc.as_ref();
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let device = header(&req, "X-Daisy-Device").map(str::to_string);
    let nonce = header(&req, "X-Daisy-Nonce").map(str::to_string);
    let signature = header(&req, "X-Daisy-Signature").map(str::to_string);
    let pair_mac_b64 = header(&req, "X-Daisy-Pair-Mac").map(str::to_string);
    let pair_session = header(&req, "X-Daisy-Pair-Session").map(str::to_string);
    let session = header(&req, "X-Daisy-Session").map(str::to_string);
    let file = header(&req, "X-Daisy-File").map(str::to_string);

    if method == Method::POST && (path == "/pair/start" || path == "/pair") {
        // Nothing is read off a connection that no live key invited.
        if !crate::commands::sync::pairing_key_live(states.app(), crate::now_unix()) {
            let closed = crate::commands::sync::pairing_key_shown(states.app());
            return pair_refusal(if closed { PairError::Expired } else { PairError::Closed });
        }
        let body = match tokio::time::timeout(STALL, read_body(req, PAIR_BODY_CAP_BYTES)).await {
            Ok(Ok(b)) => b,
            Ok(Err(status)) => return empty(status),
            Err(_) => return empty(StatusCode::REQUEST_TIMEOUT),
        };
        if path == "/pair/start" {
            return match sync_pairing_start_impl(states.app(), states.vault(), crate::now_unix(), &body, peer_cert) {
                Ok(reply_body) => reply(StatusCode::OK, reply_body),
                Err(e) => pair_refusal(e),
            };
        }
        let (Some(mac), Some(session)) = (pair_mac_b64, pair_session) else {
            return pair_refusal(PairError::BadRequest);
        };
        return match sync_pairing_accept_impl(states.app(), states.vault(), crate::now_unix(), &body, &mac, &session, peer_cert) {
            Ok((reply_body, sig_b64)) => {
                states.sync_changed();
                Response::builder()
                    .status(StatusCode::OK)
                    .header(hyper::header::CONTENT_TYPE, "application/json")
                    .header("X-Daisy-Signature", sig_b64)
                    .body(Full::new(Bytes::from(reply_body)))
                    .expect("a valid response")
            }
            Err(e) => pair_refusal(e),
        };
    }

    // Everything past pairing: the certificate and the signature both have to belong to the same paired phone. The certificate is checked first, from the handshake and the headers; the body of an unpaired caller is not read.
    let (Some(device), Some(nonce), Some(signature)) = (device, nonce, signature) else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    let Ok(id) = load_or_create(states.app(), states.vault()) else {
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Some(phone) = paired_phone(&id.paired, peer_cert, &device) else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    let body = match read_body(req, body_cap(&path)).await {
        Ok(b) => b,
        Err(status) => return signed(states, &path, &nonce, status, Vec::new()),
    };
    // Everything past the body runs on the blocking pool.
    let phone = phone.clone();
    let work = move || answer(&states_arc, &id, &phone, method, path, device, nonce, signature, session, file, body);
    match tokio::task::spawn_blocking(work).await {
        Ok(response) => response,
        Err(e) => {
            log::warn!("sync: a request's work ended early: {e}");
            empty(StatusCode::INTERNAL_SERVER_ERROR)
        }
    }
}

/// The largest body a route reads. A hello lists every session the phone
/// holds; a tombstone is one signed record.
fn body_cap(path: &str) -> usize {
    match path {
        "/hello" => HELLO_BODY_CAP_BYTES,
        "/tombstone" => TOMBSTONE_BODY_CAP_BYTES,
        _ => crate::sync::inbox::FILE_CAP_BYTES,
    }
}

/// What a hello body holds: a few hundred bytes for each session the phone
/// lists, and its tombstones.
const HELLO_BODY_CAP_BYTES: usize = 4 * 1024 * 1024;

/// What a tombstone body holds: one session id, a sequence number and a
/// signature.
const TOMBSTONE_BODY_CAP_BYTES: usize = 64 * 1024;

/// Answers a request from a paired phone once its body is read: the
/// signature and nonce are checked, then the route runs.
#[allow(clippy::too_many_arguments)]
fn answer<S: SyncStates>(
    states_arc: &Arc<S>,
    id: &crate::sync::identity::SyncIdentity,
    phone: &PairedPhone,
    method: Method,
    path: String,
    device: String,
    nonce: String,
    signature: String,
    session: Option<String>,
    file: Option<String>,
    body: Vec<u8>,
) -> Response<Body> {
    let states = states_arc.as_ref();
    if !verify_request(&phone.ed25519_pub_b64, &signature, &signing_string(method.as_str(), &path, &device, &nonce, &body))
    {
        return empty(StatusCode::UNAUTHORIZED);
    }
    if !accept_nonce(states.app(), &device, &nonce) {
        return empty(StatusCode::UNAUTHORIZED);
    }

    let inbox = inbox_of(states.app());
    let sessions_dir = states.app().profile.sessions_dir();
    let envelope_key = id.machine_key();
    let (status, out) = match (&method, path.as_str()) {
        (&Method::POST, "/hello") => {
            // The version is read before anything else.
            match crate::sync::gate::check(&body, &device) {
                Err(crate::sync::gate::Gate::Malformed) => {
                    return signed(states, &path, &nonce, StatusCode::BAD_REQUEST, Vec::new());
                }
                Err(crate::sync::gate::Gate::Refused(outdated)) => {
                    log::warn!("sync: hello from {device} refused, the {outdated:?} side is out of date");
                    if crate::sync::gate::set(states.app(), &device, Some(outdated)) {
                        states.sync_gated(&device, &phone.platform, outdated);
                        states.sync_changed();
                    }
                    return signed(states, &path, &nonce, StatusCode::UPGRADE_REQUIRED, crate::sync::gate::refusal_body());
                }
                Ok(()) => {
                    crate::sync::gate::set(states.app(), &device, None);
                }
            }
            crate::sync::hellos::record(states.app(), &device);
            states.sync_changed();
            // Off the request path.
            import_ready(states_arc, &inbox, &sessions_dir, &device, &id.paired, &envelope_key);
            let voices = voices_bytes(states.vault());
            match crate::sync::exchange::hello_reply(
                &inbox,
                &sessions_dir,
                &device,
                &phone.ed25519_pub_b64,
                &id.signing_key(),
                &envelope_key,
                &body,
                voices.as_deref(),
            ) {
                Ok(r) => {
                    log::info!(
                        "sync: hello from {device} — wants {:?}",
                        r.wants.iter().map(|w| format!("{}/{}", w.session, w.file)).collect::<Vec<_>>()
                    );
                    for (wire, local, waiting) in &r.buried {
                        states.library_changed(crate::library_events::LibraryChangeKind::Deleted, local);
                        if *waiting {
                            states.import_withdrawn(&device, wire);
                        }
                    }
                    (StatusCode::OK, serde_json::to_vec(&r).unwrap_or_default())
                }
                Err(HelloError::OtherVersion(_)) => (StatusCode::UPGRADE_REQUIRED, crate::sync::gate::refusal_body()),
                Err(HelloError::Malformed) => (StatusCode::BAD_REQUEST, Vec::new()),
            }
        }
        (&Method::PUT, p) if p.starts_with("/file/") => {
            let hash = &p["/file/".len()..];
            let (Some(session), Some(file)) = (session, file) else {
                return signed(states, &path, &nonce, StatusCode::BAD_REQUEST, Vec::new());
            };
            // A session this desktop buried for this device takes no more of its files.
            // A record travels as plaintext and is merged; everything else is a session input.
            let record = crate::sync::records::Channel::of_upload(&session, &file);
            let stored = if inbox.is_entombed(&session, &device) {
                Err(StoreError::Entombed)
            } else if record.is_some() {
                inbox.stage_record(&device, &session, &file, hash, &body)
            } else {
                crate::sync::import::stage(&inbox, &device, &session, &file, hash, &body, &envelope_key)
            };
            let outcome = match stored {
                Ok(()) => {
                    if record.is_none() {
                        let (received, total) =
                            match crate::sync::import::chunk_progress(&inbox, &device, &session, &envelope_key) {
                                Some((have, all)) => (Some(have), Some(all)),
                                None => (None, None),
                            };
                        states.import_progress(ImportProgress {
                            device_id: &device,
                            device_name: phone.shown_name(),
                            session: &session,
                            library_session: None,
                            stage: ImportStage::Receiving,
                            received,
                            total,
                            title: None,
                        });
                    }
                    import_ready(states_arc, &inbox, &sessions_dir, &device, &id.paired, &envelope_key);
                    (StatusCode::OK, Vec::new())
                }
                Err(StoreError::Entombed) => (StatusCode::CONFLICT, Vec::new()),
                Err(StoreError::TooLarge) => (StatusCode::PAYLOAD_TOO_LARGE, Vec::new()),
                Err(StoreError::HashMismatch | StoreError::BadName) => (StatusCode::BAD_REQUEST, Vec::new()),
                Err(StoreError::Io) => (StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
            };
            log::info!(
                "sync: PUT {}/{} from {device} → {}",
                crate::sync::shown(&session),
                crate::sync::shown(&file),
                outcome.0.as_u16()
            );
            outcome
        }
        (&Method::GET, p) if p.starts_with("/artifact/") => {
            let id = &p["/artifact/".len()..];
            match crate::sync::exchange::artifact_bytes(&inbox, &sessions_dir, voices_bytes(states.vault()).as_deref(), id) {
                Some(bytes) => (StatusCode::OK, bytes),
                None => (StatusCode::NOT_FOUND, Vec::new()),
            }
        }
        (&Method::POST, "/tombstone") => match serde_json::from_slice::<TombstoneRecord>(&body) {
            Ok(record) => {
                let minted = crate::sync::import::minted_ids(&sessions_dir);
                let local = crate::sync::import::local_id(&minted, &device, &record.session);
                let waiting = inbox.holds_staged(&device, &record.session);
                match accept_tombstone(&inbox, &sessions_dir, &minted, &device, &record, &phone.ed25519_pub_b64) {
                    Tombstoned::Done => {
                        states.library_changed(crate::library_events::LibraryChangeKind::Deleted, &local);
                        if waiting {
                            states.import_withdrawn(&device, &record.session);
                        }
                        (StatusCode::OK, Vec::new())
                    }
                    Tombstoned::Refused => (StatusCode::FORBIDDEN, Vec::new()),
                    // Nothing was written here; the phone keeps the tombstone and sends it again.
                    Tombstoned::Retry => (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
                }
            }
            Err(_) => (StatusCode::BAD_REQUEST, Vec::new()),
        },
        _ => (StatusCode::NOT_FOUND, Vec::new()),
    };
    signed(states, &path, &nonce, status, out)
}

/// Imports every staged session whose files have all arrived, each under the device that staged it. A session still missing a file is left staged and tried again at the next hello; one this desktop cannot open is refused at the revisions it arrived at and reported.
fn import_ready<S: SyncStates>(
    states: &Arc<S>,
    inbox: &Inbox,
    sessions_dir: &std::path::Path,
    device_id: &str,
    paired: &[PairedPhone],
    key: &crate::sync::import::MachineKey,
) {
    let ready: Vec<(String, String)> = inbox
        .staged_sessions()
        .into_iter()
        .filter(|(uploader, session)| crate::sync::import::is_ready(inbox, uploader, session, key))
        .collect();
    let profile_staged = inbox.staged_records(device_id).iter().any(|(session, _)| session == crate::sync::records::PROFILE);
    if ready.is_empty() && !profile_staged {
        return;
    }
    // Copying a session's audio and summarizing it runs off the connection.
    let states = states.clone();
    let sessions_dir = sessions_dir.to_path_buf();
    let device_id = device_id.to_string();
    let names: std::collections::BTreeMap<String, String> =
        paired.iter().map(|p| (p.device_id.clone(), p.shown_name().to_string())).collect();
    let inbox = inbox.clone();
    let key = key.clone();
    std::thread::spawn(move || {
        // A hello and the upload that completes a session both land here;
        // one worker at a time imports, and a session the other worker
        // already took is no longer staged.
        static IMPORTING: std::sync::Mutex<()> = std::sync::Mutex::new(());
        let _one_at_a_time = IMPORTING.lock().unwrap_or_else(|e| e.into_inner());
        if crate::sync::records::merge_staged_profile(states.app(), states.vault(), &inbox, &device_id, &key) {
            states.library_changed(crate::library_events::LibraryChangeKind::Updated, "");
            states.sync_changed();
            states.wake_phone(&device_id);
        }
        for (uploader, session) in ready {
            if !crate::sync::import::is_ready(&inbox, &uploader, &session, &key) || inbox.is_entombed(&session, &uploader) {
                continue;
            }
            let device_name = names.get(&uploader).map(String::as_str).unwrap_or(uploader.as_str());
            let step = |stage: ImportStage, library_session: Option<&str>, title: Option<String>| {
                states.import_progress(ImportProgress {
                    device_id: &uploader,
                    device_name,
                    session: &session,
                    library_session,
                    stage,
                    received: None,
                    total: None,
                    title,
                });
            };
            step(ImportStage::Importing, None, None);
            match crate::sync::import::import_session(&inbox, &sessions_dir, &uploader, &session, &key) {
                Ok(local) if inbox.is_entombed(&session, &uploader) => {
                    log::info!("sync: {session} from {uploader} was deleted as it landed as {local}");
                    crate::sync::import::bury_landed(&inbox, &sessions_dir, &uploader, &local);
                }
                Ok(local) => {
                    log::info!("sync: imported {session} from {uploader} as {local}");
                    states.library_changed(crate::library_events::LibraryChangeKind::Added, &local);
                    crate::sync::pipeline::run(states.app(), states.vault(), &local, &|stage| {
                        step(stage, Some(&local), None)
                    });
                    states.library_changed(crate::library_events::LibraryChangeKind::Updated, &local);
                    step(ImportStage::Done, Some(&local), session_title(&sessions_dir, &local));
                    // The phone has artifacts waiting; a wake brings it back for them.
                    states.wake_phone(&uploader);
                }
                Err(crate::sync::import::ImportError::Entombed) => {
                    log::info!("sync: {session} from {uploader} was deleted on the device; not imported");
                }
                Err(e) => match e.refusal() {
                    Some(reason) => {
                        log::warn!("sync: {session} from {uploader} refused: {reason:?}");
                        // Read before the refusal drops the staged copies.
                        let title = crate::sync::import::staged_title(&inbox, &uploader, &session, &key);
                        if let Err(e) = crate::sync::import::refuse(&inbox, &session, &uploader, reason) {
                            log::warn!("sync: refusal of {session} not recorded: {e:?}");
                        }
                        states.import_refused(RefusedEvent {
                            device_id: &uploader,
                            session: &session,
                            reason,
                            title,
                            recorded_at_unix_seconds: crate::sync::import::minted_at(&session),
                        });
                        states.sync_changed();
                    }
                    // Not final: the session goes back to waiting for what has not arrived.
                    None => {
                        log::warn!("sync: {session} not imported: {e:?}; tried again at the next hello");
                        let (received, total) =
                            match crate::sync::import::chunk_progress(&inbox, &uploader, &session, &key) {
                                Some((have, all)) => (Some(have), Some(all)),
                                None => (None, None),
                            };
                        states.import_progress(ImportProgress {
                            device_id: &uploader,
                            device_name,
                            session: &session,
                            library_session: None,
                            stage: ImportStage::Receiving,
                            received,
                            total,
                            title: None,
                        });
                    }
                },
            }
        }
    });
}

/// The title an imported session carries, for the words that say it landed.
fn session_title(sessions_dir: &std::path::Path, session: &str) -> Option<String> {
    let bytes = std::fs::read(sessions_dir.join(session).join("manifest.json")).ok()?;
    let value: serde_json::Value = serde_json::from_slice(&bytes).ok()?;
    let title = value["title"].as_str()?.trim();
    (!title.is_empty()).then(|| title.to_string())
}

/// Signs the reply the way section 4 asks: over the status, the path, the
/// request's nonce and the body.
fn signed<S: SyncStates>(states: &S, path: &str, nonce: &str, status: StatusCode, body: Vec<u8>) -> Response<Body> {
    let sig = load_or_create(states.app(), states.vault())
        .map(|id| B64.encode(id.signing_key().sign(reply_signing_string(status.as_u16(), path, nonce, &body).as_bytes()).to_bytes()))
        .unwrap_or_default();
    Response::builder()
        .status(status)
        .header(hyper::header::CONTENT_TYPE, "application/json")
        .header("X-Daisy-Signature", sig)
        .body(Full::new(Bytes::from(body)))
        .expect("a valid response")
}

/// The paired phone that both presented `peer_cert` and claims `device`.
/// Either half alone is not enough.
fn paired_phone<'a>(paired: &'a [PairedPhone], peer_cert: Option<&[u8]>, device: &str) -> Option<&'a PairedPhone> {
    let presented = peer_cert?;
    paired
        .iter()
        .find(|p| p.device_id == device && B64.decode(&p.tls_cert_der_b64).is_ok_and(|der| der == presented))
}

/// The body, refused before it is read when the sender declares more than the
/// route allows, and given up on when it stops arriving for `STALL`.
async fn read_body(req: Request<hyper::body::Incoming>, cap: usize) -> Result<Vec<u8>, StatusCode> {
    use hyper::body::Body as _;
    if req.body().size_hint().upper().is_none_or(|n| n > cap as u64) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    collect_body(req.into_body(), cap, STALL).await
}

/// Reads `body` to its end. Each piece has `stall` to arrive; a body that stops arriving is dropped.
async fn collect_body<B>(mut body: B, cap: usize, stall: std::time::Duration) -> Result<Vec<u8>, StatusCode>
where
    B: hyper::body::Body<Data = Bytes> + Unpin,
{
    let mut out = Vec::new();
    loop {
        match tokio::time::timeout(stall, body.frame()).await {
            Err(_) => return Err(StatusCode::REQUEST_TIMEOUT),
            Ok(None) => return Ok(out),
            Ok(Some(Err(_))) => return Err(StatusCode::BAD_REQUEST),
            Ok(Some(Ok(frame))) => {
                if let Ok(data) = frame.into_data() {
                    if out.len() + data.len() > cap {
                        return Err(StatusCode::PAYLOAD_TOO_LARGE);
                    }
                    out.extend_from_slice(&data);
                }
            }
        }
    }
}

/// Managed by Tauri. Holds the running listener so unlock, pairing and
/// unpairing can bring it up and down without an app restart.
pub struct SyncServerHandle {
    inner: std::sync::Mutex<Option<Running>>,
    /// The port the listener binds; 0 binds a free one.
    port: u16,
}

impl Default for SyncServerHandle {
    /// A handle whose listener binds [`crate::commands::sync::sync_port`].
    fn default() -> Self {
        Self::on_port(crate::commands::sync::sync_port())
    }
}

impl SyncServerHandle {
    /// A handle whose listener binds `port`; 0 binds a free one, which
    /// [`Self::bound_port`] then reports.
    pub fn on_port(port: u16) -> Self {
        Self { inner: Default::default(), port }
    }

    /// The port the running listener holds; `None` while it is down.
    pub fn bound_port(&self) -> Option<u16> {
        self.inner.lock().unwrap().as_ref().map(|r| r.port)
    }
}

struct Running {
    shutdown: tokio::sync::oneshot::Sender<()>,
    /// Disconnects once the server has let go of the port.
    stopped: std::sync::mpsc::Receiver<()>,
    port: u16,
}

/// How long a stop waits for the port to be let go.
const STOP_WAIT: std::time::Duration = std::time::Duration::from_secs(2);

impl Running {
    /// Stops the server and waits (bounded) until its port is free again.
    fn stop(self) {
        let _ = self.shutdown.send(());
        let _ = self.stopped.recv_timeout(STOP_WAIT);
    }
}

/// The app's own states, reached through the handle Tauri manages.
struct ManagedStates<R: tauri::Runtime>(tauri::AppHandle<R>);

impl<R: tauri::Runtime> SyncStates for ManagedStates<R> {
    fn app(&self) -> &AppState {
        use tauri::Manager as _;
        self.0.state::<AppState>().inner()
    }
    fn vault(&self) -> &VaultState {
        use tauri::Manager as _;
        self.0.state::<VaultState>().inner()
    }
    fn library_changed(&self, kind: crate::library_events::LibraryChangeKind, session_id: &str) {
        crate::library_events::emit(&self.0, kind, session_id);
    }
    fn sync_changed(&self) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:changed", ()) {
            log::warn!("sync: status change not emitted: {e}");
        }
    }
    fn wake_phone(&self, device_id: &str) -> bool {
        use tauri::Manager as _;
        crate::sync::peripheral::wake(&self.0.state::<crate::sync::peripheral::PeripheralHandle>(), device_id)
    }
    fn sync_gated(&self, device_id: &str, platform: &str, outdated: crate::sync::gate::Outdated) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:gated", GatedEvent { device_id, platform, out_of_date: outdated }) {
            log::warn!("sync: version gate event not emitted: {e}");
        }
    }
    fn import_refused(&self, ev: RefusedEvent<'_>) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:refused", ev) {
            log::warn!("sync: refused event not emitted: {e}");
        }
    }
    fn import_withdrawn(&self, device_id: &str, session_id: &str) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:withdrawn", WithdrawnEvent { device_id, session: session_id }) {
            log::warn!("sync: withdrawn event not emitted: {e}");
        }
    }
    fn import_progress(&self, ev: ImportProgress) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:import", &ev) {
            log::warn!("sync: import progress not emitted: {e}");
        }
    }
}

/// True when this desktop should be reachable: the vault is open, and either a phone is paired or a pairing code is showing.
fn should_listen(app: &AppState, vs: &VaultState) -> bool {
    if !vs.is_unlocked() {
        return false;
    }
    if crate::commands::sync::pairing_key_live(app, crate::now_unix()) {
        return true;
    }
    load_or_create(app, vs).is_ok_and(|id| !id.paired.is_empty())
}

/// Ends the pairing key if it has run out, and brings the listener and the
/// radio into line: with no phone paired, nothing is left listening.
pub fn end_expired_key<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) {
    use tauri::Manager as _;
    if crate::commands::sync::expire_pairing_key(&app_handle.state::<AppState>(), crate::now_unix()) {
        apply_sync_state(app_handle);
    }
}

/// Starts or stops the listener to match, and says whether this desktop is
/// reachable afterwards. Called after unlock and lock, when a pairing key is
/// shown, cancelled or runs out, and after a phone is paired or removed.
pub fn apply_sync_state<R: tauri::Runtime>(app_handle: &tauri::AppHandle<R>) -> bool {
    use tauri::Manager as _;
    let app = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    let handle = app_handle.state::<SyncServerHandle>();
    let mut running = handle.inner.lock().unwrap();

    // The radio follows the same rule as the socket: a paired phone, or a code on screen. It is published before the bind.
    let peripheral = app_handle.state::<crate::sync::peripheral::PeripheralHandle>();
    if vault.is_unlocked() {
        crate::sync::peripheral::apply(&peripheral, &app, &vault);
    } else {
        crate::sync::peripheral::stop(&peripheral);
    }

    let wanted = should_listen(&app, &vault);
    if wanted == running.is_some() {
        return running.is_some();
    }
    if let Some(r) = running.take() {
        r.stop();
        log::info!("sync: listener stopped");
        return false;
    }

    let listener = match std::net::TcpListener::bind(("0.0.0.0", handle.port)) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("sync: bind 0.0.0.0:{}: {e}", handle.port);
            return false;
        }
    };
    let port = listener.local_addr().map(|a| a.port()).unwrap_or(handle.port);
    if let Err(e) = listener.set_nonblocking(true) {
        log::warn!("sync: set_nonblocking: {e}");
        return false;
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let (stopped_tx, stopped) = std::sync::mpsc::channel::<()>();
    let states = Arc::new(ManagedStates(app_handle.clone()));
    tauri::async_runtime::spawn(async move {
        let _stopped = stopped_tx;
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("sync: listener conversion failed: {e}");
                return;
            }
        };
        log::info!("sync: listening on 0.0.0.0:{port}");
        serve(listener, states, async {
            rx.await.ok();
        })
        .await;
    });
    *running = Some(Running { shutdown: tx, stopped, port });
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::pin::Pin;
    use std::task::{Context, Poll};
    use std::time::Duration;

    /// A body whose pieces arrive when the test sends them.
    struct Trickle(tokio::sync::mpsc::Receiver<Bytes>);

    impl hyper::body::Body for Trickle {
        type Data = Bytes;
        type Error = std::convert::Infallible;
        fn poll_frame(
            mut self: Pin<&mut Self>,
            cx: &mut Context<'_>,
        ) -> Poll<Option<Result<hyper::body::Frame<Bytes>, Self::Error>>> {
            self.0.poll_recv(cx).map(|piece| piece.map(|b| Ok(hyper::body::Frame::data(b))))
        }
    }

    fn trickle(pieces: usize, gap: Duration) -> Trickle {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tokio::spawn(async move {
            for _ in 0..pieces {
                tokio::time::sleep(gap).await;
                if tx.send(Bytes::from_static(b"0123456789")).await.is_err() {
                    return;
                }
            }
        });
        Trickle(rx)
    }

    #[tokio::test]
    async fn a_body_that_keeps_arriving_is_read_however_long_it_takes() {
        let stall = Duration::from_millis(200);
        let started = std::time::Instant::now();
        let body = collect_body(trickle(8, Duration::from_millis(60)), 1024, stall).await.unwrap();
        assert_eq!(body.len(), 80);
        assert!(started.elapsed() > stall * 2, "the whole took longer than the stall bound");
    }

    #[tokio::test]
    async fn a_body_that_stops_arriving_is_given_up_on() {
        let (tx, rx) = tokio::sync::mpsc::channel(1);
        tx.send(Bytes::from_static(b"{\"v\":2,")).await.unwrap();
        assert_eq!(collect_body(Trickle(rx), 1024, Duration::from_millis(100)).await, Err(StatusCode::REQUEST_TIMEOUT));
        drop(tx);
    }

    #[tokio::test]
    async fn a_body_past_the_cap_is_refused() {
        assert_eq!(
            collect_body(trickle(3, Duration::ZERO), 25, Duration::from_secs(1)).await,
            Err(StatusCode::PAYLOAD_TOO_LARGE)
        );
    }
}
