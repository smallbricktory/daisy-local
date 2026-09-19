// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The LAN listener a phone talks to: TLS with the desktop's own
//! certificate, mutual TLS on every route but `/pair`, and a signature over
//! each request. Sections 3-8 of the sync protocol.
//!
//! The server binds every interface, so no route is reachable without either
//! an open pairing window or a client certificate that a pairing put in the
//! identity file.
use std::future::Future;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use ed25519_dalek::Signer;
use http_body_util::{BodyExt, Full};
use hyper::body::Bytes;
use hyper::{Method, Request, Response, StatusCode};

use crate::commands::sync::sync_pairing_accept_impl;
use crate::state::{AppState, VaultState};
use crate::sync::exchange::{accept_tombstone, hello_reply, HelloError, TombstoneRecord, Tombstoned};
use crate::sync::identity::{load_or_create, PairedPhone};
use crate::sync::inbox::{Inbox, RefusalReason, StoreError};
use crate::sync::pairing::PairError;
use crate::sync::signing::{accept_nonce, reply_signing_string, signing_string, verify_request};

/// The two pieces of app state the routes read. A Tauri app holds them in its
/// manager; a test owns them directly.
pub trait SyncStates: Send + Sync + 'static {
    fn app(&self) -> &AppState;
    fn vault(&self) -> &VaultState;

    /// Tells the rest of the app a session arrived or changed. A session
    /// imported without this stays invisible until the next restart.
    fn library_changed(&self, _kind: crate::library_events::LibraryChangeKind, _session_id: &str) {}

    /// Tells the rest of the app this desktop's sync status changed: a phone
    /// paired, or one that is paired was heard from. The settings screen
    /// reads the status once and then only when it is told to.
    fn sync_changed(&self) {}

    /// Wakes a phone over Bluetooth once there is something for it. False on
    /// a desktop with no radio, where the phone collects at its next hello.
    fn wake_phone(&self, _device_id: &str) -> bool {
        false
    }

    /// Tells the rest of the app a phone's session was given up on: this
    /// desktop cannot open it, and the person has to act for it to arrive.
    fn import_refused(&self, _device_id: &str, _session_id: &str, _reason: RefusalReason) {}

    /// Tells the rest of the app how far a phone's session has come, from the
    /// first file landing in the inbox to the last step of the pipeline. The
    /// audio arrives and is processed for minutes before the session appears
    /// in the library, and this is what the person sees in the meantime.
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
}

/// Where this machine stages phone uploads. Beside the identity, named for
/// the machine, because a half-arrived session is one desktop's business
/// even when the profile is synced to another.
pub fn inbox_of(app: &AppState) -> Inbox {
    let name = crate::hardware::sanitized_machine_name();
    Inbox::at(app.profile.root().join(format!("sync.{name}.inbox")))
}

/// How many connections this desktop serves at once. A phone opens one per
/// request and a handful at a time; the cap is what stops anything that
/// reaches the port from holding memory open by the connection.
const MAX_CONNECTIONS: usize = 16;

/// What a `/pair` body holds: an identity and a self-signed certificate. The
/// route reads it before anything about the sender is known, so it is capped
/// far below the file cap every other route works to.
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
            let tls = match acceptor.accept(tcp).await {
                Ok(t) => t,
                Err(e) => {
                    log::debug!("sync: TLS handshake refused: {e}");
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
            if let Err(e) = hyper::server::conn::http1::Builder::new().serve_connection(io, service).await {
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
        PairError::BadRequest => StatusCode::BAD_REQUEST,
        PairError::BadMac => StatusCode::FORBIDDEN,
        PairError::Closed | PairError::DeviceTaken => StatusCode::CONFLICT,
        PairError::Expired => StatusCode::GONE,
        PairError::Internal => StatusCode::INTERNAL_SERVER_ERROR,
    }
}

/// What a refused `/pair` answers. A device id already paired under another
/// key is the one refusal a phone cannot tell from its status alone, so it
/// carries the reason and what the owner has to do.
fn pair_refusal(e: PairError) -> Response<Body> {
    match e {
        PairError::DeviceTaken => reply(
            status_of(e),
            serde_json::to_vec(&serde_json::json!({
                "error": "device_id_taken",
                "message": "A phone is already paired with this computer under that device id. \
                            Unpair it on the computer, then scan a fresh code.",
            }))
            .unwrap_or_default(),
        ),
        _ => empty(status_of(e)),
    }
}

/// The whole route table. `/pair` is open while a window is, and needs no
/// client certificate; everything else needs a paired one and a signature.
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
    let session = header(&req, "X-Daisy-Session").map(str::to_string);
    let file = header(&req, "X-Daisy-File").map(str::to_string);

    if method == Method::POST && path == "/pair" {
        let Some(mac) = pair_mac_b64 else { return empty(StatusCode::BAD_REQUEST) };
        // Nothing is read off a connection that no open code invited.
        if !crate::commands::sync::pairing_window_open(states.app(), crate::now_unix()) {
            return empty(StatusCode::CONFLICT);
        }
        let body = match read_body(req, PAIR_BODY_CAP_BYTES).await {
            Ok(b) => b,
            Err(status) => return empty(status),
        };
        return match sync_pairing_accept_impl(states.app(), states.vault(), crate::now_unix(), &body, &mac) {
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

    // Everything past pairing: the certificate and the signature both have to
    // belong to the same paired phone. The certificate is the half that can be
    // checked from the handshake and the headers, so it is checked first and
    // the body of an unpaired caller is never read.
    let (Some(device), Some(nonce), Some(signature)) = (device, nonce, signature) else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    let Ok(id) = load_or_create(states.app(), states.vault()) else {
        return empty(StatusCode::INTERNAL_SERVER_ERROR);
    };
    let Some(phone) = paired_phone(&id.paired, peer_cert, &device) else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    let body = match read_body(req, crate::sync::inbox::FILE_CAP_BYTES).await {
        Ok(b) => b,
        Err(status) => return signed(states, &path, &nonce, status, Vec::new()),
    };
    if !verify_request(&phone.ed25519_pub_b64, &signature, &signing_string(method.as_str(), &path, &device, &nonce, &body))
    {
        return empty(StatusCode::UNAUTHORIZED);
    }
    if !accept_nonce(states.app(), &device, &nonce) {
        return empty(StatusCode::UNAUTHORIZED);
    }

    let inbox = inbox_of(states.app());
    let sessions_dir = states.app().profile.sessions_dir();
    let states_for_work = states_arc.clone();
    let envelope_key = id.machine_key();
    let (status, out) = match (&method, path.as_str()) {
        (&Method::POST, "/hello") => {
            crate::sync::hellos::record(states.app(), &device);
            states.sync_changed();
            // Off the request path: the phone has about ten seconds for the
            // whole exchange and importing a session copies its audio. A
            // staged file is not wanted again in the meantime, so the reply
            // is the same either way.
            import_ready(&states_for_work, &inbox, &sessions_dir, &device, &id.paired, &envelope_key);
            match hello_reply(&inbox, &sessions_dir, &device, &phone.ed25519_pub_b64, &id.signing_key(), &envelope_key, &body) {
                Ok(r) => {
                    log::info!(
                        "sync: hello from {device} — wants {:?}",
                        r.wants.iter().map(|w| format!("{}/{}", w.session, w.file)).collect::<Vec<_>>()
                    );
                    (StatusCode::OK, serde_json::to_vec(&r).unwrap_or_default())
                }
                Err(HelloError::NewerPhone { highest }) => (
                    StatusCode::UPGRADE_REQUIRED,
                    serde_json::to_vec(&serde_json::json!({ "v": highest })).unwrap_or_default(),
                ),
                Err(HelloError::Malformed) => (StatusCode::BAD_REQUEST, Vec::new()),
            }
        }
        (&Method::PUT, p) if p.starts_with("/file/") => {
            let hash = &p["/file/".len()..];
            let (Some(session), Some(file)) = (session, file) else {
                return signed(states, &path, &nonce, StatusCode::BAD_REQUEST, Vec::new());
            };
            // A session this desktop buried for this device takes no more of
            // its files; one another device buried under the same id is not
            // this device's business.
            let stored = if session == crate::sync::profile::SESSION {
                inbox.store_profile(&device, &file, hash, &body)
            } else if inbox.is_entombed(&session, &device) {
                Err(StoreError::Entombed)
            } else {
                inbox.store(&device, &session, &file, hash, &body)
            };
            let outcome = match stored {
                Ok(()) => {
                    if session != crate::sync::profile::SESSION {
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
                    import_ready(&states_for_work, &inbox, &sessions_dir, &device, &id.paired, &envelope_key);
                    (StatusCode::OK, Vec::new())
                }
                Err(StoreError::Entombed) => (StatusCode::CONFLICT, Vec::new()),
                Err(StoreError::TooLarge) => (StatusCode::PAYLOAD_TOO_LARGE, Vec::new()),
                Err(StoreError::HashMismatch | StoreError::BadName) => (StatusCode::BAD_REQUEST, Vec::new()),
                Err(StoreError::Io) => (StatusCode::INTERNAL_SERVER_ERROR, Vec::new()),
            };
            log::info!("sync: PUT {session}/{file} from {device} → {}", outcome.0.as_u16());
            outcome
        }
        (&Method::GET, p) if p.starts_with("/artifact/") => {
            match crate::sync::exchange::artifact_bytes(&inbox, &sessions_dir, &p["/artifact/".len()..]) {
                Some(bytes) => (StatusCode::OK, bytes),
                None => (StatusCode::NOT_FOUND, Vec::new()),
            }
        }
        (&Method::POST, "/tombstone") => match serde_json::from_slice::<TombstoneRecord>(&body) {
            Ok(record) => {
                let minted = crate::sync::import::minted_ids(&sessions_dir);
                let local = crate::sync::import::local_id(&minted, &device, &record.session);
                match accept_tombstone(&inbox, &sessions_dir, &minted, &device, &record, &phone.ed25519_pub_b64) {
                    Tombstoned::Done => {
                        states.library_changed(crate::library_events::LibraryChangeKind::Deleted, &local);
                        (StatusCode::OK, Vec::new())
                    }
                    Tombstoned::Refused => (StatusCode::FORBIDDEN, Vec::new()),
                    // Nothing was written here. The phone keeps the tombstone
                    // and sends it again, rather than dropping a session this
                    // desktop still holds.
                    Tombstoned::Retry => (StatusCode::SERVICE_UNAVAILABLE, Vec::new()),
                }
            }
            Err(_) => (StatusCode::BAD_REQUEST, Vec::new()),
        },
        _ => (StatusCode::NOT_FOUND, Vec::new()),
    };
    signed(states, &path, &nonce, status, out)
}

/// Imports every staged session whose files have all arrived, each under the
/// device that staged it — never under the device whose connection started
/// this run, which is a second phone often enough that one would stamp its id
/// on the other's recording. A session still missing a file is left staged and
/// tried again at the next hello; one this desktop cannot open is refused at
/// the revisions it arrived at and reported, so the phone stops offering it
/// and the person hears why.
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
    if ready.is_empty() && !inbox.has_staged_profile(device_id) {
        return;
    }
    // Copying a session's audio and then summarizing it takes far longer than
    // a phone's wake window, so none of it happens on the connection.
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
        if crate::sync::profile::merge_staged(states.app(), states.vault(), &inbox, &device_id, &key) {
            states.library_changed(crate::library_events::LibraryChangeKind::Updated, "");
            states.sync_changed();
            states.wake_phone(&device_id);
        }
        for (uploader, session) in ready {
            if !crate::sync::import::is_ready(&inbox, &uploader, &session, &key) {
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
                Ok(local) => {
                    log::info!("sync: imported {session} from {uploader} as {local}");
                    states.library_changed(crate::library_events::LibraryChangeKind::Added, &local);
                    crate::sync::pipeline::run(states.app(), states.vault(), &local, &|stage| {
                        step(stage, Some(&local), None)
                    });
                    states.library_changed(crate::library_events::LibraryChangeKind::Updated, &local);
                    step(ImportStage::Done, Some(&local), session_title(&sessions_dir, &local));
                    // The phone has artifacts waiting; a wake brings it back
                    // for them rather than making it wait for its next hello.
                    states.wake_phone(&uploader);
                }
                Err(e) => match e.refusal() {
                    Some(reason) => {
                        log::warn!("sync: {session} from {uploader} refused: {reason:?}");
                        if let Err(e) = crate::sync::import::refuse(&inbox, &session, &uploader, reason) {
                            log::warn!("sync: refusal of {session} not recorded: {e:?}");
                        }
                        states.import_refused(&uploader, &session, reason);
                        states.sync_changed();
                    }
                    // Not this session's fault and not final: it goes back to
                    // waiting for what has not arrived, and says so rather
                    // than leaving a toast reading "adding it to your library"
                    // until the next hello.
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
/// route allows.
async fn read_body(req: Request<hyper::body::Incoming>, cap: usize) -> Result<Vec<u8>, StatusCode> {
    use hyper::body::Body as _;
    if req.body().size_hint().upper().is_none_or(|n| n > cap as u64) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE);
    }
    req.into_body()
        .collect()
        .await
        .map(|b| b.to_bytes().to_vec())
        .map_err(|_| StatusCode::BAD_REQUEST)
}

/// Managed by Tauri. Holds the running listener so unlock, pairing and
/// unpairing can bring it up and down without an app restart.
#[derive(Default)]
pub struct SyncServerHandle {
    inner: std::sync::Mutex<Option<Running>>,
}

struct Running {
    shutdown: tokio::sync::oneshot::Sender<()>,
}

/// The app's own states, reached through the handle Tauri manages.
struct ManagedStates(tauri::AppHandle);

impl SyncStates for ManagedStates {
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
        crate::sync::peripheral::wake(
            &self.0.state::<crate::sync::peripheral::PeripheralHandle>(),
            self.app(),
            self.vault(),
            device_id,
        )
    }
    fn import_refused(&self, device_id: &str, session_id: &str, reason: RefusalReason) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:refused", RefusedEvent { device_id, session: session_id, reason }) {
            log::warn!("sync: refused event not emitted: {e}");
        }
    }
    fn import_progress(&self, ev: ImportProgress) {
        use tauri::Emitter as _;
        if let Err(e) = self.0.emit("sync:import", &ev) {
            log::warn!("sync: import progress not emitted: {e}");
        }
    }
}

/// True when this desktop should be reachable: the vault is open, and either
/// a phone is paired or the owner is showing a pairing code. A machine with
/// no phone never opens a port on the café wifi.
fn should_listen(app: &AppState, vs: &VaultState) -> bool {
    if !vs.is_unlocked() {
        return false;
    }
    if crate::commands::sync::pairing_window_open(app, crate::now_unix()) {
        return true;
    }
    load_or_create(app, vs).is_ok_and(|id| !id.paired.is_empty())
}

/// Starts or stops the listener to match, and says whether this desktop is
/// reachable afterwards. Called after unlock and lock, when a pairing window
/// opens or closes, and after a phone is paired or removed.
pub fn apply_sync_state(app_handle: &tauri::AppHandle) -> bool {
    use tauri::Manager as _;
    let app = app_handle.state::<AppState>();
    let vault = app_handle.state::<VaultState>();
    let handle = app_handle.state::<SyncServerHandle>();
    let mut running = handle.inner.lock().unwrap();

    // The radio follows the same rule as the socket: a paired phone, or a
    // code on screen. It is published before the bind, so a phone that wakes
    // on it finds the address it needs.
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
        let _ = r.shutdown.send(());
        log::info!("sync: listener stopped");
        return false;
    }

    let listener = match std::net::TcpListener::bind(("0.0.0.0", crate::commands::sync::SYNC_PORT)) {
        Ok(l) => l,
        Err(e) => {
            log::warn!("sync: bind 0.0.0.0:{}: {e}", crate::commands::sync::SYNC_PORT);
            return false;
        }
    };
    if let Err(e) = listener.set_nonblocking(true) {
        log::warn!("sync: set_nonblocking: {e}");
        return false;
    }
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let states = Arc::new(ManagedStates(app_handle.clone()));
    tauri::async_runtime::spawn(async move {
        let listener = match tokio::net::TcpListener::from_std(listener) {
            Ok(l) => l,
            Err(e) => {
                log::warn!("sync: listener conversion failed: {e}");
                return;
            }
        };
        log::info!("sync: listening on 0.0.0.0:{}", crate::commands::sync::SYNC_PORT);
        serve(listener, states, async {
            rx.await.ok();
        })
        .await;
    });
    *running = Some(Running { shutdown: tx });
    true
}
