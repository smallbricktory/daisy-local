//! Calendar subscriptions: periodically fetches read-only ICS URLs and caches
//! parsed events at `<profile>/calendar/events.json`.
//!
//! Threading: `refresh_calendars_impl` issues blocking HTTP and runs inside
//! `spawn_blocking`; read-side helpers only touch the cached JSON and run on
//! the Tauri command thread.

use crate::error::{AppError, Result};
use crate::state::{AppState, CalendarSubscription, VaultState};
use vault::{decrypt, encrypt, Envelope};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::Duration;

/// Read the vault's calendar subscriptions (cloned). Errs if locked.
fn read_subs(vs: &VaultState) -> Result<Vec<CalendarSubscription>> {
    let g = vs.keys.lock().unwrap();
    let keys = g
        .as_ref()
        .ok_or(AppError::VaultLocked)?;
    Ok(keys.calendar_subscriptions.clone())
}

/// Mutate the vault's calendar subscriptions in place, then re-encrypt to
/// disk under the stashed passphrase.
fn with_subs_mut<F>(app: &AppState, vs: &VaultState, mutate: F) -> Result<()>
where
    F: FnOnce(&mut Vec<CalendarSubscription>) -> Result<()>,
{
    {
        let mut g = vs.keys.lock().unwrap();
        let keys = g
            .as_mut()
            .ok_or(AppError::VaultLocked)?;
        mutate(&mut keys.calendar_subscriptions)?;
    }
    crate::commands::lifecycle::re_encrypt_keys(app, vs)
}

const CACHE_SCHEMA: u32 = 1;
/// Events ending more than this many days in the past are not stored.
const KEEP_PAST_DAYS: i64 = 30;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarAttendee {
    pub display_name: Option<String>,
    pub email: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CalendarEvent {
    /// Hash of `(subscription_id, uid)`. Stable across refreshes for the
    /// same feed.
    pub id: String,
    /// Raw ICS UID.
    #[serde(default)]
    pub uid: String,
    /// Which `CalendarSubscription` this event came from.
    pub subscription_id: String,
    pub subscription_name: String,
    /// Color copied from the owning subscription.
    #[serde(default)]
    pub subscription_color: String,
    /// Auto-tag id copied from the owning subscription at parse time.
    #[serde(default)]
    pub subscription_tag_id: Option<String>,
    pub title: String,
    pub start_unix_seconds: i64,
    pub end_unix_seconds: i64,
    #[serde(default)]
    pub attendees: Vec<CalendarAttendee>,
    #[serde(default)]
    pub location: Option<String>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CalendarCache {
    pub schema_version: u32,
    pub fetched_at_unix_seconds: i64,
    pub events: Vec<CalendarEvent>,
}

fn cache_path(app: &AppState) -> PathBuf {
    app.profile.root().join("calendar").join("events.json")
}

use crate::now_unix;

// ---- CRUD over subscriptions (in the encrypted vault) ---------------------

#[derive(Debug, Deserialize)]
pub struct AddSubscriptionRequest {
    pub name: String,
    pub url: String,
}

pub fn list_calendar_subscriptions_impl(
    _app: &AppState,
    vs: &VaultState,
) -> Result<Vec<CalendarSubscription>> {
    read_subs(vs)
}

pub fn add_calendar_subscription_impl(
    app: &AppState,
    vs: &VaultState,
    req: AddSubscriptionRequest,
) -> Result<CalendarSubscription> {
    let name = req.name.trim().to_string();
    let url = req.url.trim().to_string();
    if name.is_empty() {
        return Err(AppError::Config("name is required".into()));
    }
    if !(url.starts_with("https://") || url.starts_with("http://") || url.starts_with("webcal://"))
    {
        return Err(AppError::Config(
            "url must start with https:// (or webcal://)".into(),
        ));
    }
    let entry = CalendarSubscription {
        id: uuid::Uuid::new_v4().to_string(),
        name,
        // Normalize webcal:// → https://.
        url: url.replacen("webcal://", "https://", 1),
        enabled: true,
        color_hex: String::new(), // filled below
        tag_id: None,
        dismissed_event_uids: Vec::new(),
    };
    let mut created = entry.clone();
    with_subs_mut(app, vs, |subs| {
        created.color_hex = pick_default_color(subs.len());
        subs.push(CalendarSubscription { color_hex: created.color_hex.clone(), ..entry });
        Ok(())
    })?;
    Ok(created)
}

#[derive(Debug, Deserialize)]
pub struct UpdateSubscriptionRequest {
    pub id: String,
    pub name: Option<String>,
    pub url: Option<String>,
    pub enabled: Option<bool>,
    pub color_hex: Option<String>,
    /// Empty string clears the linked tag (no auto-tagging); a non-empty
    /// value sets it; `None` (field absent) leaves it untouched.
    pub tag_id: Option<String>,
}

pub fn update_calendar_subscription_impl(
    app: &AppState,
    vs: &VaultState,
    req: UpdateSubscriptionRequest,
) -> Result<CalendarSubscription> {
    let mut saved: Option<CalendarSubscription> = None;
    with_subs_mut(app, vs, |subs| {
        let entry = subs
            .iter_mut()
            .find(|c| c.id == req.id)
            .ok_or_else(|| AppError::Config(format!("no calendar subscription {}", req.id)))?;
        if let Some(n) = req.name {
            let n = n.trim().to_string();
            if !n.is_empty() {
                entry.name = n;
            }
        }
        if let Some(u) = req.url {
            let u = u.trim().to_string();
            if !u.is_empty() {
                entry.url = u.replacen("webcal://", "https://", 1);
            }
        }
        if let Some(e) = req.enabled {
            entry.enabled = e;
        }
        if let Some(c) = req.color_hex {
            let c = c.trim().to_string();
            if !c.is_empty() {
                entry.color_hex = c;
            }
        }
        if let Some(t) = req.tag_id {
            entry.tag_id = if t.is_empty() { None } else { Some(t) };
        }
        saved = Some(entry.clone());
        Ok(())
    })?;
    saved.ok_or_else(|| AppError::Config("subscription vanished mid-update".into()))
}

pub fn delete_calendar_subscription_impl(app: &AppState, vs: &VaultState, id: &str) -> Result<()> {
    with_subs_mut(app, vs, |subs| {
        let before = subs.len();
        subs.retain(|c| c.id != id);
        if subs.len() == before {
            return Err(AppError::Config(format!("no calendar subscription {id}")));
        }
        Ok(())
    })?;

    // Purge cached events for the deleted subscription. Best-effort: a
    // missing cache file is skipped.
    if let Some(mut cache) = read_cache(app, vs) {
        cache.events.retain(|e| e.subscription_id != id);
        write_cache(app, vs, &cache).map_err(|e| {
            AppError::Io(format!("purge calendar cache after delete: {e}"))
        })?;
    }
    Ok(())
}

/// Mark a calendar event as dismissed: the dismissal is stored on the
/// owning subscription and the event is evicted from the live cache.
/// Existing recordings linked to this event are NOT touched.
pub fn dismiss_calendar_event_impl(
    app: &AppState,
    vs: &VaultState,
    subscription_id: &str,
    uid: &str,
) -> Result<()> {
    with_subs_mut(app, vs, |subs| {
        let sub = subs
            .iter_mut()
            .find(|c| c.id == subscription_id)
            .ok_or_else(|| {
                AppError::Config(format!("no calendar subscription {subscription_id}"))
            })?;
        if !sub.dismissed_event_uids.iter().any(|u| u == uid) {
            sub.dismissed_event_uids.push(uid.to_string());
            // FIFO-capped at 1000 entries.
            if sub.dismissed_event_uids.len() > 1000 {
                let overflow = sub.dismissed_event_uids.len() - 1000;
                sub.dismissed_event_uids.drain(0..overflow);
            }
        }
        Ok(())
    })?;

    if let Some(mut cache) = read_cache(app, vs) {
        cache
            .events
            .retain(|e| !(e.subscription_id == subscription_id && e.uid == uid));
        write_cache(app, vs, &cache).map_err(|e| {
            AppError::Io(format!("evict dismissed event from cache: {e}"))
        })?;
    }
    Ok(())
}

/// Palette color for a new subscription; `n` is the count of existing
/// subscriptions.
fn pick_default_color(n: usize) -> String {
    const PALETTE: &[&str] = &[
        "#5BA3D0", // blue
        "#E07A5F", // terracotta
        "#81B29A", // sage
        "#F2CC8F", // sand
        "#9B5DE5", // violet
        "#D7263D", // crimson
    ];
    PALETTE[n % PALETTE.len()].to_string()
}

fn write_cache(app: &AppState, vs: &VaultState, cache: &CalendarCache) -> std::io::Result<()> {
    let path = cache_path(app);
    if let Some(dir) = path.parent() {
        syncsafe::create_dir_all(dir)?;
    }
    // The events cache is encrypted with the vault key. Vault locked (no
    // passphrase) → the write fails and the cache is not updated.
    let plaintext = serde_json::to_vec(cache)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let pass = vs
        .passphrase
        .lock()
        .unwrap()
        .clone()
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::Other, "vault locked"))?;
    let env = encrypt(&plaintext, pass.as_str())
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, format!("encrypt: {e}")))?;
    let bytes = serde_json::to_vec(&env)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    let tmp = path.with_extension("json.tmp");
    syncsafe::write(&tmp, &bytes)?;

    // Atomic replace via rename. On Windows the rename can fail with
    // ACCESS_DENIED (os error 5) while another process transiently holds a
    // handle on the destination; the rename is retried, then falls back to a
    // direct overwrite.
    let mut last_err = None;
    for attempt in 0..5u32 {
        match syncsafe::rename(&tmp, &path) {
            Ok(()) => return Ok(()),
            Err(e) => {
                last_err = Some(e);
                std::thread::sleep(std::time::Duration::from_millis(60 * (attempt as u64 + 1)));
            }
        }
    }
    // Fallback: overwrite the destination directly (non-atomic; the full
    // buffer is written in one call).
    match syncsafe::write(&path, &bytes) {
        Ok(()) => {
            let _ = syncsafe::remove_file(&tmp);
            log::warn!(
                "calendar cache: rename locked (sync client?), wrote in place to {}",
                path.display()
            );
            Ok(())
        }
        Err(_) => Err(last_err.unwrap_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::Other, "calendar cache rename failed")
        })),
    }
}

// ---- refresh ---------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct RefreshResult {
    pub subscriptions_scanned: u32,
    pub events_loaded: u32,
    pub errors: Vec<String>,
}

/// Fetch every enabled subscription and rewrite the on-disk cache. Errors
/// from individual subscriptions are collected; the sweep continues.
pub fn refresh_calendars_impl(app: &AppState, vs: &VaultState) -> Result<RefreshResult> {
    let subs: Vec<CalendarSubscription> = read_subs(vs)?
        .into_iter()
        .filter(|c| c.enabled)
        .collect();
    let mut all: Vec<CalendarEvent> = Vec::new();
    let mut errors: Vec<String> = Vec::new();
    // Microsoft's published-calendar edge serves the ICS feed only to
    // requests with a browser-like User-Agent.
    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(30))
        .user_agent("Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/124.0.0.0 Safari/537.36")
        .build()
        .map_err(|e| AppError::Provider(format!("calendar http client: {e}")))?;
    let cutoff = now_unix() - KEEP_PAST_DAYS * 86_400;
    for sub in &subs {
        // No explicit Accept header: Microsoft's published-calendar proxy
        // returns 417 when one is set.
        match client.get(&sub.url).send() {
            Ok(r) if r.status().is_success() => {
                let body = r.text().unwrap_or_default();
                // An unpublished or org-policy-blocked calendar can 302 to an
                // auth/error page that returns 200 with an empty or HTML body.
                if !body.contains("BEGIN:VCALENDAR") {
                    let hint = if body.trim().is_empty() {
                        "empty response — the publish URL may be disabled by your calendar provider / org policy, or the link is wrong".to_string()
                    } else {
                        format!("response wasn't an ICS feed ({} bytes, no VCALENDAR) — check the URL is a *published* ICS link, not an auth-gated one", body.len())
                    };
                    errors.push(format!("{}: {hint}", sub.name));
                    continue;
                }
                let now = now_unix();
                // Recurrences expand out to 60 days, the max query window in
                // list_upcoming_events_impl.
                let events = parse_ics(&body, sub, now, now + 60 * 86_400);
                if events.is_empty() {
                    errors.push(format!(
                        "{}: feed parsed but contained no events",
                        sub.name
                    ));
                }
                let dismissed: std::collections::HashSet<&str> = sub
                    .dismissed_event_uids
                    .iter()
                    .map(|s| s.as_str())
                    .collect();
                for e in events {
                    if e.end_unix_seconds < cutoff {
                        continue;
                    }
                    if dismissed.contains(e.uid.as_str()) {
                        continue;
                    }
                    all.push(e);
                }
            }
            Ok(r) => {
                errors.push(format!("{}: HTTP {}", sub.name, r.status().as_u16()));
            }
            Err(e) => {
                errors.push(format!("{}: {}", sub.name, e));
            }
        }
    }
    all.sort_by_key(|e| e.start_unix_seconds);
    let count = all.len() as u32;
    let cache = CalendarCache {
        schema_version: CACHE_SCHEMA,
        fetched_at_unix_seconds: now_unix(),
        events: all,
    };
    write_cache(app, vs, &cache).map_err(|e| AppError::Io(format!("write calendar cache: {e}")))?;
    Ok(RefreshResult {
        subscriptions_scanned: subs.len() as u32,
        events_loaded: count,
        errors,
    })
}

fn read_cache(app: &AppState, vs: &VaultState) -> Option<CalendarCache> {
    let bytes = syncsafe::read(cache_path(app)).ok()?;
    // Encrypted with the vault key (see write_cache). A plaintext cache fails
    // to parse as an Envelope → None; the next refresh rewrites it encrypted.
    // Vault locked → None.
    let env: Envelope = serde_json::from_slice(&bytes).ok()?;
    let pass = vs.passphrase.lock().unwrap().clone()?;
    let plain = decrypt(&env, pass.as_str()).ok()?;
    serde_json::from_slice(&plain).ok()
}

#[derive(Debug, Deserialize)]
pub struct UpcomingRequest {
    /// Window in days from now (forward only). Defaults to 7.
    #[serde(default)]
    pub days: Option<u32>,
}

pub fn list_upcoming_events_impl(
    app: &AppState,
    vs: &VaultState,
    req: UpcomingRequest,
) -> Result<Vec<CalendarEvent>> {
    let cache = match read_cache(app, vs) {
        Some(c) => c,
        None => return Ok(Vec::new()),
    };
    let days = req.days.unwrap_or(7).max(1).min(60) as i64;
    let now = now_unix();
    let cutoff = now + days * 86_400;
    // The lower bound is widened by a full day to include earlier-today
    // meetings; the frontend trims to local midnight.
    let floor = now - 86_400;
    Ok(cache
        .events
        .into_iter()
        .filter(|e| e.end_unix_seconds >= floor && e.start_unix_seconds <= cutoff)
        .collect())
}

// ---- ICS parser ------------------------------------------------------------

/// Parse an ICS body into `CalendarEvent`s: VEVENT blocks with
/// UID/SUMMARY/DTSTART/DTEND/ATTENDEE/LOCATION/DESCRIPTION. Handles RFC
/// 5545 line-folding (continuation lines start with a space or tab) and
/// escaped characters in TEXT values.
pub fn parse_ics(
    body: &str,
    sub: &CalendarSubscription,
    now: i64,
    horizon: i64,
) -> Vec<CalendarEvent> {
    let unfolded = unfold(body);
    let tz = parse_vtimezones(&unfolded);
    let mut out: Vec<CalendarEvent> = Vec::new();
    let mut current: Option<EventBuilder> = None;
    for line in unfolded.lines() {
        let trimmed = line.trim_end_matches('\r');
        if trimmed == "BEGIN:VEVENT" {
            current = Some(EventBuilder::default());
            continue;
        }
        if trimmed == "END:VEVENT" {
            if let Some(b) = current.take() {
                // Recurring events expand into per-occurrence instances within
                // the [now-1day, horizon] window; non-recurring events pass
                // through unchanged.
                let rrule = b.rrule.clone();
                let raw = b.dtstart_raw.clone();
                let exdates = b.exdates.clone();
                if let Some(base) = b.build(sub) {
                    match (rrule, raw) {
                        (Some(rr), Some((val, params))) => out.extend(expand_rrule(
                            &base, &val, &params, &rr, &exdates, &tz, now, horizon,
                        )),
                        _ => out.push(base),
                    }
                }
            }
            continue;
        }
        let Some(builder) = current.as_mut() else {
            continue;
        };
        if let Some((name, params, value)) = split_property(trimmed) {
            builder.set(&name, &params, value, &tz);
        }
    }
    out
}

#[derive(Default)]
struct EventBuilder {
    uid: Option<String>,
    summary: Option<String>,
    dtstart: Option<i64>,
    dtend: Option<i64>,
    /// Raw DTSTART (value, params); recurrence expansion re-resolves
    /// occurrences through the tz db with the per-date DST offset.
    dtstart_raw: Option<(String, String)>,
    /// Raw RRULE value (e.g. "FREQ=WEEKLY;BYDAY=MO,WE"), if the event recurs.
    rrule: Option<String>,
    /// Resolved EXDATE instants (occurrences to skip).
    exdates: Vec<i64>,
    attendees: Vec<CalendarAttendee>,
    location: Option<String>,
    description: Option<String>,
}

impl EventBuilder {
    fn set(&mut self, name: &str, params: &str, value: &str, tz: &TzDb) {
        match name {
            "UID" => self.uid = Some(value.to_string()),
            "SUMMARY" => self.summary = Some(decode_text(value)),
            "DTSTART" => {
                self.dtstart = parse_ics_datetime(value, params, tz);
                self.dtstart_raw = Some((value.to_string(), params.to_string()));
            }
            "DTEND" => self.dtend = parse_ics_datetime(value, params, tz),
            "RRULE" => self.rrule = Some(value.to_string()),
            "EXDATE" => {
                for v in value.split(',') {
                    if let Some(t) = parse_ics_datetime(v.trim(), params, tz) {
                        self.exdates.push(t);
                    }
                }
            }
            "ATTENDEE" => {
                let email = value.strip_prefix("mailto:").map(|s| s.to_string());
                let mut cn: Option<String> = None;
                for kv in params.split(';').filter(|p| !p.is_empty()) {
                    if let Some(v) = kv.strip_prefix("CN=") {
                        cn = Some(v.trim_matches('"').to_string());
                    }
                }
                self.attendees.push(CalendarAttendee {
                    display_name: cn,
                    email,
                });
            }
            "LOCATION" => self.location = Some(decode_text(value)),
            "DESCRIPTION" => self.description = Some(decode_text(value)),
            _ => {}
        }
    }

    fn build(self, sub: &CalendarSubscription) -> Option<CalendarEvent> {
        let uid = self.uid?;
        let start = self.dtstart?;
        // A missing DTEND defaults to a 30-minute duration.
        let end = self.dtend.unwrap_or(start + 30 * 60);
        Some(CalendarEvent {
            id: hash_id(&sub.id, &uid),
            uid,
            subscription_id: sub.id.clone(),
            subscription_name: sub.name.clone(),
            subscription_color: sub.color_hex.clone(),
            subscription_tag_id: sub.tag_id.clone(),
            title: self.summary.unwrap_or_else(|| "(no title)".into()),
            start_unix_seconds: start,
            end_unix_seconds: end,
            attendees: self.attendees,
            location: self.location,
            description: self.description,
        })
    }
}

/// Wall-clock components of a DTSTART value (date or date-time).
struct WallClock {
    y: i64,
    mo: i64,
    d: i64,
    h: i64,
    mi: i64,
    se: i64,
    date_only: bool,
}

fn parse_wallclock(value: &str) -> Option<WallClock> {
    let v = value.trim();
    let g = |a: usize, b: usize| v.get(a..b)?.parse::<i64>().ok();
    if v.len() == 8 {
        return Some(WallClock { y: g(0, 4)?, mo: g(4, 6)?, d: g(6, 8)?, h: 0, mi: 0, se: 0, date_only: true });
    }
    if v.len() >= 15 && v.as_bytes().get(8) == Some(&b'T') {
        return Some(WallClock {
            y: g(0, 4)?, mo: g(4, 6)?, d: g(6, 8)?,
            h: g(9, 11)?, mi: g(11, 13)?, se: g(13, 15)?,
            date_only: false,
        });
    }
    None
}

/// Civil date (year, month, day) from a count of days since the Unix epoch —
/// inverse of `ymd_hms_to_unix`'s day math (Howard Hinnant, public domain).
fn civil_from_days(days: i64) -> (i64, i64, i64) {
    let z = days + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = z - era * 146097; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365; // [0, 399]
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Day-number since epoch for a civil date (00:00).
fn days_from_civil(y: i64, mo: i64, d: i64) -> Option<i64> {
    Some(
        ymd_hms_to_unix(
            &y.to_string(),
            &format!("{mo:02}"),
            &format!("{d:02}"),
            "00", "00", "00", true,
        )? / 86_400,
    )
}

/// Expand a VEVENT RRULE into occurrence events from the base event through
/// `horizon` (unix seconds). Supports `FREQ=DAILY|WEEKLY|MONTHLY`, `INTERVAL`,
/// `COUNT`, `UNTIL`, weekly `BYDAY` (multi-day), and `EXDATE` skips. Each
/// occurrence is re-resolved through the tz db with that date's DST offset.
/// COUNT is honoured from the original DTSTART; only occurrences ending
/// at/after `now - 1 day` are emitted. Unsupported/garbled FREQ falls back to
/// just the base event.
#[allow(clippy::too_many_arguments)]
fn expand_rrule(
    base: &CalendarEvent,
    dtstart_value: &str,
    dtstart_params: &str,
    rrule: &str,
    exdates: &[i64],
    tz: &TzDb,
    now: i64,
    horizon: i64,
) -> Vec<CalendarEvent> {
    let Some(wc) = parse_wallclock(dtstart_value) else { return vec![base.clone()] };
    let utc = dtstart_value.trim().ends_with('Z') || dtstart_params.contains("TZID=UTC");
    let duration = (base.end_unix_seconds - base.start_unix_seconds).max(0);
    let floor = now - 86_400;

    let mut freq = "";
    let mut interval: i64 = 1;
    let mut count: Option<i64> = None;
    let mut until: Option<i64> = None;
    let mut byday: Vec<i64> = Vec::new();
    for part in rrule.split(';') {
        let Some((k, val)) = part.split_once('=') else { continue };
        match k {
            "FREQ" => freq = val,
            "INTERVAL" => interval = val.parse().unwrap_or(1).max(1),
            "COUNT" => count = val.parse().ok(),
            "UNTIL" => until = parse_ics_datetime(val, dtstart_params, tz),
            "BYDAY" => {
                byday = val
                    .split(',')
                    .filter_map(|d| parse_byday(d).map(|(_, w)| w))
                    .collect();
            }
            _ => {}
        }
    }

    // Resolve a single occurrence's (y,mo,d) to its UTC start, applying the
    // original wall-clock time + the correct DST offset for THAT date.
    let resolve = |y: i64, mo: i64, d: i64| -> Option<i64> {
        let v = if wc.date_only {
            format!("{y:04}{mo:02}{d:02}")
        } else {
            format!("{y:04}{mo:02}{d:02}T{:02}{:02}{:02}{}", wc.h, wc.mi, wc.se, if utc { "Z" } else { "" })
        };
        parse_ics_datetime(&v, dtstart_params, tz)
    };

    let mut out: Vec<CalendarEvent> = Vec::new();
    let mut emitted: i64 = 0; // counts toward COUNT, from DTSTART forward
    const MAX_OCC: i64 = 2000;

    // Each candidate is a civil (y,mo,d). `emit` resolves + filters it and
    // returns false when a stop condition (COUNT/UNTIL/horizon) is hit.
    let mut total = 0i64;
    let emit = |y: i64, mo: i64, d: i64, out: &mut Vec<CalendarEvent>, emitted: &mut i64| -> bool {
        let Some(start) = resolve(y, mo, d) else { return true };
        if start < base.start_unix_seconds {
            return true; // before the series start; skip, keep going
        }
        if let Some(u) = until {
            if start > u {
                return false;
            }
        }
        if start > horizon {
            return false;
        }
        *emitted += 1;
        if let Some(c) = count {
            if *emitted > c {
                return false;
            }
        }
        let end = start + duration;
        if end < floor || exdates.contains(&start) {
            return true; // past or excluded — counted, not emitted
        }
        let mut ev = base.clone();
        ev.start_unix_seconds = start;
        ev.end_unix_seconds = end;
        ev.id = hash_id(&base.subscription_id, &format!("{}@{start}", base.uid));
        out.push(ev);
        true
    };

    match freq {
        "DAILY" => {
            let start_days = days_from_civil(wc.y, wc.mo, wc.d).unwrap_or(0);
            let mut n = 0i64;
            loop {
                total += 1;
                if total > MAX_OCC {
                    break;
                }
                let (y, mo, d) = civil_from_days(start_days + n * interval);
                if !emit(y, mo, d, &mut out, &mut emitted) {
                    break;
                }
                n += 1;
            }
        }
        "WEEKLY" => {
            let start_days = days_from_civil(wc.y, wc.mo, wc.d).unwrap_or(0);
            let start_dow = (start_days % 7 + 4).rem_euclid(7); // 0=Sun
            let week_start = start_days - start_dow; // Sunday of DTSTART week
            let mut days: Vec<i64> = if byday.is_empty() { vec![start_dow] } else { byday.clone() };
            days.sort_unstable();
            days.dedup();
            let mut wk = 0i64;
            'weeks: loop {
                total += 1;
                if total > MAX_OCC {
                    break;
                }
                let block = week_start + wk * interval * 7;
                if block > start_days + (horizon - base.start_unix_seconds) / 86_400 + 7 {
                    break;
                }
                for &wd in &days {
                    let (y, mo, d) = civil_from_days(block + wd);
                    if !emit(y, mo, d, &mut out, &mut emitted) {
                        break 'weeks;
                    }
                }
                wk += 1;
            }
        }
        "MONTHLY" => {
            let mut n = 0i64;
            loop {
                total += 1;
                if total > MAX_OCC {
                    break;
                }
                // DTSTART month + n*interval, same day-of-month (skip months
                // that don't have that day, e.g. the 31st).
                let total_months = (wc.y * 12 + (wc.mo - 1)) + n * interval;
                let y = total_months.div_euclid(12);
                let mo = total_months.rem_euclid(12) + 1;
                if wc.d <= days_in_month(y, mo) {
                    if !emit(y, mo, wc.d, &mut out, &mut emitted) {
                        break;
                    }
                } else if resolve(y, mo, 1).map(|s| s > horizon).unwrap_or(false) {
                    break; // past the horizon even at month start
                }
                n += 1;
            }
        }
        _ => return vec![base.clone()], // unsupported FREQ → first instance only
    }

    out
}

fn unfold(s: &str) -> String {
    // RFC 5545 §3.1 — a CRLF followed by a single LWSP character (space
    // or tab) is a "folded" continuation of the previous line.
    let mut out = String::with_capacity(s.len());
    for line in s.split('\n') {
        let line = line.trim_end_matches('\r');
        if (line.starts_with(' ') || line.starts_with('\t')) && !out.is_empty() {
            // pop the trailing newline written for the previous line
            if out.ends_with('\n') {
                out.pop();
            }
            out.push_str(&line[1..]);
            out.push('\n');
        } else {
            out.push_str(line);
            out.push('\n');
        }
    }
    out
}

/// Split `NAME[;PARAMS]:VALUE`. Returns (name, params_str, value). The
/// params_str includes the leading `;` if any params are present (or empty).
fn split_property(line: &str) -> Option<(String, String, &str)> {
    let colon = line.find(':')?;
    let head = &line[..colon];
    let value = &line[colon + 1..];
    let (name, params) = match head.find(';') {
        Some(p) => (&head[..p], &head[p + 1..]),
        None => (head, ""),
    };
    Some((name.to_string(), params.to_string(), value))
}

fn decode_text(s: &str) -> String {
    // Unescape per RFC 5545 §3.3.11.
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') | Some('N') => out.push('\n'),
                Some(',') => out.push(','),
                Some(';') => out.push(';'),
                Some('\\') => out.push('\\'),
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn parse_ics_datetime(value: &str, params: &str, tz: &TzDb) -> Option<i64> {
    // Supported forms:
    //   YYYYMMDD                      — DATE; treated as 00:00 UTC (all-day)
    //   YYYYMMDDTHHMMSS               — floating; treated as UTC
    //   YYYYMMDDTHHMMSSZ              — UTC
    //   YYYYMMDDTHHMMSS with TZID=…   — wall-clock in the named zone, resolved
    //     to a UTC instant via the VTIMEZONE block carried in the same feed
    //     (offsets + DST transition rules).
    let v = value.trim();
    if v.len() == 8 {
        // DATE only.
        return ymd_hms_to_unix(&v[0..4], &v[4..6], &v[6..8], "00", "00", "00", true);
    }
    if v.len() >= 15 && v.as_bytes().get(8) == Some(&b'T') {
        let y = &v[0..4];
        let mo = &v[4..6];
        let d = &v[6..8];
        let h = &v[9..11];
        let mi = &v[11..13];
        let se = &v[13..15];
        if v.ends_with('Z') || params.contains("TZID=UTC") {
            return ymd_hms_to_unix(y, mo, d, h, mi, se, true);
        }
        // Wall-clock value. Compute the naive instant first, then shift by the
        // zone offset that applies on that wall date.
        let naive = ymd_hms_to_unix(y, mo, d, h, mi, se, true)?;
        if let Some(tzid) = extract_tzid(params) {
            if let Some(info) = tz.get(&tzid) {
                let year: i64 = y.parse().ok()?;
                let offset = info.offset_for(year, naive);
                // wall = utc + offset  ⇒  utc = wall − offset
                return Some(naive - offset);
            }
        }
        // Unknown zone or floating time is treated as UTC.
        return Some(naive);
    }
    None
}

type TzDb = std::collections::HashMap<String, TzInfo>;

/// A DST transition rule extracted from a VTIMEZONE subcomponent's RRULE
/// (e.g. `FREQ=YEARLY;BYDAY=2SU;BYMONTH=3` → 2nd Sunday of March).
#[derive(Clone)]
struct TzRule {
    month: i64,
    nth: i64,
    weekday: i64,
    time_secs: i64,
}

/// Resolved offsets + transition rules for one named zone.
struct TzInfo {
    std_offset: i64,
    dst_offset: Option<i64>,
    dst_start: Option<TzRule>,
    std_start: Option<TzRule>,
}

impl TzInfo {
    /// Offset (seconds east of UTC) in effect for the given naive wall-clock
    /// instant. Falls back to the standard offset when DST rules are absent.
    fn offset_for(&self, year: i64, naive_wall: i64) -> i64 {
        let (Some(dst_off), Some(dstr), Some(stdr)) =
            (self.dst_offset, self.dst_start.as_ref(), self.std_start.as_ref())
        else {
            return self.std_offset;
        };
        let (Some(dst_begin), Some(std_begin)) =
            (transition_unix(year, dstr), transition_unix(year, stdr))
        else {
            return self.std_offset;
        };
        let in_dst = if dst_begin < std_begin {
            naive_wall >= dst_begin && naive_wall < std_begin
        } else {
            // Southern hemisphere: daylight wraps the year boundary.
            naive_wall >= dst_begin || naive_wall < std_begin
        };
        if in_dst {
            dst_off
        } else {
            self.std_offset
        }
    }
}

/// Naive (wall-clock) unix seconds of a transition rule in a given year.
fn transition_unix(year: i64, rule: &TzRule) -> Option<i64> {
    let day = nth_weekday_of_month(year, rule.month, rule.nth, rule.weekday)?;
    let base = ymd_hms_to_unix(
        &year.to_string(),
        &format!("{:02}", rule.month),
        &format!("{:02}", day),
        "00",
        "00",
        "00",
        true,
    )?;
    Some(base + rule.time_secs)
}

/// Day-of-month for the `nth` `weekday` (0=Sun) of `month`. `nth` may be
/// negative for "last" (e.g. -1 = last Sunday).
fn nth_weekday_of_month(year: i64, month: i64, nth: i64, weekday: i64) -> Option<i64> {
    if nth > 0 {
        let first = ymd_hms_to_unix(
            &year.to_string(),
            &format!("{:02}", month),
            "01",
            "00",
            "00",
            "00",
            true,
        )?;
        let first_dow = ((first / 86_400) % 7 + 4).rem_euclid(7);
        let offset = (weekday - first_dow).rem_euclid(7);
        Some(1 + offset + (nth - 1) * 7)
    } else {
        let dim = days_in_month(year, month);
        let last = ymd_hms_to_unix(
            &year.to_string(),
            &format!("{:02}", month),
            &format!("{:02}", dim),
            "00",
            "00",
            "00",
            true,
        )?;
        let last_dow = ((last / 86_400) % 7 + 4).rem_euclid(7);
        let back = (last_dow - weekday).rem_euclid(7);
        Some(dim - back)
    }
}

fn days_in_month(year: i64, month: i64) -> i64 {
    let leap = (year % 4 == 0 && year % 100 != 0) || year % 400 == 0;
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 => {
            if leap {
                29
            } else {
                28
            }
        }
        _ => 30,
    }
}

/// Pull the `TZID=` value out of a property's params string. Strips
/// surrounding quotes; the value itself may contain spaces (Windows zone
/// names like "Pacific Standard Time").
fn extract_tzid(params: &str) -> Option<String> {
    for kv in params.split(';') {
        if let Some(v) = kv.strip_prefix("TZID=") {
            return Some(v.trim_matches('"').to_string());
        }
    }
    None
}

/// Parse all VTIMEZONE blocks into a name→offset/rules map. Each block has a
/// TZID plus STANDARD and (optionally) DAYLIGHT subcomponents carrying
/// TZOFFSETTO and an RRULE describing the yearly transition.
fn parse_vtimezones(unfolded: &str) -> TzDb {
    let mut db = TzDb::new();
    let mut tzid: Option<String> = None;
    let mut std_offset: Option<i64> = None;
    let mut dst_offset: Option<i64> = None;
    let mut std_start: Option<TzRule> = None;
    let mut dst_start: Option<TzRule> = None;
    // Current subcomponent: 0=none, 1=STANDARD, 2=DAYLIGHT.
    let mut sub = 0u8;
    let mut sub_offset: Option<i64> = None;
    let mut sub_rule_month: Option<i64> = None;
    let mut sub_rule_nth: Option<i64> = None;
    let mut sub_rule_weekday: Option<i64> = None;
    let mut sub_time_secs: i64 = 0;

    let mut in_tz = false;
    for line in unfolded.lines() {
        let line = line.trim_end_matches('\r');
        match line {
            "BEGIN:VTIMEZONE" => {
                in_tz = true;
                tzid = None;
                std_offset = None;
                dst_offset = None;
                std_start = None;
                dst_start = None;
            }
            "END:VTIMEZONE" => {
                if let Some(id) = tzid.take() {
                    if let Some(so) = std_offset {
                        db.insert(
                            id,
                            TzInfo {
                                std_offset: so,
                                dst_offset,
                                dst_start: dst_start.clone(),
                                std_start: std_start.clone(),
                            },
                        );
                    }
                }
                in_tz = false;
            }
            "BEGIN:STANDARD" | "BEGIN:DAYLIGHT" => {
                sub = if line.ends_with("STANDARD") { 1 } else { 2 };
                sub_offset = None;
                sub_rule_month = None;
                sub_rule_nth = None;
                sub_rule_weekday = None;
                sub_time_secs = 0;
            }
            "END:STANDARD" | "END:DAYLIGHT" => {
                let rule = match (sub_rule_month, sub_rule_nth, sub_rule_weekday) {
                    (Some(m), Some(n), Some(w)) => Some(TzRule {
                        month: m,
                        nth: n,
                        weekday: w,
                        time_secs: sub_time_secs,
                    }),
                    _ => None,
                };
                if sub == 1 {
                    std_offset = sub_offset;
                    std_start = rule;
                } else if sub == 2 {
                    dst_offset = sub_offset;
                    dst_start = rule;
                }
                sub = 0;
            }
            _ if in_tz => {
                if let Some((name, _params, value)) = split_property(line) {
                    match name.as_str() {
                        "TZID" if sub == 0 => tzid = Some(value.to_string()),
                        "TZOFFSETTO" if sub != 0 => sub_offset = parse_utc_offset(value),
                        "DTSTART" if sub != 0 => {
                            // e.g. 16010101T020000 — only the time matters.
                            if value.len() >= 15 && value.as_bytes().get(8) == Some(&b'T') {
                                let h: i64 = value[9..11].parse().unwrap_or(0);
                                let mi: i64 = value[11..13].parse().unwrap_or(0);
                                let se: i64 = value[13..15].parse().unwrap_or(0);
                                sub_time_secs = h * 3600 + mi * 60 + se;
                            }
                        }
                        "RRULE" if sub != 0 => {
                            for kv in value.split(';') {
                                if let Some(m) = kv.strip_prefix("BYMONTH=") {
                                    sub_rule_month = m.parse().ok();
                                } else if let Some(byday) = kv.strip_prefix("BYDAY=") {
                                    if let Some((n, w)) = parse_byday(byday) {
                                        sub_rule_nth = Some(n);
                                        sub_rule_weekday = Some(w);
                                    }
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    db
}

/// Parse an ICS UTC offset like `-0800` / `+0530` into seconds east of UTC.
fn parse_utc_offset(s: &str) -> Option<i64> {
    let s = s.trim();
    let (sign, rest) = match s.as_bytes().first()? {
        b'-' => (-1, &s[1..]),
        b'+' => (1, &s[1..]),
        _ => (1, s),
    };
    if rest.len() < 4 {
        return None;
    }
    let h: i64 = rest[0..2].parse().ok()?;
    let m: i64 = rest[2..4].parse().ok()?;
    Some(sign * (h * 3600 + m * 60))
}

/// Parse an RRULE BYDAY token like `2SU` (2nd Sunday) or `-1SU` (last
/// Sunday) into (nth, weekday) with weekday 0=Sun … 6=Sat.
fn parse_byday(s: &str) -> Option<(i64, i64)> {
    let s = s.trim();
    let (num, day) = s.split_at(s.len().checked_sub(2)?);
    let weekday = match day {
        "SU" => 0,
        "MO" => 1,
        "TU" => 2,
        "WE" => 3,
        "TH" => 4,
        "FR" => 5,
        "SA" => 6,
        _ => return None,
    };
    let nth = if num.is_empty() { 1 } else { num.parse().ok()? };
    Some((nth, weekday))
}

/// DATE+TIME → unix seconds. The `_utc` flag is unused; the arithmetic is
/// offset-agnostic (callers apply any zone offset themselves).
fn ymd_hms_to_unix(
    y: &str, mo: &str, d: &str, h: &str, mi: &str, se: &str, _utc: bool,
) -> Option<i64> {
    let y: i64 = y.parse().ok()?;
    let mo: i64 = mo.parse().ok()?;
    let d: i64 = d.parse().ok()?;
    let h: i64 = h.parse().ok()?;
    let mi: i64 = mi.parse().ok()?;
    let se: i64 = se.parse().ok()?;
    if !(1..=12).contains(&mo) || !(1..=31).contains(&d) {
        return None;
    }
    // Days from civil date (Howard Hinnant's algorithm — public domain).
    let y_adj = if mo <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = (y_adj - era * 400) as i64; // [0..399]
    let mp = if mo > 2 { mo - 3 } else { mo + 9 } as i64; // [0..11]
    let doy = (153 * mp + 2) / 5 + d - 1; // [0..365]
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy; // [0..146096]
    let days_since_epoch = era * 146097 + doe - 719468;
    Some(days_since_epoch * 86_400 + h * 3600 + mi * 60 + se)
}

fn hash_id(subscription_id: &str, uid: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    subscription_id.hash(&mut h);
    uid.hash(&mut h);
    format!("{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sub() -> CalendarSubscription {
        CalendarSubscription {
            id: "s1".into(),
            name: "Test".into(),
            url: "https://example.com/cal.ics".into(),
            enabled: true,
            color_hex: "#5BA3D0".into(),
            tag_id: None,
            dismissed_event_uids: Vec::new(),
        }
    }

    #[test]
    fn parses_simple_vevent() {
        let body = "BEGIN:VCALENDAR\r\n\
                    BEGIN:VEVENT\r\n\
                    UID:abc-123\r\n\
                    SUMMARY:Team Standup\r\n\
                    DTSTART:20260601T140000Z\r\n\
                    DTEND:20260601T143000Z\r\n\
                    LOCATION:Zoom\r\n\
                    ATTENDEE;CN=\"Sam P\":mailto:sam@example.com\r\n\
                    ATTENDEE;CN=Tara:mailto:tara@example.com\r\n\
                    END:VEVENT\r\n\
                    END:VCALENDAR\r\n";
        let evs = parse_ics(body, &sub(), 0, i64::MAX);
        assert_eq!(evs.len(), 1);
        let e = &evs[0];
        assert_eq!(e.title, "Team Standup");
        assert_eq!(e.location.as_deref(), Some("Zoom"));
        assert_eq!(e.attendees.len(), 2);
        assert_eq!(e.attendees[0].display_name.as_deref(), Some("Sam P"));
        assert_eq!(e.attendees[0].email.as_deref(), Some("sam@example.com"));
        assert_eq!(e.end_unix_seconds - e.start_unix_seconds, 1800);
    }

    #[test]
    fn expands_weekly_recurrence_into_window() {
        // Weekly meeting that started in the past (2026-06-01, a Monday);
        // expansion must surface the upcoming occurrences.
        let body = "BEGIN:VCALENDAR\r\n\
                    BEGIN:VEVENT\r\n\
                    UID:weekly-1\r\n\
                    SUMMARY:Weekly Sync\r\n\
                    DTSTART:20260601T140000Z\r\n\
                    DTEND:20260601T143000Z\r\n\
                    RRULE:FREQ=WEEKLY;BYDAY=MO\r\n\
                    END:VEVENT\r\n\
                    END:VCALENDAR\r\n";
        // First occurrence start: 2026-06-01T14:00:00Z (a Monday).
        let first = 1_780_322_400;
        // "now" just before the series start.
        let now = first - 3600;
        let evs = parse_ics(body, &sub(), now, now + 25 * 86_400);
        // Mondays within ~25 days: Jun 1, 8, 15, 22.
        assert!(evs.len() >= 3, "expected several weekly occurrences, got {}", evs.len());
        // All exactly 7 days apart, all Mondays, all 30-min, unique ids.
        let mut starts: Vec<i64> = evs.iter().map(|e| e.start_unix_seconds).collect();
        starts.sort_unstable();
        for w in starts.windows(2) {
            assert_eq!(w[1] - w[0], 7 * 86_400, "weekly spacing");
        }
        assert!(evs.iter().all(|e| e.end_unix_seconds - e.start_unix_seconds == 1800));
        let ids: std::collections::HashSet<_> = evs.iter().map(|e| &e.id).collect();
        assert_eq!(ids.len(), evs.len(), "per-occurrence ids must be unique");
    }

    #[test]
    fn recurrence_count_and_exdate() {
        // COUNT=3 from DTSTART; EXDATE removes the 2nd. Window wide open.
        let body = "BEGIN:VCALENDAR\r\n\
                    BEGIN:VEVENT\r\n\
                    UID:daily-1\r\n\
                    SUMMARY:Daily\r\n\
                    DTSTART:20260601T090000Z\r\n\
                    DTEND:20260601T091500Z\r\n\
                    RRULE:FREQ=DAILY;COUNT=3\r\n\
                    EXDATE:20260602T090000Z\r\n\
                    END:VEVENT\r\n\
                    END:VCALENDAR\r\n";
        // 2026-06-01T09:00:00Z; "now" just before the first occurrence.
        let now = 1_780_304_400 - 3600;
        let evs = parse_ics(body, &sub(), now, now + 365 * 86_400);
        // COUNT=3 occurrences (Jun 1/2/3), EXDATE drops Jun 2 → 2 emitted.
        assert_eq!(evs.len(), 2, "COUNT=3 minus 1 EXDATE");
    }

    #[test]
    fn resolves_tzid_wall_clock_with_dst() {
        // Exchange feed: Windows zone name + VTIMEZONE with DST rules.
        // 2026-05-20 is during DST (PDT, -0700): 09:30 wall = 16:30 UTC.
        let body = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VTIMEZONE\r\n",
            "TZID:Pacific Standard Time\r\n",
            "BEGIN:STANDARD\r\n",
            "DTSTART:16010101T020000\r\n",
            "TZOFFSETFROM:-0700\r\n",
            "TZOFFSETTO:-0800\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\n",
            "END:STANDARD\r\n",
            "BEGIN:DAYLIGHT\r\n",
            "DTSTART:16010101T020000\r\n",
            "TZOFFSETFROM:-0800\r\n",
            "TZOFFSETTO:-0700\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\n",
            "END:DAYLIGHT\r\n",
            "END:VTIMEZONE\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:tz-1\r\n",
            "SUMMARY:Standup\r\n",
            "DTSTART;TZID=Pacific Standard Time:20260520T093000\r\n",
            "DTEND;TZID=Pacific Standard Time:20260520T100000\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let evs = parse_ics(body, &sub(), 0, i64::MAX);
        assert_eq!(evs.len(), 1);
        // 2026-05-20 16:30:00 UTC
        let expect = ymd_hms_to_unix("2026", "05", "20", "16", "30", "00", true).unwrap();
        assert_eq!(evs[0].start_unix_seconds, expect);
        assert_eq!(evs[0].end_unix_seconds - evs[0].start_unix_seconds, 1800);
    }

    #[test]
    fn resolves_tzid_wall_clock_standard_offset() {
        // 2026-01-15 is standard time (PST, -0800): 09:00 wall = 17:00 UTC.
        let body = concat!(
            "BEGIN:VCALENDAR\r\n",
            "BEGIN:VTIMEZONE\r\n",
            "TZID:Pacific Standard Time\r\n",
            "BEGIN:STANDARD\r\n",
            "DTSTART:16010101T020000\r\n",
            "TZOFFSETFROM:-0700\r\n",
            "TZOFFSETTO:-0800\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=1SU;BYMONTH=11\r\n",
            "END:STANDARD\r\n",
            "BEGIN:DAYLIGHT\r\n",
            "DTSTART:16010101T020000\r\n",
            "TZOFFSETFROM:-0800\r\n",
            "TZOFFSETTO:-0700\r\n",
            "RRULE:FREQ=YEARLY;INTERVAL=1;BYDAY=2SU;BYMONTH=3\r\n",
            "END:DAYLIGHT\r\n",
            "END:VTIMEZONE\r\n",
            "BEGIN:VEVENT\r\n",
            "UID:tz-2\r\n",
            "SUMMARY:Winter\r\n",
            "DTSTART;TZID=Pacific Standard Time:20260115T090000\r\n",
            "DTEND;TZID=Pacific Standard Time:20260115T093000\r\n",
            "END:VEVENT\r\n",
            "END:VCALENDAR\r\n",
        );
        let evs = parse_ics(body, &sub(), 0, i64::MAX);
        let expect = ymd_hms_to_unix("2026", "01", "15", "17", "00", "00", true).unwrap();
        assert_eq!(evs[0].start_unix_seconds, expect);
    }

    #[test]
    fn nth_weekday_known_dates() {
        // 2nd Sunday of March 2026 = March 8.
        assert_eq!(nth_weekday_of_month(2026, 3, 2, 0), Some(8));
        // 1st Sunday of November 2026 = November 1.
        assert_eq!(nth_weekday_of_month(2026, 11, 1, 0), Some(1));
        // Last Sunday of October 2026 = October 25.
        assert_eq!(nth_weekday_of_month(2026, 10, -1, 0), Some(25));
    }

    #[test]
    fn handles_line_folding_and_escapes() {
        // RFC 5545 §3.1: a CRLF followed by a single LWSP is the fold
        // marker; the LWSP is consumed. "been " ends with a space, and the
        // fold reconstructs "been folded". Rust `\` line continuations strip
        // leading whitespace on the next line and eat the fold marker; the
        // body is built with concat!.
        let body = concat!(
            "BEGIN:VEVENT\r\n",
            "UID:fold-1\r\n",
            "SUMMARY:Long title that has been \r\n",
            " folded across two lines\r\n",
            "DTSTART:20260101T000000Z\r\n",
            "DTEND:20260101T010000Z\r\n",
            "DESCRIPTION:Line one\\nLine two\\, with comma\r\n",
            "END:VEVENT\r\n",
        );
        let evs = parse_ics(body, &sub(), 0, i64::MAX);
        assert_eq!(evs.len(), 1);
        assert_eq!(evs[0].title, "Long title that has been folded across two lines");
        assert_eq!(evs[0].description.as_deref(), Some("Line one\nLine two, with comma"));
    }

    #[test]
    fn skips_event_without_uid() {
        let body = "BEGIN:VEVENT\r\nSUMMARY:no uid\r\nDTSTART:20260101T000000Z\r\nDTEND:20260101T010000Z\r\nEND:VEVENT\r\n";
        let evs = parse_ics(body, &sub(), 0, i64::MAX);
        assert!(evs.is_empty());
    }

    #[test]
    fn ymd_unix_roundtrip_known_epoch() {
        // 2000-01-01 00:00:00 UTC = 946_684_800
        assert_eq!(
            ymd_hms_to_unix("2000", "01", "01", "00", "00", "00", true),
            Some(946_684_800)
        );
        // 1970-01-01 00:00:00 = 0
        assert_eq!(
            ymd_hms_to_unix("1970", "01", "01", "00", "00", "00", true),
            Some(0)
        );
    }
}
