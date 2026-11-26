mod common;

use common::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use sync_engine::*;

#[test]
fn two_devices_converge() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let b = Device::new("b", "bob", &s, 2);
    let id = a.insert_item("team:1", "hello", 1);
    assert_eq!(a.unsent(), 1);
    let r = a.sync();
    assert_eq!(r.pushed, 1);
    assert_eq!(a.unsent(), 0);
    b.sync();
    assert_eq!(b.dump("team:1"), a.dump("team:1"));
    b.exec(
        "UPDATE items SET qty = 5 WHERE id = ?1",
        &[id.clone().into()],
    );
    b.sync();
    a.sync();
    assert_eq!(
        a.dump("team:1")[&("items".into(), id)],
        json!({"title": "hello", "qty": 5, "note": null})
    );
    assert_eq!(a.dump("team:1"), server_dump(&s, "team:1"));
}

#[test]
fn edits_to_different_fields_merge() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let b = Device::new("b", "bob", &s, 2);
    let id = a.insert_item("team:1", "base", 0);
    a.sync();
    b.sync();
    a.exec(
        "UPDATE items SET title = 'from a' WHERE id = ?1",
        &[id.clone().into()],
    );
    b.exec(
        "UPDATE items SET qty = 9 WHERE id = ?1",
        &[id.clone().into()],
    );
    a.sync();
    b.sync();
    a.sync();
    let expected = json!({"title": "from a", "qty": 9, "note": null});
    assert_eq!(a.dump("team:1")[&("items".into(), id.clone())], expected);
    assert_eq!(b.dump("team:1")[&("items".into(), id.clone())], expected);
    assert_eq!(server_dump(&s, "team:1")[&("items".into(), id)], expected);
}

#[test]
fn server_wins_table_reports_conflict_and_takes_server_state() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let b = Device::new("b", "bob", &s, 2);
    let rec = Arc::new(Recorder::default());
    b.e().add_listener(rec.clone());
    let id = a.e().new_id();
    a.exec(
        "INSERT INTO locked (id, scope_id, title, qty) VALUES (?1, 'team:1', 'base', 0)",
        &[id.clone().into()],
    );
    a.sync();
    b.sync();
    a.exec(
        "UPDATE locked SET title = 'a wins' WHERE id = ?1",
        &[id.clone().into()],
    );
    b.exec(
        "UPDATE locked SET title = 'b loses' WHERE id = ?1",
        &[id.clone().into()],
    );
    a.sync();
    let r = b.sync();
    assert_eq!(r.conflicts, 1);
    assert_eq!(b.unsent(), 0);
    assert_eq!(
        b.dump("team:1")[&("locked".into(), id.clone())]["title"],
        "a wins"
    );
    assert!(rec
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, SyncEvent::ChangeConflicted { reason, .. } if reason == "stale")));
}

#[test]
fn server_wins_conflict_noticed_on_pull_first() {
    // B's edit is still unsent when the newer server version arrives.
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.e().new_id();
    a.exec(
        "INSERT INTO locked (id, scope_id, title, qty) VALUES (?1, 'team:1', 'base', 0)",
        &[id.clone().into()],
    );
    a.sync();
    admin_write(&s, "team:1", "locked", &id, Some(json!({"title": "admin"})));
    a.exec(
        "UPDATE locked SET qty = 1 WHERE id = ?1",
        &[id.clone().into()],
    );
    // Push first: the server refuses the stale change.
    let r = a.sync();
    assert_eq!(r.conflicts, 1);
    assert_eq!(
        a.dump("team:1")[&("locked".into(), id)],
        json!({"title": "admin", "qty": 0})
    );
}

#[test]
fn unsent_edit_survives_a_pull_of_newer_data() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "t", 1);
    a.sync();
    admin_write(&s, "team:1", "items", &id, Some(json!({"qty": 50})));
    a.exec(
        "UPDATE items SET note = 'mine' WHERE id = ?1",
        &[id.clone().into()],
    );
    // Simulate the pull landing before the push by losing the push.
    *a.transport.intercept.lock().unwrap() = Some(Box::new(|req| {
        (req.method == sync_core::http::Method::Post)
            .then(|| sync_core::http::HttpResponse::json(503, &json!({"error": {"code": "busy"}})))
    }));
    assert!(a.e().sync_now().is_err());
    *a.transport.intercept.lock().unwrap() = None;
    let r = a.sync();
    assert_eq!(r.pushed, 1);
    let expected = json!({"title": "t", "qty": 50, "note": "mine"});
    assert_eq!(a.dump("team:1")[&("items".into(), id.clone())], expected);
    assert_eq!(server_dump(&s, "team:1")[&("items".into(), id)], expected);
}

#[test]
fn pulled_record_meets_unsent_edit_field_merge() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "t", 1);
    a.sync();
    admin_write(&s, "team:1", "items", &id, Some(json!({"qty": 2})));
    write_during_pull(
        &a,
        "UPDATE items SET note = 'local' WHERE id = ?1",
        vec![id.clone().into()],
    );
    a.sync();
    let expected = json!({"title": "t", "qty": 2, "note": "local"});
    assert_eq!(
        a.dump("team:1")[&("items".into(), id.clone())],
        expected,
        "server fields taken, local edit kept"
    );
    assert_eq!(a.unsent(), 1, "the local edit is still to be sent");
    a.sync();
    assert_eq!(server_dump(&s, "team:1")[&("items".into(), id)], expected);
}

#[test]
fn pulled_record_meets_unsent_edit_server_wins() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    let id = a.e().new_id();
    a.exec(
        "INSERT INTO locked (id, scope_id, title, qty) VALUES (?1, 'team:1', 'base', 0)",
        &[id.clone().into()],
    );
    a.sync();
    admin_write(&s, "team:1", "locked", &id, Some(json!({"title": "admin"})));
    write_during_pull(
        &a,
        "UPDATE locked SET qty = 5 WHERE id = ?1",
        vec![id.clone().into()],
    );
    let r = a.sync();
    assert_eq!(r.conflicts, 1);
    assert_eq!(a.unsent(), 0, "the stale edit is dropped");
    assert_eq!(
        a.dump("team:1")[&("locked".into(), id)],
        json!({"title": "admin", "qty": 0})
    );
    assert!(rec
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| matches!(e, SyncEvent::ChangeConflicted { .. })));
}

#[test]
fn delete_wins_over_a_concurrent_edit() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let b = Device::new("b", "bob", &s, 2);
    let id = a.insert_item("team:1", "doomed", 1);
    a.sync();
    b.sync();
    a.exec("DELETE FROM items WHERE id = ?1", &[id.clone().into()]);
    b.exec(
        "UPDATE items SET qty = 2 WHERE id = ?1",
        &[id.clone().into()],
    );
    a.sync();
    let r = b.sync();
    assert_eq!(r.conflicts, 1);
    assert!(!b.dump("team:1").contains_key(&("items".into(), id.clone())));
    assert!(!server_dump(&s, "team:1").contains_key(&("items".into(), id)));
    assert_eq!(b.unsent(), 0);
}

#[test]
fn local_create_then_delete_never_reaches_the_server() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "temp", 1);
    a.exec("DELETE FROM items WHERE id = ?1", &[id.into()]);
    let r = a.sync();
    assert_eq!(r.pushed, 0);
    assert_eq!(a.unsent(), 0);
    assert!(server_dump(&s, "team:1").is_empty());
}

#[test]
fn lost_response_is_resent_and_not_applied_twice() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "once", 1);
    a.transport.set_faults(Faults {
        drop_push_response: 1.0,
        ..Faults::default()
    });
    assert!(a.e().sync_now().is_err());
    assert_eq!(a.unsent(), 1, "the change stays until its result is known");
    let v1 = server_dump(&s, "team:1").len();
    assert_eq!(v1, 1, "the server did apply it");
    // Meanwhile another device changes the record.
    let b = Device::new("b", "bob", &s, 2);
    b.sync();
    b.exec(
        "UPDATE items SET qty = 7 WHERE id = ?1",
        &[id.clone().into()],
    );
    b.sync();
    a.transport.set_faults(Faults::default());
    a.sync();
    assert_eq!(a.unsent(), 0);
    assert_eq!(
        a.dump("team:1")[&("items".into(), id)]["qty"],
        7,
        "the resend did not undo B's change"
    );
}

#[test]
fn every_crash_point_recovers() {
    for point in [
        FaultPoint::BatchRecorded,
        FaultPoint::PushAnswered,
        FaultPoint::PushApplied,
        FaultPoint::PageReceived,
        FaultPoint::PageApplied,
    ] {
        let s = server();
        let b = Device::new("b", "bob", &s, 2);
        for i in 0..10 {
            b.insert_item("team:1", &format!("from b {i}"), i);
        }
        b.sync();
        let mut a = Device::new("a", "alice", &s, 1);
        for i in 0..20 {
            a.insert_item("team:1", &format!("from a {i}"), i);
        }
        *a.crash.point.lock().unwrap() = Some(point);
        match a.e().sync_now() {
            Err(EngineError::Crashed(p)) => assert_eq!(p, point),
            other => panic!("{point:?}: expected a crash, got {other:?}"),
        }
        a.restart();
        a.sync();
        b.sync();
        assert_eq!(a.unsent(), 0, "{point:?}");
        let server_state = server_dump(&s, "team:1");
        assert_eq!(server_state.len(), 30, "{point:?}");
        assert_eq!(a.dump("team:1"), server_state, "{point:?}");
        assert_eq!(b.dump("team:1"), server_state, "{point:?}");
    }
}

#[test]
fn local_only_columns_are_kept() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "t", 1);
    a.exec(
        "UPDATE items SET local_note = 'private' WHERE id = ?1",
        &[id.clone().into()],
    );
    let r = a.sync();
    assert_eq!(r.pushed, 1, "a local-only change adds nothing to push");
    admin_write(
        &s,
        "team:1",
        "items",
        &id,
        Some(json!({"title": "renamed"})),
    );
    a.sync();
    let r = a
        .e()
        .query(
            "SELECT title, local_note FROM items WHERE id = ?1",
            &[id.into()],
        )
        .unwrap();
    assert_eq!(
        r.rows[0],
        vec![
            SqlValue::Text("renamed".into()),
            SqlValue::Text("private".into())
        ]
    );
}

#[test]
fn default_scope_tables_sync() {
    let s = server();
    let a1 = Device::new("a1", "alice", &s, 1);
    let a2 = Device::new("a2", "alice", &s, 2);
    a1.exec("INSERT INTO prefs (id, theme) VALUES ('me', 'dark')", &[]);
    a1.sync();
    a2.sync();
    assert_eq!(a2.dump("user:alice"), a1.dump("user:alice"));
    assert_eq!(server_dump(&s, "user:alice").len(), 1);
}

#[test]
fn watch_reruns_on_local_and_remote_changes() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let seen: Arc<Mutex<Vec<usize>>> = Arc::default();
    let seen2 = seen.clone();
    a.e()
        .watch(
            "SELECT id FROM items WHERE scope_id = ?1",
            vec!["team:1".into()],
            move |r| {
                seen2.lock().unwrap().push(r.unwrap().rows.len());
            },
        )
        .unwrap();
    a.insert_item("team:1", "local", 1);
    a.exec("UPDATE prefs SET theme = 'x'", &[]);
    admin_write(
        &s,
        "team:1",
        "items",
        "remote-1",
        Some(json!({"title": "remote"})),
    );
    a.sync();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![0, 1, 2],
        "initial, local insert, pulled record; not the prefs write"
    );
}

#[test]
fn listener_sees_status_and_pending_count() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    a.insert_item("team:1", "x", 1);
    a.sync();
    let st = rec.statuses.lock().unwrap();
    assert!(st.iter().any(|s| s.pending_changes == 1));
    assert!(st.iter().any(|s| s.phase == Phase::Syncing));
    let last = st.last().unwrap();
    assert_eq!(last.phase, Phase::Idle);
    assert_eq!(last.pending_changes, 0);
    assert!(last.last_sync_at.is_some());
}

#[test]
fn offline_sets_offline_phase_and_keeps_work() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    a.insert_item("team:1", "x", 1);
    a.transport.set_faults(Faults {
        offline: true,
        ..Faults::default()
    });
    assert!(matches!(a.e().sync_now(), Err(EngineError::Network(_))));
    assert_eq!(a.e().status().phase, Phase::Offline);
    assert_eq!(a.unsent(), 1);
    a.transport.set_faults(Faults::default());
    a.sync();
    assert_eq!(a.e().status().phase, Phase::Idle);
    assert_eq!(server_dump(&s, "team:1").len(), 1);
}

#[test]
fn concurrent_sync_requests_coalesce() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    for i in 0..50 {
        a.insert_item("team:1", "x", i);
    }
    let e1 = a.e().clone();
    let e2 = a.e().clone();
    let t1 = std::thread::spawn(move || e1.sync_now().unwrap());
    let t2 = std::thread::spawn(move || e2.sync_now().unwrap());
    let (r1, r2) = (t1.join().unwrap(), t2.join().unwrap());
    assert!(r1.coalesced || r2.coalesced || r1.cycles + r2.cycles >= 2);
    a.sync();
    assert_eq!(server_dump(&s, "team:1").len(), 50);
    assert_eq!(a.unsent(), 0);
}

#[test]
fn own_edit_made_during_a_push_is_not_a_conflict() {
    // On a server_wins table, an edit made while the previous edit of the
    // same record is being uploaded must be based on the version that
    // upload produces, or the server would refuse it as stale.
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.e().new_id();
    a.exec(
        "INSERT INTO locked (id, scope_id, title, qty) VALUES (?1, 'team:1', 'base', 0)",
        &[id.clone().into()],
    );
    a.sync();
    a.exec(
        "UPDATE locked SET title = 'first' WHERE id = ?1",
        &[id.clone().into()],
    );
    let engine = a.e().clone();
    let id2 = id.clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *a.transport.intercept.lock().unwrap() = Some(Box::new(move |req| {
        if req.method == sync_core::http::Method::Post
            && !done.swap(true, std::sync::atomic::Ordering::SeqCst)
        {
            engine
                .execute(
                    "UPDATE locked SET qty = 7 WHERE id = ?1",
                    &[id2.clone().into()],
                )
                .unwrap();
        }
        None
    }));
    let r = a.sync();
    assert_eq!(r.conflicts, 0, "{r:?}");
    assert_eq!(r.pushed, 2);
    assert_eq!(
        server_dump(&s, "team:1")[&("locked".into(), id)],
        json!({"title": "first", "qty": 7})
    );
}

#[test]
fn first_download_after_own_upload_still_learns_of_deletions() {
    // Found by the long convergence run. The device uploads a record, its
    // first download fails, another device deletes the record, and the
    // download from the beginning carries no tombstones.
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "short-lived", 1);
    *a.transport.intercept.lock().unwrap() = Some(Box::new(|req| {
        (req.method == sync_core::http::Method::Get && req.path.ends_with("/changes"))
            .then(|| sync_core::http::HttpResponse::json(503, &json!({"error": {"code": "busy"}})))
    }));
    assert!(a.e().sync_now().is_err());
    assert_eq!(
        server_dump(&s, "team:1").len(),
        1,
        "the upload went through"
    );
    *a.transport.intercept.lock().unwrap() = None;

    let b = Device::new("b", "bob", &s, 2);
    b.sync();
    b.exec("DELETE FROM items WHERE id = ?1", &[id.clone().into()]);
    b.sync();

    a.sync();
    assert!(!a.dump("team:1").contains_key(&("items".into(), id)));
    assert_eq!(a.dump("team:1"), server_dump(&s, "team:1"));
}
