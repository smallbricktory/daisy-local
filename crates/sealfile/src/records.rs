// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The records format (`DSR1`): append-only JSONL with each line sealed on
//! its own. The first line is [`RECORDS_FIRST_LINE`]; every line after it
//! is `base64(nonce (12) || ciphertext || tag)`, sealed with a random nonce
//! and the file's relative path as associated data.

use base64::{engine::general_purpose::STANDARD as B64, Engine as _};

use crate::{gcm_open, gcm_seal, random_array, SessionKey};

pub const MAGIC: &[u8; 4] = b"DSR1";
/// The first line of a records file, written without its newline.
pub const RECORDS_FIRST_LINE: &str = "DSR1 1";

/// One sealed line, without a trailing newline.
pub fn seal_record(key: &SessionKey, rel: &str, record: &[u8]) -> String {
    let nonce = random_array::<12>();
    let mut line = nonce.to_vec();
    line.extend_from_slice(&gcm_seal(key.as_bytes(), &nonce, rel.as_bytes(), record));
    B64.encode(line)
}

/// The records in a file, in order. A line that does not open (a torn last
/// line after a crash, a damaged one, or one sealed under a key not given)
/// is dropped. A file without the records first line yields nothing.
pub fn open_records(key: &SessionKey, rel: &str, bytes: &[u8]) -> Vec<Vec<u8>> {
    open_records_any(std::slice::from_ref(key), rel, bytes).0
}

/// [`open_records`] with each line opened under the first key of `keys`
/// that opens it, the key that opened the line before tried first. Also
/// returns how many well-formed lines no key opened: lines sealed under a
/// key not given, or damaged inside the ciphertext. A line that is not
/// base64 of a nonce and a tag (a torn line, a damaged one) is dropped
/// without being counted.
pub fn open_records_any(keys: &[SessionKey], rel: &str, bytes: &[u8]) -> (Vec<Vec<u8>>, usize) {
    let mut lines = bytes.split(|b| *b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    if lines.next() != Some(RECORDS_FIRST_LINE.as_bytes()) {
        return (Vec::new(), 0);
    }
    let (mut out, mut missed, mut last) = (Vec::new(), 0usize, 0usize);
    for line in lines.filter(|l| !l.is_empty()) {
        let order = std::iter::once(last).chain((0..keys.len()).filter(|&i| i != last));
        match order.filter(|&i| i < keys.len()).find_map(|i| open_line(&keys[i], rel, line).map(|r| (i, r))) {
            Some((i, record)) => {
                last = i;
                out.push(record);
            }
            None if is_well_formed(line) => missed += 1,
            None => {}
        }
    }
    (out, missed)
}

/// The index in `keys` of the first key that opens the first record line;
/// `None` when the file holds no record line or no key opens it.
pub fn first_opening_key(keys: &[SessionKey], rel: &str, bytes: &[u8]) -> Option<usize> {
    let mut lines = bytes.split(|b| *b == b'\n').map(|l| l.strip_suffix(b"\r").unwrap_or(l));
    if lines.next() != Some(RECORDS_FIRST_LINE.as_bytes()) {
        return None;
    }
    let line = lines.find(|l| !l.is_empty())?;
    keys.iter().position(|key| open_line(key, rel, line).is_some())
}

/// True when `line` is base64 of at least a nonce and a tag.
fn is_well_formed(line: &[u8]) -> bool {
    B64.decode(line).is_ok_and(|raw| raw.len() >= 12 + 16)
}

fn open_line(key: &SessionKey, rel: &str, line: &[u8]) -> Option<Vec<u8>> {
    let raw = B64.decode(line).ok()?;
    if raw.len() < 12 + 16 {
        return None;
    }
    let nonce: [u8; 12] = raw[..12].try_into().ok()?;
    gcm_open(key.as_bytes(), &nonce, rel.as_bytes(), &raw[12..]).ok()
}
