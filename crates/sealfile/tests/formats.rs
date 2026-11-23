// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The sealed formats round-trip, refuse what was tampered with or moved,
//! and read back the authenticated prefix of a stream cut short.

use std::io::{Cursor, Read, Seek, SeekFrom, Write};

use sealfile::keyfile::{derive_session_key, open_with, seal_to, unwrap_session_key, wrap_session_key};
use sealfile::records::{open_records, seal_record, RECORDS_FIRST_LINE};
use sealfile::stream::{open_whole, seal_whole, PcmFormat, StreamHeader, StreamReader, StreamWriter, HEADER_LEN, SEGMENT};
use sealfile::wav_view::wav_view;
use sealfile::{is_sealed, ContentSecret, Format, SealError, SessionKey};

const REL: &str = "chunks/0001/mic.wav";

/// Deterministic bytes that differ at every offset a test reads.
fn bytes(n: usize) -> Vec<u8> {
    (0..n).map(|i| (i.wrapping_mul(31) ^ (i >> 8)) as u8).collect()
}

fn sealed_stream(key: &SessionKey, plain: &[u8], finish: bool) -> Vec<u8> {
    let mut w = StreamWriter::new(Cursor::new(Vec::new()), key, REL, StreamHeader::opaque()).unwrap();
    w.write_all(plain).unwrap();
    if finish {
        w.finish().unwrap().into_inner()
    } else {
        w.flush_full_segments().unwrap();
        w.into_inner_unfinished().into_inner()
    }
}

fn read_all(key: &SessionKey, sealed: Vec<u8>) -> (Vec<u8>, bool) {
    let mut r = StreamReader::open(Cursor::new(sealed), key, REL).unwrap();
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    (out, r.truncated())
}

#[test]
fn whole_round_trip() {
    let key = SessionKey::random();
    let plain = br#"{"chunks":[]}"#;
    let sealed = seal_whole(&key, "manifest.json", plain);
    assert_ne!(&sealed[..], &plain[..]);
    assert_eq!(open_whole(&key, "manifest.json", &sealed).unwrap(), plain);
}

#[test]
fn whole_rejects_other_path() {
    let key = SessionKey::random();
    let sealed = seal_whole(&key, "manifest.json", b"x");
    assert!(matches!(open_whole(&key, "summary.md", &sealed), Err(SealError::Auth)));
}

#[test]
fn whole_rejects_flipped_byte() {
    let key = SessionKey::random();
    let mut sealed = seal_whole(&key, "manifest.json", &bytes(100));
    let at = sealed.len() - 20;
    sealed[at] ^= 1;
    assert!(matches!(open_whole(&key, "manifest.json", &sealed), Err(SealError::Auth)));
}

#[test]
fn whole_rejects_unfinished() {
    let key = SessionKey::random();
    let sealed = sealed_stream(&key, &bytes(SEGMENT * 2 + 5), false);
    let mut r = StreamReader::open(Cursor::new(sealed.clone()), &key, REL).unwrap();
    let mut out = Vec::new();
    r.read_to_end(&mut out).unwrap();
    assert!(r.truncated());
    assert!(matches!(open_whole(&key, REL, &sealed), Err(SealError::Auth)));
}

#[test]
fn stream_round_trip_sizes() {
    let key = SessionKey::random();
    for n in [0, 1, 16383, 16384, 16385, 5 * 16384 + 7] {
        let plain = bytes(n);
        let (out, truncated) = read_all(&key, sealed_stream(&key, &plain, true));
        assert_eq!(out.len(), n, "length for {n}");
        assert_eq!(out, plain, "bytes for {n}");
        assert!(!truncated, "finished stream of {n}");
    }
}

#[test]
fn stream_seek_reads_across_segment_boundary() {
    let key = SessionKey::random();
    let plain = bytes(3 * SEGMENT + 100);
    let mut r = StreamReader::open(Cursor::new(sealed_stream(&key, &plain, true)), &key, REL).unwrap();
    assert_eq!(r.len(), plain.len() as u64);
    r.seek(SeekFrom::Start(16380)).unwrap();
    let mut got = [0u8; 10];
    r.read_exact(&mut got).unwrap();
    assert_eq!(&got[..], &plain[16380..16390]);
    r.seek(SeekFrom::End(-3)).unwrap();
    let mut tail = Vec::new();
    r.read_to_end(&mut tail).unwrap();
    assert_eq!(&tail[..], &plain[plain.len() - 3..]);
}

#[test]
fn stream_torn_tail_returns_prefix() {
    let key = SessionKey::random();
    let plain = bytes(3 * SEGMENT);
    let mut sealed = sealed_stream(&key, &plain, false);
    // Three full segments on disk; cut the third in half.
    let cut = sealed.len() - SEGMENT / 2;
    sealed.truncate(cut);
    let (out, truncated) = read_all(&key, sealed);
    assert_eq!(out.len(), 2 * SEGMENT);
    assert_eq!(out, plain[..2 * SEGMENT]);
    assert!(truncated);
}

#[test]
fn stream_unfinished_writer_reads_full_segments() {
    let key = SessionKey::random();
    let plain = bytes(40_000);
    let mut w = StreamWriter::new(Cursor::new(Vec::new()), &key, REL, StreamHeader::opaque()).unwrap();
    w.write_all(&plain).unwrap();
    let sealed = w.into_inner_unfinished().into_inner();
    let (out, truncated) = read_all(&key, sealed);
    assert_eq!(out.len(), 32_768);
    assert_eq!(out, plain[..32_768]);
    assert!(truncated);
}

#[test]
fn stream_append_continues_finished_and_unfinished() {
    let key = SessionKey::random();
    let first = bytes(20_000);
    let more: Vec<u8> = bytes(10_000).into_iter().map(|b| b ^ 0x5a).collect();
    for finish in [true, false] {
        let sealed = sealed_stream(&key, &first, finish);
        let mut w = StreamWriter::append(Cursor::new(sealed), &key, REL).unwrap();
        w.write_all(&more).unwrap();
        let sealed = w.finish().unwrap().into_inner();
        let (out, truncated) = read_all(&key, sealed);
        // An unfinished stream kept only its full segments.
        let kept = if finish { first.len() } else { SEGMENT };
        let mut want = first[..kept].to_vec();
        want.extend_from_slice(&more);
        assert_eq!(out, want, "finished: {finish}");
        assert!(!truncated);
    }
}

#[test]
fn stream_append_cuts_a_torn_tail() {
    let key = SessionKey::random();
    let mut sealed = sealed_stream(&key, &bytes(3 * SEGMENT), false);
    sealed.truncate(sealed.len() - 100);
    let mut w = StreamWriter::append(Cursor::new(sealed), &key, REL).unwrap();
    w.write_all(b"tail").unwrap();
    let (out, truncated) = read_all(&key, w.finish().unwrap().into_inner());
    let mut want = bytes(3 * SEGMENT)[..2 * SEGMENT].to_vec();
    want.extend_from_slice(b"tail");
    assert_eq!(out, want);
    assert!(!truncated);
}

#[test]
fn short_stream_under_another_key_fails_and_append_leaves_it() {
    let key = SessionKey::random();
    let other = SessionKey::random();
    for len in [10, SEGMENT - 1] {
        let sealed = sealed_stream(&key, &bytes(len), true);
        assert!(matches!(StreamReader::open(Cursor::new(sealed.clone()), &other, REL), Err(SealError::Auth)), "{len} bytes");
        let td = tempfile::tempdir().unwrap();
        let path = td.path().join("mic.wav");
        std::fs::write(&path, &sealed).unwrap();
        let file = std::fs::OpenOptions::new().read(true).write(true).open(&path).unwrap();
        assert!(matches!(StreamWriter::append(file, &other, REL), Err(SealError::Auth)));
        assert_eq!(std::fs::read(&path).unwrap(), sealed, "append left the file as it was");
    }
}

#[test]
fn append_keeps_finished_tail_until_it_is_rewritten() {
    let key = SessionKey::random();
    let first = bytes(20_000);
    let sealed = sealed_stream(&key, &first, true);
    // A writer that stops before the next full segment (a crash) leaves
    // the finished stream readable as it was.
    let mut w = StreamWriter::append(Cursor::new(sealed), &key, REL).unwrap();
    w.write_all(b"lost").unwrap();
    let (out, truncated) = read_all(&key, w.into_inner_unfinished().into_inner());
    assert_eq!(out, first);
    assert!(!truncated);
}

/// On disk, a segment is `nonce (12) || ciphertext || tag (16)`.
const SEALED: usize = SEGMENT + 28;

#[test]
fn append_reseal_uses_fresh_nonce() {
    let key = SessionKey::random();
    let first = bytes(20_000);
    let sealed = sealed_stream(&key, &first, true);
    let last_at = HEADER_LEN + SEALED;
    let before = sealed[last_at..last_at + 12].to_vec();
    let mut w = StreamWriter::append(Cursor::new(sealed), &key, REL).unwrap();
    w.write_all(b"!").unwrap();
    let resealed = w.finish().unwrap().into_inner();
    assert_ne!(&resealed[last_at..last_at + 12], &before[..], "the last segment's nonce is new");
    let mut want = first.clone();
    want.push(b'!');
    assert_eq!(read_all(&key, resealed).0, want);

    // Every segment of every write draws its own nonce.
    let a = sealed_stream(&key, &first, true);
    let b = sealed_stream(&key, &first, true);
    assert_ne!(a[HEADER_LEN..HEADER_LEN + 12], b[HEADER_LEN..HEADER_LEN + 12]);
    assert_ne!(a[HEADER_LEN..HEADER_LEN + 12], a[last_at..last_at + 12]);
}

#[test]
fn reordered_or_dropped_segments_fail_authentication() {
    let key = SessionKey::random();
    let plain = bytes(3 * SEGMENT + 100);
    let sealed = sealed_stream(&key, &plain, true);
    let seg = |i: usize| HEADER_LEN + i * SEALED..HEADER_LEN + (i + 1) * SEALED;

    // Segments 0 and 1 swapped: the first read fails.
    let mut swapped = sealed.clone();
    let (s0, s1) = (sealed[seg(0)].to_vec(), sealed[seg(1)].to_vec());
    swapped[seg(0)].copy_from_slice(&s1);
    swapped[seg(1)].copy_from_slice(&s0);
    let mut r = StreamReader::open(Cursor::new(swapped), &key, REL).unwrap();
    let mut out = Vec::new();
    assert!(r.read_to_end(&mut out).is_err());
    assert!(out.is_empty());

    // Segment 1 dropped: the stream ends after segment 0 and reports the cut.
    let mut dropped = sealed[..seg(1).start].to_vec();
    dropped.extend_from_slice(&sealed[seg(1).end..]);
    let (out, truncated) = read_all(&key, dropped);
    assert_eq!(out, plain[..SEGMENT]);
    assert!(truncated);

    // The last segment dropped: three full segments, cut short.
    let (out, truncated) = read_all(&key, sealed[..seg(3).start].to_vec());
    assert_eq!(out, plain[..3 * SEGMENT]);
    assert!(truncated);

    // A segment from another stream of the same session does not open here.
    let other = sealed_stream(&key, &bytes(3 * SEGMENT + 100), true);
    let mut spliced = sealed.clone();
    spliced[seg(2)].copy_from_slice(&other[seg(1)]);
    let mut r = StreamReader::open(Cursor::new(spliced), &key, REL).unwrap();
    r.seek(SeekFrom::Start(2 * SEGMENT as u64)).unwrap();
    let mut buf = [0u8; 8];
    assert!(r.read(&mut buf).is_err());
}

#[test]
fn segment_from_older_write_of_same_path_fails() {
    let key = SessionKey::random();
    let a = sealed_stream(&key, &bytes(2 * SEGMENT + 10), true);
    let b = sealed_stream(&key, &bytes(2 * SEGMENT + 10).iter().map(|x| x ^ 0xff).collect::<Vec<_>>(), true);
    assert_ne!(a[6..13], b[6..13], "each write draws its own file id");
    let seg0 = HEADER_LEN..HEADER_LEN + SEALED;
    let mut spliced = b.clone();
    spliced[seg0.clone()].copy_from_slice(&a[seg0]);
    let mut r = StreamReader::open(Cursor::new(spliced), &key, REL).unwrap();
    let mut out = Vec::new();
    assert!(r.read_to_end(&mut out).is_err());
    assert!(out.is_empty());

    // Append keeps the file id.
    let mut w = StreamWriter::append(Cursor::new(a.clone()), &key, REL).unwrap();
    w.write_all(b"more").unwrap();
    assert_eq!(w.finish().unwrap().into_inner()[..HEADER_LEN], a[..HEADER_LEN]);
}

#[test]
fn stream_rejects_other_path() {
    let key = SessionKey::random();
    // Nothing authenticates, so the open fails, one short segment included.
    for len in [3 * SEGMENT + 100, 100] {
        let sealed = sealed_stream(&key, &bytes(len), true);
        assert!(matches!(StreamReader::open(Cursor::new(sealed), &key, "chunks/0002/mic.wav"), Err(SealError::Auth)), "{len}");
    }
    // A header alone is an empty stream.
    let sealed = sealed_stream(&key, &[], false);
    let r = StreamReader::open(Cursor::new(sealed), &key, "chunks/0002/mic.wav").unwrap();
    assert!(r.is_empty());
}

#[test]
fn records_drop_torn_last_line() {
    let key = SessionKey::random();
    let rel = "live_transcript.jsonl";
    let mut file = format!("{RECORDS_FIRST_LINE}\n");
    for r in [&b"{\"a\":1}"[..], b"{\"b\":2}", b"{\"c\":3}"] {
        file.push_str(&seal_record(&key, rel, r));
        file.push('\n');
    }
    let whole = file.clone();
    assert_eq!(open_records(&key, rel, whole.as_bytes()), vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec(), b"{\"c\":3}".to_vec()]);
    // A crash cut the last record mid-line.
    let torn = &whole.as_bytes()[..whole.len() - 10];
    assert_eq!(open_records(&key, rel, torn), vec![b"{\"a\":1}".to_vec(), b"{\"b\":2}".to_vec()]);
    // Another path's records do not open here.
    assert!(open_records(&key, "telemetry.jsonl", whole.as_bytes()).is_empty());
    assert!(!seal_record(&key, rel, b"x").contains('\n'));
}

#[test]
fn session_key_wrap_round_trip() {
    let (secret, public) = ContentSecret::generate();
    let key = SessionKey::random();
    let json = wrap_session_key(&public, &key);
    let v: serde_json::Value = serde_json::from_slice(&json).unwrap();
    assert_eq!(v["v"], 1);
    for field in ["content_pub_b64", "ephemeral_pub_b64", "nonce_b64", "wrapped_b64"] {
        assert!(v[field].is_string(), "{field}");
    }
    assert_eq!(unwrap_session_key(&secret, &json).unwrap(), key);

    let sealed = seal_to(&public, "inbox", b"phone upload");
    assert_eq!(open_with(&secret, "inbox", &sealed).unwrap(), b"phone upload");
    assert!(matches!(open_with(&secret, "other", &sealed), Err(SealError::Auth)));
}

#[test]
fn unwrap_reports_other_content_key() {
    let (_, public) = ContentSecret::generate();
    let (other_secret, _) = ContentSecret::generate();
    let json = wrap_session_key(&public, &SessionKey::random());
    assert!(matches!(unwrap_session_key(&other_secret, &json), Err(SealError::OtherContentKey)));
    let sealed = seal_to(&public, "inbox", b"x");
    assert!(matches!(open_with(&other_secret, "inbox", &sealed), Err(SealError::OtherContentKey)));
}

#[test]
fn derived_session_key_is_stable() {
    let secret = ContentSecret::from_bytes([3u8; 32]);
    let again = ContentSecret::from_bytes([3u8; 32]);
    assert_eq!(derive_session_key(&secret, "daisy-1"), derive_session_key(&again, "daisy-1"));
    assert_ne!(derive_session_key(&secret, "daisy-1"), derive_session_key(&secret, "daisy-2"));
    let other = ContentSecret::from_bytes([4u8; 32]);
    assert_ne!(derive_session_key(&secret, "daisy-1"), derive_session_key(&other, "daisy-1"));
}

/// The session-key wrap is envelope v2's X25519 construction: a data key
/// wrapped with label `x25519-wrap` opens with the vault crate's reader.
#[test]
fn wrap_matches_envelope_v2() {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
    let recipient = [5u8; 32];
    let recipient_pub = x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(recipient)).to_bytes();
    let data_key = [7u8; 32];
    let wrapped = sealfile::keyfile::x25519_seal(&sealfile::ContentPublic(recipient_pub), b"x25519-wrap", &data_key);
    let body_nonce = [1u8; 12];
    let body = aes_gcm::Aes256Gcm::new_from_slice(&data_key)
        .unwrap()
        .encrypt(aes_gcm::Nonce::from_slice(&body_nonce), b"the manifest".as_slice())
        .unwrap();
    let envelope = serde_json::json!({
        "schema_version": 2,
        "cipher": "aes-256-gcm",
        "nonce_b64": B64.encode(body_nonce),
        "ciphertext_b64": B64.encode(body),
        "recipients": [{
            "kind": "x25519",
            "id": "mac-1",
            "ephemeral_pub_b64": B64.encode(wrapped.ephemeral_pub),
            "nonce_b64": B64.encode(wrapped.nonce),
            "wrapped_b64": B64.encode(&wrapped.ciphertext),
        }],
    });
    let opened = vault::envelope_v2::open_v2(&serde_json::to_vec(&envelope).unwrap(), &recipient, "mac-1").unwrap();
    assert_eq!(&opened[..], b"the manifest");
}

#[test]
fn wav_view_parses_with_hound() {
    let key = SessionKey::random();
    let samples: Vec<i16> = (0..20_000).map(|i| ((i * 37) % 65_536 - 32_768) as i16).collect();
    let pcm = PcmFormat { sample_rate: 16_000, channels: 1, bits_per_sample: 16 };
    let mut w = StreamWriter::new(Cursor::new(Vec::new()), &key, REL, StreamHeader::pcm(pcm)).unwrap();
    for s in &samples {
        w.write_all(&s.to_le_bytes()).unwrap();
    }
    let sealed = w.finish().unwrap().into_inner();
    let r = StreamReader::open(Cursor::new(sealed), &key, REL).unwrap();
    assert_eq!(r.header().pcm, Some(pcm));
    let mut reader = hound::WavReader::new(wav_view(r)).unwrap();
    let spec = reader.spec();
    assert_eq!((spec.sample_rate, spec.channels, spec.bits_per_sample), (16_000, 1, 16));
    let got: Vec<i16> = reader.samples::<i16>().map(Result::unwrap).collect();
    assert_eq!(got, samples);
}

#[test]
fn is_sealed_detects_each_format_and_plaintext() {
    let key = SessionKey::random();
    assert_eq!(is_sealed(&seal_whole(&key, "a.json", b"{}")), Some(Format::Stream));
    let records = format!("{RECORDS_FIRST_LINE}\n{}\n", seal_record(&key, "a.jsonl", b"{}"));
    assert_eq!(is_sealed(records.as_bytes()), Some(Format::Records));
    assert_eq!(is_sealed(b"{\"chunks\":[]}"), None);
    assert_eq!(is_sealed(b"RIFF\0\0\0\0WAVE"), None);
    assert_eq!(is_sealed(b"DS"), None);
    assert_eq!(is_sealed(b""), None);
}
