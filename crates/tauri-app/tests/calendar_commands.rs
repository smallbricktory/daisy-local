// SPDX-License-Identifier: Elastic-2.0
// Copyright (c) 2026 Apexis Solutions Inc. (dba Small Bricktory) — see LICENSE
//! The seven calendar commands the Calendar screen calls, driven end to end:
//! the subscription CRUD that lives in the encrypted vault, the refresh that
//! goes out over HTTP, and the upcoming window the screen reads back.
//!
//! `parse_ics` has its own unit tests in the module; nothing here re-tests the
//! parser. These drive the commands: what they persist, what they refuse, what
//! they leave in the cache, and what a locked vault does to each.
use tauri_app_core::commands::calendar::{
    add_calendar_subscription_impl, delete_calendar_subscription_impl,
    dismiss_calendar_event_impl, list_calendar_subscriptions_impl, list_upcoming_events_impl,
    refresh_calendars_impl, update_calendar_subscription_impl, AddSubscriptionRequest,
    UpcomingRequest, UpdateSubscriptionRequest,
};
use tauri_app_core::commands::lifecycle::{init_vault_impl, lock_vault_impl, unlock_vault_impl};
use tauri_app_core::error::AppError;
use tauri_app_core::now_unix;
use tauri_app_core::profile::ProfileDir;
use tauri_app_core::state::{AppState, VaultState};

const PASS: &str = "correct-horse-battery-staple-extended-22";

fn unlocked() -> (AppState, VaultState, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let profile = ProfileDir::at(dir.path().join("daisy")).unwrap();
    let app = AppState::new(profile);
    let vs = VaultState::new();
    init_vault_impl(&app, &vs, PASS).unwrap();
    (app, vs, dir)
}

fn add(app: &AppState, vs: &VaultState, name: &str, url: &str) -> String {
    add_calendar_subscription_impl(
        app,
        vs,
        AddSubscriptionRequest {
            name: name.into(),
            url: url.into(),
        },
    )
    .unwrap()
    .id
}

fn update(id: &str) -> UpdateSubscriptionRequest {
    UpdateSubscriptionRequest {
        id: id.into(),
        name: None,
        url: None,
        enabled: None,
        color_hex: None,
        tag_id: None,
    }
}

/// One VEVENT, `offset_days` from now, with a UID the caller picks.
fn ics(uid: &str, title: &str, offset_days: i64) -> String {
    let start = now_unix() + offset_days * 86_400;
    let end = start + 3600;
    let stamp = |t: i64| {
        let days = t / 86_400;
        // 1970-01-01 + days, spelled through a civil-date conversion that
        // matches the parser's own epoch handling.
        let (y, m, d) = civil_from_days(days);
        let rem = t.rem_euclid(86_400);
        format!(
            "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
            rem / 3600,
            (rem % 3600) / 60,
            rem % 60
        )
    };
    format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nBEGIN:VEVENT\r\nUID:{uid}\r\nSUMMARY:{title}\r\n\
         DTSTART:{}\r\nDTEND:{}\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        stamp(start),
        stamp(end)
    )
}

/// Howard Hinnant's days-from-civil, inverted. Used only to spell fixture
/// timestamps; the parser is what turns them back into epoch seconds.
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    (if m <= 2 { y + 1 } else { y }, m as u32, d as u32)
}

// ---- CRUD ------------------------------------------------------------------

#[test]
fn a_subscription_needs_a_name_and_a_web_url() {
    let (app, vs, _d) = unlocked();
    for (name, url) in [
        ("", "https://example.invalid/a.ics"),
        ("   ", "https://example.invalid/a.ics"),
    ] {
        let e = add_calendar_subscription_impl(
            &app,
            &vs,
            AddSubscriptionRequest {
                name: name.into(),
                url: url.into(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(e, AppError::Config(ref m) if m.contains("name")),
            "blank name should be refused, got {e:?}"
        );
    }
    for url in ["", "ftp://example.invalid/a.ics", "example.invalid/a.ics"] {
        let e = add_calendar_subscription_impl(
            &app,
            &vs,
            AddSubscriptionRequest {
                name: "Work".into(),
                url: url.into(),
            },
        )
        .unwrap_err();
        assert!(
            matches!(e, AppError::Config(ref m) if m.contains("url")),
            "{url:?} should be refused, got {e:?}"
        );
    }
    assert!(list_calendar_subscriptions_impl(&app, &vs).unwrap().is_empty());
}

#[test]
fn webcal_is_stored_as_https_and_colors_come_off_the_palette_in_order() {
    let (app, vs, _d) = unlocked();
    let sub = add_calendar_subscription_impl(
        &app,
        &vs,
        AddSubscriptionRequest {
            name: "  Work  ".into(),
            url: "  webcal://example.invalid/work.ics  ".into(),
        },
    )
    .unwrap();
    assert_eq!(sub.name, "Work", "name is trimmed");
    assert_eq!(sub.url, "https://example.invalid/work.ics");
    assert!(sub.enabled, "a new subscription is on");
    assert_eq!(sub.color_hex, "#5BA3D0", "first palette entry");

    let second = add_calendar_subscription_impl(
        &app,
        &vs,
        AddSubscriptionRequest {
            name: "Personal".into(),
            url: "https://example.invalid/home.ics".into(),
        },
    )
    .unwrap();
    assert_eq!(second.color_hex, "#E07A5F", "second palette entry");
    assert_ne!(sub.id, second.id);

    // The returned row is the row that was stored, colour included.
    let stored = list_calendar_subscriptions_impl(&app, &vs).unwrap();
    assert_eq!(stored.len(), 2);
    assert_eq!(stored[0], sub);
    assert_eq!(stored[1], second);
}

#[test]
fn subscriptions_survive_a_lock_and_unlock() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Work", "https://example.invalid/work.ics");
    lock_vault_impl(&app, &vs).unwrap();
    assert!(
        matches!(
            list_calendar_subscriptions_impl(&app, &vs).unwrap_err(),
            AppError::VaultLocked
        ),
        "a locked vault has no subscriptions to list"
    );
    unlock_vault_impl(&app, &vs, PASS).unwrap();
    let back = list_calendar_subscriptions_impl(&app, &vs).unwrap();
    assert_eq!(back.len(), 1);
    assert_eq!(back[0].id, id);
    assert_eq!(back[0].url, "https://example.invalid/work.ics");
}

#[test]
fn every_calendar_command_is_refused_while_the_vault_is_locked() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Work", "https://example.invalid/work.ics");
    lock_vault_impl(&app, &vs).unwrap();

    let locked = |e: AppError| matches!(e, AppError::VaultLocked);
    assert!(locked(
        list_calendar_subscriptions_impl(&app, &vs).unwrap_err()
    ));
    assert!(locked(
        add_calendar_subscription_impl(
            &app,
            &vs,
            AddSubscriptionRequest {
                name: "X".into(),
                url: "https://example.invalid/x.ics".into()
            }
        )
        .unwrap_err()
    ));
    assert!(locked(
        update_calendar_subscription_impl(&app, &vs, update(&id)).unwrap_err()
    ));
    assert!(locked(
        delete_calendar_subscription_impl(&app, &vs, &id).unwrap_err()
    ));
    assert!(locked(
        dismiss_calendar_event_impl(&app, &vs, &id, "uid-1").unwrap_err()
    ));
    assert!(locked(refresh_calendars_impl(&app, &vs).unwrap_err()));
    // The upcoming list is the one read that answers rather than erring: the
    // screen shows an empty calendar, not an error, behind the lock screen.
    assert!(list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
        .unwrap()
        .is_empty());
}

#[test]
fn an_update_changes_only_the_fields_it_carries() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Work", "https://example.invalid/work.ics");

    let renamed = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            name: Some("Work calendar".into()),
            ..update(&id)
        },
    )
    .unwrap();
    assert_eq!(renamed.name, "Work calendar");
    assert_eq!(renamed.url, "https://example.invalid/work.ics", "url untouched");
    assert!(renamed.enabled);
    assert_eq!(renamed.color_hex, "#5BA3D0", "colour untouched");

    // A blank name is not a rename to nothing — it is left alone.
    let blank = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            name: Some("   ".into()),
            url: Some("".into()),
            color_hex: Some("  ".into()),
            ..update(&id)
        },
    )
    .unwrap();
    assert_eq!(blank.name, "Work calendar");
    assert_eq!(blank.url, "https://example.invalid/work.ics");
    assert_eq!(blank.color_hex, "#5BA3D0");

    let off = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            enabled: Some(false),
            url: Some("webcal://example.invalid/moved.ics".into()),
            color_hex: Some("#123456".into()),
            ..update(&id)
        },
    )
    .unwrap();
    assert!(!off.enabled);
    assert_eq!(off.url, "https://example.invalid/moved.ics", "webcal normalized on update too");
    assert_eq!(off.color_hex, "#123456");
}

#[test]
fn an_empty_tag_id_clears_the_link_and_an_absent_one_leaves_it() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Work", "https://example.invalid/work.ics");

    let linked = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            tag_id: Some("tag-abc".into()),
            ..update(&id)
        },
    )
    .unwrap();
    assert_eq!(linked.tag_id.as_deref(), Some("tag-abc"));

    let renamed = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            name: Some("Work!".into()),
            ..update(&id)
        },
    )
    .unwrap();
    assert_eq!(renamed.tag_id.as_deref(), Some("tag-abc"), "absent leaves it");

    let cleared = update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            tag_id: Some(String::new()),
            ..update(&id)
        },
    )
    .unwrap();
    assert_eq!(cleared.tag_id, None, "empty clears it");
}

#[test]
fn updating_or_deleting_a_subscription_that_is_not_there_says_so() {
    let (app, vs, _d) = unlocked();
    add(&app, &vs, "Work", "https://example.invalid/work.ics");
    let e = update_calendar_subscription_impl(&app, &vs, update("no-such-id")).unwrap_err();
    assert!(matches!(e, AppError::Config(ref m) if m.contains("no-such-id")), "{e:?}");
    let e = delete_calendar_subscription_impl(&app, &vs, "no-such-id").unwrap_err();
    assert!(matches!(e, AppError::Config(ref m) if m.contains("no-such-id")), "{e:?}");
    assert_eq!(
        list_calendar_subscriptions_impl(&app, &vs).unwrap().len(),
        1,
        "a refused delete leaves the list alone"
    );
}

// ---- refresh ---------------------------------------------------------------

#[test]
fn a_refresh_caches_the_feeds_events_and_the_upcoming_window_reads_them_back() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/work.ics")
        .with_status(200)
        .with_body(ics("uid-standup", "Standup", 1))
        .create();
    add(&app, &vs, "Work", &format!("{}/work.ics", server.url()));

    let res = refresh_calendars_impl(&app, &vs).unwrap();
    m.assert();
    assert_eq!(res.subscriptions_scanned, 1);
    assert_eq!(res.events_loaded, 1);
    assert!(res.errors.is_empty(), "{:?}", res.errors);

    let up = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None }).unwrap();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].title, "Standup");
    assert_eq!(up[0].uid, "uid-standup");
    assert_eq!(up[0].subscription_name, "Work");
    assert_eq!(up[0].subscription_color, "#5BA3D0", "the sub's colour rides along");
}

#[test]
fn a_disabled_subscription_is_not_fetched() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    // `expect(0)` is the assertion: a disabled feed must not be reached.
    let m = server
        .mock("GET", "/off.ics")
        .with_status(200)
        .with_body(ics("uid-off", "Off", 1))
        .expect(0)
        .create();
    let id = add(&app, &vs, "Off", &format!("{}/off.ics", server.url()));
    update_calendar_subscription_impl(
        &app,
        &vs,
        UpdateSubscriptionRequest {
            enabled: Some(false),
            ..update(&id)
        },
    )
    .unwrap();

    let res = refresh_calendars_impl(&app, &vs).unwrap();
    m.assert();
    assert_eq!(res.subscriptions_scanned, 0);
    assert_eq!(res.events_loaded, 0);
}

#[test]
fn one_bad_feed_does_not_stop_the_sweep_and_each_failure_says_what_it_was() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    let good = server
        .mock("GET", "/good.ics")
        .with_status(200)
        .with_body(ics("uid-good", "Good", 1))
        .create();
    let five_hundred = server.mock("GET", "/boom.ics").with_status(500).create();
    let html = server
        .mock("GET", "/login.ics")
        .with_status(200)
        .with_body("<html><body>Sign in</body></html>")
        .create();
    let empty = server
        .mock("GET", "/empty.ics")
        .with_status(200)
        .with_body("")
        .create();

    add(&app, &vs, "Good", &format!("{}/good.ics", server.url()));
    add(&app, &vs, "Boom", &format!("{}/boom.ics", server.url()));
    add(&app, &vs, "Login", &format!("{}/login.ics", server.url()));
    add(&app, &vs, "Empty", &format!("{}/empty.ics", server.url()));

    let res = refresh_calendars_impl(&app, &vs).unwrap();
    good.assert();
    five_hundred.assert();
    html.assert();
    empty.assert();

    assert_eq!(res.subscriptions_scanned, 4);
    assert_eq!(res.events_loaded, 1, "the good feed still landed");
    assert_eq!(res.errors.len(), 3, "{:?}", res.errors);
    let joined = res.errors.join("\n");
    assert!(joined.contains("Boom: HTTP 500"), "{joined}");
    assert!(
        joined.contains("Login:") && joined.contains("wasn't an ICS feed"),
        "an auth page is named as one: {joined}"
    );
    assert!(
        joined.contains("Empty:") && joined.contains("empty response"),
        "an empty body is named as one: {joined}"
    );
    // The good feed's event is what the screen gets.
    let up = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None }).unwrap();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].title, "Good");
}

#[test]
fn a_feed_that_parses_to_nothing_is_reported_rather_than_passing_silently() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    let m = server
        .mock("GET", "/bare.ics")
        .with_status(200)
        .with_body("BEGIN:VCALENDAR\r\nVERSION:2.0\r\nEND:VCALENDAR\r\n")
        .create();
    add(&app, &vs, "Bare", &format!("{}/bare.ics", server.url()));
    let res = refresh_calendars_impl(&app, &vs).unwrap();
    m.assert();
    assert_eq!(res.events_loaded, 0);
    assert_eq!(res.errors.len(), 1);
    assert!(res.errors[0].contains("no events"), "{:?}", res.errors);
}

#[test]
fn an_event_that_ended_more_than_a_month_ago_is_not_kept() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    let body = format!(
        "{}{}",
        ics("uid-old", "Ancient", -40).trim_end_matches("END:VCALENDAR\r\n"),
        ics("uid-soon", "Soon", 2)
            .trim_start_matches("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n")
    );
    let m = server
        .mock("GET", "/mixed.ics")
        .with_status(200)
        .with_body(body)
        .create();
    add(&app, &vs, "Mixed", &format!("{}/mixed.ics", server.url()));
    let res = refresh_calendars_impl(&app, &vs).unwrap();
    m.assert();
    assert_eq!(res.events_loaded, 1, "only the future one is cached");
    let up = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: Some(60) }).unwrap();
    assert_eq!(up.len(), 1);
    assert_eq!(up[0].title, "Soon");
}

#[test]
fn a_refresh_replaces_the_cache_rather_than_adding_to_it() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    let first = server
        .mock("GET", "/feed.ics")
        .with_status(200)
        .with_body(ics("uid-a", "First", 1))
        .create();
    add(&app, &vs, "Feed", &format!("{}/feed.ics", server.url()));
    refresh_calendars_impl(&app, &vs).unwrap();
    first.assert();
    assert_eq!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .len(),
        1
    );

    let second = server
        .mock("GET", "/feed.ics")
        .with_status(200)
        .with_body(ics("uid-b", "Second", 1))
        .create();
    refresh_calendars_impl(&app, &vs).unwrap();
    second.assert();
    let up = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None }).unwrap();
    assert_eq!(up.len(), 1, "the removed event is gone, not merged");
    assert_eq!(up[0].title, "Second");
}

// ---- dismissal -------------------------------------------------------------

#[test]
fn dismissing_an_event_evicts_it_now_and_keeps_it_out_of_the_next_refresh() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/feed.ics")
        .with_status(200)
        .with_body(ics("uid-standup", "Standup", 1))
        .expect_at_least(2)
        .create();
    let id = add(&app, &vs, "Feed", &format!("{}/feed.ics", server.url()));
    refresh_calendars_impl(&app, &vs).unwrap();
    assert_eq!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .len(),
        1
    );

    dismiss_calendar_event_impl(&app, &vs, &id, "uid-standup").unwrap();
    assert!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .is_empty(),
        "dismissal evicts from the live cache at once"
    );
    let sub = list_calendar_subscriptions_impl(&app, &vs).unwrap().remove(0);
    assert_eq!(sub.dismissed_event_uids, vec!["uid-standup".to_string()]);

    // And the feed still serving it does not bring it back.
    let res = refresh_calendars_impl(&app, &vs).unwrap();
    assert_eq!(res.events_loaded, 0);
    assert!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .is_empty()
    );
}

#[test]
fn dismissing_twice_records_one_uid() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Feed", "https://example.invalid/f.ics");
    dismiss_calendar_event_impl(&app, &vs, &id, "uid-1").unwrap();
    dismiss_calendar_event_impl(&app, &vs, &id, "uid-1").unwrap();
    let sub = list_calendar_subscriptions_impl(&app, &vs).unwrap().remove(0);
    assert_eq!(sub.dismissed_event_uids, vec!["uid-1".to_string()]);
}

#[test]
fn the_dismissal_list_is_capped_at_a_thousand_and_drops_the_oldest() {
    let (app, vs, _d) = unlocked();
    let id = add(&app, &vs, "Feed", "https://example.invalid/f.ics");
    // Seeded to the brim in memory rather than through a thousand commands:
    // each dismissal re-encrypts the vault, and an Argon2id run apiece would
    // put this test in the minutes. The last two go through the command,
    // which is the code under test.
    {
        let mut g = vs.keys.lock().unwrap();
        let subs = &mut g.as_mut().unwrap().calendar_subscriptions;
        let sub = subs.iter_mut().find(|c| c.id == id).unwrap();
        sub.dismissed_event_uids = (0..1_000).map(|i| format!("uid-{i}")).collect();
    }
    dismiss_calendar_event_impl(&app, &vs, &id, "uid-1000").unwrap();
    dismiss_calendar_event_impl(&app, &vs, &id, "uid-1001").unwrap();

    let sub = list_calendar_subscriptions_impl(&app, &vs).unwrap().remove(0);
    assert_eq!(sub.dismissed_event_uids.len(), 1_000, "the cap holds");
    assert_eq!(
        sub.dismissed_event_uids.first().unwrap(),
        "uid-2",
        "the two oldest went to make room"
    );
    assert_eq!(sub.dismissed_event_uids.last().unwrap(), "uid-1001");
}

#[test]
fn dismissing_against_a_subscription_that_is_not_there_says_so() {
    let (app, vs, _d) = unlocked();
    let e = dismiss_calendar_event_impl(&app, &vs, "no-such-id", "uid-1").unwrap_err();
    assert!(matches!(e, AppError::Config(ref m) if m.contains("no-such-id")), "{e:?}");
}

// ---- delete ----------------------------------------------------------------

#[test]
fn deleting_a_subscription_takes_its_cached_events_and_leaves_the_others() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/a.ics")
        .with_status(200)
        .with_body(ics("uid-a", "Ay", 1))
        .create();
    server
        .mock("GET", "/b.ics")
        .with_status(200)
        .with_body(ics("uid-b", "Bee", 1))
        .create();
    let a = add(&app, &vs, "Ay", &format!("{}/a.ics", server.url()));
    add(&app, &vs, "Bee", &format!("{}/b.ics", server.url()));
    refresh_calendars_impl(&app, &vs).unwrap();
    assert_eq!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .len(),
        2
    );

    delete_calendar_subscription_impl(&app, &vs, &a).unwrap();
    let subs = list_calendar_subscriptions_impl(&app, &vs).unwrap();
    assert_eq!(subs.len(), 1);
    assert_eq!(subs[0].name, "Bee");
    let up = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None }).unwrap();
    assert_eq!(up.len(), 1, "only the deleted feed's events are purged");
    assert_eq!(up[0].title, "Bee");
}

// ---- the upcoming window ---------------------------------------------------

#[test]
fn the_upcoming_window_is_clamped_and_reaches_back_one_day() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    // Yesterday (still inside the one-day floor), tomorrow, and in fifty days.
    let parts = [
        ics("uid-yday", "Yesterday", -1),
        ics("uid-tmrw", "Tomorrow", 1),
        ics("uid-far", "Far", 50),
    ];
    let body = format!(
        "BEGIN:VCALENDAR\r\nVERSION:2.0\r\n{}END:VCALENDAR\r\n",
        parts
            .iter()
            .map(|p| p
                .trim_start_matches("BEGIN:VCALENDAR\r\nVERSION:2.0\r\n")
                .trim_end_matches("END:VCALENDAR\r\n")
                .to_string())
            .collect::<String>()
    );
    server
        .mock("GET", "/w.ics")
        .with_status(200)
        .with_body(body)
        .create();
    add(&app, &vs, "W", &format!("{}/w.ics", server.url()));
    let res = refresh_calendars_impl(&app, &vs).unwrap();
    assert_eq!(res.events_loaded, 3, "{:?}", res.errors);

    let titles = |days: Option<u32>| {
        let mut t: Vec<String> = list_upcoming_events_impl(&app, &vs, UpcomingRequest { days })
            .unwrap()
            .into_iter()
            .map(|e| e.title)
            .collect();
        t.sort();
        t
    };
    assert_eq!(
        titles(Some(7)),
        vec!["Tomorrow".to_string(), "Yesterday".to_string()],
        "a week reaches back one day and forward seven"
    );
    assert_eq!(titles(None), titles(Some(7)), "seven days is the default");
    // Zero clamps up to one day, which still includes yesterday's floor.
    assert_eq!(
        titles(Some(0)),
        vec!["Tomorrow".to_string(), "Yesterday".to_string()]
    );
    // Anything past sixty clamps down to sixty, which reaches the far event.
    assert_eq!(titles(Some(9_000)).len(), 3);
    assert_eq!(titles(Some(60)).len(), 3);
}

#[test]
fn the_upcoming_window_is_empty_before_the_first_refresh() {
    let (app, vs, _d) = unlocked();
    add(&app, &vs, "Work", "https://example.invalid/work.ics");
    assert!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()
            .is_empty(),
        "a subscription with no fetch behind it shows nothing rather than erring"
    );
}

#[test]
fn the_cached_events_are_encrypted_on_disk() {
    let (app, vs, _d) = unlocked();
    let mut server = mockito::Server::new();
    server
        .mock("GET", "/secret.ics")
        .with_status(200)
        .with_body(ics("uid-secret", "Board strategy", 1))
        .create();
    add(&app, &vs, "Secret", &format!("{}/secret.ics", server.url()));
    refresh_calendars_impl(&app, &vs).unwrap();

    let path = app.profile.root().join("calendar").join("events.json");
    let bytes = std::fs::read(&path).unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(
        !text.contains("Board strategy") && !text.contains("uid-secret"),
        "the cache on disk must not carry event titles in the clear"
    );
    // It is an envelope, and it is what the unlocked read gives back.
    assert!(serde_json::from_slice::<vault::Envelope>(&bytes).is_ok());
    assert_eq!(
        list_upcoming_events_impl(&app, &vs, UpcomingRequest { days: None })
            .unwrap()[0]
            .title,
        "Board strategy"
    );
}
