// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The `daisy-media://` scheme: a session's playback audio for the webview's
//! `<audio>` element, with HTTP Range support.
//!
//! Route: `/session/<id>/meeting`. Answers:
//!
//!   - 200 the whole file (no `Range` header), 206 a byte range with
//!     `Content-Range`, 416 a range past the end.
//!   - 403 while the vault is locked.
//!   - 404 for an unsafe session id, a path outside the sessions directory,
//!     an unknown route, or a session without a `meeting.opus`.
//!
//! A sealed `meeting.opus` is read through its stream: a range decrypts only
//! the 16 KiB segments it touches. Where the webview cannot play Ogg-Opus
//! (macOS) the body is the µ-law WAV built from it, and ranges address the
//! built bytes.

use crate::commands::playback::{resolve_meeting_audio, MAX_PLAYBACK_BYTES, WEBVIEW_PLAYS_OGG_OPUS};
use crate::state::{AppState, VaultState};
use recording::mixdown::MEETING_AUDIO_NAME;
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

/// Most bytes one response carries; the player asks again for the rest.
const MAX_CHUNK: u64 = 8 * 1024 * 1024;

/// A reply to one request, independent of the webview's HTTP types.
#[derive(Debug)]
pub struct MediaReply {
    pub status: u16,
    pub content_type: &'static str,
    pub content_range: Option<String>,
    pub body: Vec<u8>,
}

impl MediaReply {
    fn status(status: u16) -> Self {
        Self { status, content_type: "text/plain", content_range: None, body: Vec::new() }
    }
}

/// The µ-law WAV last built, so each range request of one playback reuses it.
struct Built {
    path: PathBuf,
    len: u64,
    modified: Option<SystemTime>,
    wav: Arc<Vec<u8>>,
}

static BUILT: Mutex<Option<Built>> = Mutex::new(None);

/// Drops the cached WAV; the vault does this on lock.
pub fn forget_built_audio() {
    *BUILT.lock().unwrap_or_else(|e| e.into_inner()) = None;
}

/// Answers a request for `path` (the URL path) with the `Range` header value.
pub fn serve(app: &AppState, vs: &VaultState, path: &str, range: Option<&str>) -> MediaReply {
    serve_as(app, vs, path, range, WEBVIEW_PLAYS_OGG_OPUS)
}

/// [`serve`] with the audio format chosen: the Ogg-Opus stream itself when
/// `ogg_native`, else the built µ-law WAV.
pub fn serve_as(
    app: &AppState,
    vs: &VaultState,
    path: &str,
    range: Option<&str>,
    ogg_native: bool,
) -> MediaReply {
    let Some(id) = route_session_id(path) else {
        return MediaReply::status(404);
    };
    if !vs.is_unlocked() {
        forget_built_audio();
        return MediaReply::status(403);
    }
    // The id check and the containment check both answer 404.
    if resolve_meeting_audio(app, id).is_err() {
        return MediaReply::status(404);
    }
    let file = app.profile.session_path(id).join(MEETING_AUDIO_NAME);
    let result = if ogg_native {
        serve_stream(&file, range)
    } else {
        serve_built(&file, range, &|| vs.is_unlocked())
    };
    result.unwrap_or_else(|e| {
        log::warn!("daisy-media: {}: {e}", file.display());
        MediaReply::status(match e.kind() {
            std::io::ErrorKind::PermissionDenied => 403,
            std::io::ErrorKind::NotFound => 404,
            _ => 500,
        })
    })
}

/// The session id of `/session/<id>/meeting`; `None` for any other path.
fn route_session_id(path: &str) -> Option<&str> {
    let path = path.split(['?', '#']).next().unwrap_or("");
    let mut parts = path.strip_prefix('/')?.split('/');
    match (parts.next(), parts.next(), parts.next(), parts.next()) {
        (Some("session"), Some(id), Some("meeting"), None) if !id.is_empty() => Some(id),
        _ => None,
    }
}

fn serve_stream(file: &std::path::Path, range: Option<&str>) -> std::io::Result<MediaReply> {
    let mut r = sealfile::fs::open_stream(file)?;
    let total = r.len();
    reply(total, range, "audio/ogg", |start, n| {
        r.seek(SeekFrom::Start(start))?;
        let mut out = Vec::with_capacity(n as usize);
        (&mut r).take(n).read_to_end(&mut out)?;
        Ok(out)
    })
}

fn serve_built(file: &std::path::Path, range: Option<&str>, unlocked: &dyn Fn() -> bool) -> std::io::Result<MediaReply> {
    let wav = built_wav(file, unlocked)?;
    reply(wav.len() as u64, range, "audio/wav", |start, n| {
        Ok(wav[start as usize..(start + n) as usize].to_vec())
    })
}

/// The µ-law WAV built from `file`, cached for the next request. A vault
/// that locked while it was built (`unlocked` is false) gets
/// `PermissionDenied`, and nothing is cached.
fn built_wav(file: &std::path::Path, unlocked: &dyn Fn() -> bool) -> std::io::Result<Arc<Vec<u8>>> {
    let meta = std::fs::metadata(file)?;
    let (len, modified) = (meta.len(), meta.modified().ok());
    let mut cache = BUILT.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(b) = cache.as_ref() {
        if b.path == file && b.len == len && b.modified == modified {
            return Ok(b.wav.clone());
        }
    }
    let wav = Arc::new(
        recording::compress::decode_opus_to_ulaw_wav_bytes(file)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e.to_string()))?,
    );
    if !unlocked() {
        return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, "the vault locked"));
    }
    *cache = Some(Built { path: file.to_path_buf(), len, modified, wav: wav.clone() });
    Ok(wav)
}

/// Builds the reply for a body of `total` bytes: `read(start, n)` returns
/// `n` bytes from `start`.
fn reply(
    total: u64,
    range: Option<&str>,
    content_type: &'static str,
    mut read: impl FnMut(u64, u64) -> std::io::Result<Vec<u8>>,
) -> std::io::Result<MediaReply> {
    match range.and_then(|h| parse_range(h, total)) {
        Some(Err(())) => Ok(MediaReply {
            content_range: Some(format!("bytes */{total}")),
            ..MediaReply::status(416)
        }),
        Some(Ok((start, end))) => {
            let end = end.min(start + MAX_CHUNK - 1);
            let body = read(start, end - start + 1)?;
            Ok(MediaReply {
                status: 206,
                content_type,
                content_range: Some(format!("bytes {start}-{end}/{total}")),
                body,
            })
        }
        None if total > MAX_PLAYBACK_BYTES => Ok(MediaReply::status(413)),
        None => Ok(MediaReply { status: 200, content_type, content_range: None, body: read(0, total)? }),
    }
}

/// The inclusive byte range a single-range `Range` header names inside
/// `total` bytes. `None` for a header that is not a single `bytes=` range
/// (served whole); `Err` for a range no byte of the body satisfies.
fn parse_range(header: &str, total: u64) -> Option<Result<(u64, u64), ()>> {
    let spec = header.trim().strip_prefix("bytes=")?;
    if spec.contains(',') {
        return None;
    }
    let (a, b) = spec.split_once('-')?;
    let (a, b) = (a.trim(), b.trim());
    let (start, end) = if a.is_empty() {
        let suffix: u64 = b.parse().ok()?;
        if suffix == 0 {
            return Some(Err(()));
        }
        (total.saturating_sub(suffix), total.checked_sub(1)?)
    } else {
        let start: u64 = a.parse().ok()?;
        let end = if b.is_empty() { u64::MAX } else { b.parse().ok()? };
        if end < start {
            return None;
        }
        (start, end.min(total.saturating_sub(1)))
    };
    if start >= total {
        return Some(Err(()));
    }
    Some(Ok((start, end)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::profile::ProfileDir;

    fn app() -> (AppState, VaultState, tempfile::TempDir) {
        let dir = tempfile::tempdir().unwrap();
        let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
        let vs = VaultState::new();
        *vs.keys.lock().unwrap() = Some(Default::default());
        (AppState::new(profile), vs, dir)
    }

    /// A `meeting.opus` of noise, sealed under the session's key.
    fn write_opus(app: &AppState, id: &str, seconds: usize) -> PathBuf {
        let dir = app.profile.session_path(id);
        std::fs::create_dir_all(&dir).unwrap();
        let mut state = 0x2545_F491u32;
        let pcm: Vec<i16> = (0..16_000 * seconds)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                ((state >> 16) as i16) / 8
            })
            .collect();
        let p = dir.join(MEETING_AUDIO_NAME);
        recording::compress::encode_mono_pcm16(&pcm, 16_000, &recording::compress::CompressParams::default(), &p)
            .unwrap();
        p
    }

    fn plain(p: &std::path::Path) -> Vec<u8> {
        let mut out = Vec::new();
        sealfile::fs::open_stream(p).unwrap().read_to_end(&mut out).unwrap();
        out
    }

    #[test]
    fn media_scheme_serves_range_across_segments() {
        let _k = crate::test_keys::unlocked();
        let (app, vs, _d) = app();
        let p = write_opus(&app, "daisy-1", 30);
        let want = plain(&p);
        assert!(want.len() > 3 * 16 * 1024, "fixture spans several segments");
        assert_eq!(&want[0..4], b"OggS");

        let (a, b) = (16 * 1024 - 100, 2 * 16 * 1024 + 100);
        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", Some(&format!("bytes={a}-{b}")), true);
        assert_eq!(r.status, 206);
        assert_eq!(r.content_range.unwrap(), format!("bytes {a}-{b}/{}", want.len()));
        assert_eq!(r.body, &want[a..=b]);

        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", Some("bytes=-1000"), true);
        assert_eq!(r.status, 206);
        assert_eq!(r.body, &want[want.len() - 1000..]);

        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", Some(&format!("bytes={}-", want.len() - 5)), true);
        assert_eq!((r.status, r.body.as_slice()), (206, &want[want.len() - 5..]));

        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", Some(&format!("bytes={}-", want.len())), true);
        assert_eq!(r.status, 416);

        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", None, true);
        assert_eq!((r.status, r.body), (200, want));
    }

    #[test]
    fn media_scheme_serves_range_over_the_built_wav() {
        let _k = crate::test_keys::unlocked();
        let (app, vs, _d) = app();
        let p = write_opus(&app, "daisy-1", 3);
        let whole = serve_as(&app, &vs, "/session/daisy-1/meeting", None, false);
        assert_eq!(whole.status, 200);
        assert_eq!(&whole.body[0..4], b"RIFF");
        assert_eq!(whole.body, recording::compress::decode_opus_to_ulaw_wav_bytes(&p).unwrap());
        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", Some("bytes=10-49"), false);
        assert_eq!((r.status, r.body.as_slice()), (206, &whole.body[10..50]));
        assert_eq!(r.content_range.unwrap(), format!("bytes 10-49/{}", whole.body.len()));
    }

    #[test]
    fn a_wav_built_while_the_vault_locked_is_not_kept() {
        let _k = crate::test_keys::unlocked();
        let (app, _vs, _d) = app();
        let p = write_opus(&app, "daisy-locks", 1);
        let err = built_wav(&p, &|| false).unwrap_err();
        assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
        let cached = BUILT.lock().unwrap().as_ref().is_some_and(|b| b.path == p);
        assert!(!cached, "the decoded audio is not kept");
    }

    #[test]
    fn media_scheme_locked_is_403() {
        let (app, vs, _d) = app();
        {
            let _k = crate::test_keys::unlocked();
            write_opus(&app, "daisy-1", 1);
        }
        let _k = crate::test_keys::exclusive();
        sealfile::keyring::lock();
        *vs.keys.lock().unwrap() = None;
        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", None, true);
        assert_eq!(r.status, 403);

        *vs.keys.lock().unwrap() = Some(Default::default());
        let r = serve_as(&app, &vs, "/session/daisy-1/meeting", None, true);
        assert_eq!(r.status, 403);
    }

    #[test]
    fn media_scheme_rejects_path_escape() {
        let _k = crate::test_keys::unlocked();
        let (app, vs, _d) = app();
        write_opus(&app, "daisy-1", 1);
        for path in [
            "/session/../meeting",
            "/session/..%2Fdaisy-1/meeting",
            "/session/a/b/meeting",
            "/session//meeting",
            "/session/daisy-1/other",
            "/session/daisy-1/meeting/x",
            "/other/daisy-1/meeting",
            "/session/daisy-missing/meeting",
        ] {
            assert_eq!(serve_as(&app, &vs, path, None, true).status, 404, "{path}");
        }
        #[cfg(unix)]
        {
            let outside = _d.path().join("outside/daisy-2");
            std::fs::create_dir_all(&outside).unwrap();
            std::fs::write(outside.join(MEETING_AUDIO_NAME), b"OggS").unwrap();
            std::os::unix::fs::symlink(&outside, app.profile.session_path("daisy-2")).unwrap();
            assert_eq!(serve_as(&app, &vs, "/session/daisy-2/meeting", None, true).status, 404);
        }
        assert_eq!(serve_as(&app, &vs, "/session/daisy-1/meeting?x=1", None, true).status, 200);
    }

    #[test]
    fn range_headers_parse() {
        assert_eq!(parse_range("bytes=0-9", 100), Some(Ok((0, 9))));
        assert_eq!(parse_range("bytes=90-", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=50-500", 100), Some(Ok((50, 99))));
        assert_eq!(parse_range("bytes=-10", 100), Some(Ok((90, 99))));
        assert_eq!(parse_range("bytes=100-", 100), Some(Err(())));
        assert_eq!(parse_range("bytes=0-1,5-6", 100), None);
        assert_eq!(parse_range("items=0-1", 100), None);
        assert_eq!(parse_range("bytes=0-", 0), Some(Err(())));
    }
}
