//! Phase 5: one test per row of the failure table in
//! `docs/04-client-engine.md` section 4.8, plus scope changes, sign-out,
//! migrations with unsent work, and oversized batches.
//!
//! Rows already covered elsewhere:
//! - "During a local write": `sync-store` tests `batch_writes_are_atomic`.
//! - "After sending a push, before the response", "While applying push
//!   results", "While applying a pull page": `engine.rs`
//!   `every_crash_point_recovers` and `lost_response_is_resent_and_not_applied_twice`.
//! - "Network down": `engine.rs` `offline_sets_offline_phase_and_keeps_work`.

mod common;

use common::*;
use serde_json::json;
use std::sync::{Arc, Mutex};
use sync_core::http::{HttpResponse, Method};
use sync_engine::*;
use sync_refserver::RefServer;

// ------------------------------------------------------------------ token expired

struct RotatingToken {
    calls: Mutex<Vec<bool>>,
}

impl TokenProvider for RotatingToken {
    fn token(&self, force_refresh: bool) -> Result<String, String> {
        self.calls.lock().unwrap().push(force_refresh);
        // The cached token has expired; a refresh yields a valid one.
        Ok(if force_refresh {
            "alice".into()
        } else {
            "expired".into()
        })
    }
}

#[test]
fn expired_token_is_refreshed_once_and_sync_continues() {
    let s = server();
    let mut a = Device::new("a", "alice", &s, 1);
    let tokens = Arc::new(RotatingToken {
        calls: Mutex::new(vec![]),
    });
    a.tokens = Some(tokens.clone());
    a.restart();
    a.insert_item("team:1", "x", 1);
    a.sync();
    assert_eq!(server_dump(&s, "team:1").len(), 1);
    assert!(tokens.calls.lock().unwrap().contains(&true));
}

#[test]
fn refused_token_reports_unauthorized() {
    let s = server();
    let a = Device::new("a", "nobody", &s, 1);
    assert!(matches!(a.e().sync_now(), Err(EngineError::Unauthorized)));
    assert_eq!(a.e().status().phase, Phase::Error);
}

// ------------------------------------------------------------------ upgrade required

#[test]
fn upgrade_required_stops_sync_keeps_work_and_resumes_after_update() {
    let mut cfg = server_config();
    cfg.schema_version = 2;
    cfg.min_schema_version = 2;
    let s = Arc::new(RefServer::new(cfg).unwrap());
    let mut a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    a.insert_item("team:1", "written on the old app", 1);

    match a.e().sync_now() {
        Err(EngineError::UpgradeRequired { min_schema_version }) => {
            assert_eq!(min_schema_version, 2)
        }
        other => panic!("expected upgrade required, got {other:?}"),
    }
    assert_eq!(a.e().status().phase, Phase::UpgradeRequired);
    assert_eq!(a.unsent(), 1, "the outbox is kept");
    assert!(events(&rec).contains(&SyncEvent::UpgradeRequired {
        min_schema_version: 2
    }));

    // The user installs the new app version.
    a.manifest.schema_version = 2;
    a.restart();
    a.sync();
    assert_eq!(a.unsent(), 0);
    assert_eq!(server_dump(&s, "team:1").len(), 1);
}

#[test]
fn server_older_than_app_is_retried_later() {
    let s = server();
    let mut a = Device::new("a", "alice", &s, 1);
    a.manifest.schema_version = 5;
    a.restart();
    assert!(matches!(a.e().sync_now(), Err(EngineError::SchemaAhead)));
}

// ------------------------------------------------------------------ cursor expired

#[test]
fn expired_cursor_resyncs_and_keeps_unsent_rows() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    let keep = a.insert_item("team:1", "keep", 1);
    let gone = a.insert_item("team:1", "gone", 2);
    a.sync();

    // Someone else deletes a record, and the server prunes the tombstone
    // before this device hears about it.
    admin_write(&s, "team:1", "items", &gone, None);
    hook(&s, "prune", json!({}));

    // A row created while the resync is under way must survive it.
    let fresh = a.e().new_id();
    write_during_pull(
        &a,
        "INSERT INTO items (id, scope_id, title) VALUES (?1, 'team:1', 'fresh')",
        vec![fresh.clone().into()],
    );
    a.sync();
    let local = a.dump("team:1");
    assert!(local.contains_key(&("items".into(), keep)));
    assert!(
        !local.contains_key(&("items".into(), gone)),
        "a deletion learnt through the resync"
    );
    assert!(
        local.contains_key(&("items".into(), fresh.clone())),
        "unsent row kept"
    );
    let ev = events(&rec);
    assert!(ev.contains(&SyncEvent::ResyncStarted {
        scope: "team:1".into()
    }));
    assert!(ev.contains(&SyncEvent::ResyncFinished {
        scope: "team:1".into()
    }));

    *a.transport.intercept.lock().unwrap() = None;
    a.sync();
    assert_eq!(a.dump("team:1"), server_dump(&s, "team:1"));
    assert!(server_dump(&s, "team:1").contains_key(&("items".into(), fresh)));
}

// ------------------------------------------------------------------ scopes

#[test]
fn revoked_scope_is_removed_with_its_unsent_changes() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    a.insert_item("team:2", "synced", 1);
    a.insert_item("team:1", "other scope", 1);
    a.sync();
    a.insert_item("team:2", "unsent", 2);

    hook(
        &s,
        "access",
        json!({"token": "alice", "scope": "team:2", "access": null}),
    );
    a.sync();
    assert!(a.dump("team:2").is_empty());
    assert_eq!(a.dump("team:1").len(), 1, "other scopes untouched");
    assert_eq!(a.unsent(), 0);
    assert!(events(&rec).contains(&SyncEvent::ScopeRemoved {
        scope: "team:2".into(),
        dropped_changes: 1
    }));
    assert_eq!(
        server_dump(&s, "team:2").len(),
        1,
        "the unsent row never reached the server"
    );
}

#[test]
fn granted_scope_is_downloaded() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    a.insert_item("team:2", "already there", 1);
    a.sync();
    let b = Device::new("b", "bob", &s, 2);
    let rec = Arc::new(Recorder::default());
    b.e().add_listener(rec.clone());
    b.sync();
    assert!(b.dump("team:2").is_empty());
    hook(
        &s,
        "access",
        json!({"token": "bob", "scope": "team:2", "access": "read_write"}),
    );
    b.sync();
    assert_eq!(b.dump("team:2"), server_dump(&s, "team:2"));
    assert!(events(&rec).contains(&SyncEvent::ScopeAdded {
        scope: "team:2".into()
    }));
}

#[test]
fn writes_to_a_read_only_scope_are_rejected_and_undone() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "original", 1);
    a.sync();
    let b = Device::new("b", "bob", &s, 2);
    b.sync();
    hook(
        &s,
        "access",
        json!({"token": "bob", "scope": "team:1", "access": "read"}),
    );
    let rec = Arc::new(Recorder::default());
    b.e().add_listener(rec.clone());
    b.exec(
        "UPDATE items SET title = 'not allowed' WHERE id = ?1",
        &[id.clone().into()],
    );
    let new = b.insert_item("team:1", "not allowed either", 1);
    b.sync();
    assert_eq!(b.unsent(), 0);
    let local = b.dump("team:1");
    assert_eq!(
        local[&("items".into(), id)]["title"],
        "original",
        "restored from the server"
    );
    assert!(
        !local.contains_key(&("items".into(), new)),
        "the local creation is undone"
    );
    assert_eq!(local, server_dump(&s, "team:1"));
    let rejected = events(&rec)
        .into_iter()
        .filter(
            |e| matches!(e, SyncEvent::ChangeRejected { code, .. } if code == "read_only_scope"),
        )
        .count();
    assert_eq!(rejected, 2);
}

// ------------------------------------------------------------------ sign-out

#[test]
fn sign_out_refuses_with_unsent_changes_unless_told() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    a.insert_item("team:1", "synced", 1);
    a.sync();
    a.insert_item("team:1", "unsent", 2);
    assert!(matches!(
        a.e().sign_out(false),
        Err(EngineError::PendingChanges(1))
    ));
    a.e().sign_out(true).unwrap();
    assert!(a.dump("team:1").is_empty());
    assert_eq!(a.unsent(), 0);
    assert_eq!(a.e().debug_dump().unwrap()["scopes"], json!([]));
    // Signing in again downloads everything from scratch.
    a.sync();
    assert_eq!(a.dump("team:1"), server_dump(&s, "team:1"));
}

// ------------------------------------------------------------------ migrations

#[test]
fn app_update_with_migration_keeps_unsent_changes() {
    let s = server();
    let mut a = Device::new("a", "alice", &s, 1);
    let id = a.insert_item("team:1", "made before the update", 1);
    a.migrations.push(Migration::sql(
        2,
        "ALTER TABLE items ADD COLUMN colour TEXT; CREATE INDEX items_title ON items (title);",
    ));
    a.restart();
    assert_eq!(a.unsent(), 1);
    a.exec(
        "UPDATE items SET colour = 'red', qty = 3 WHERE id = ?1",
        &[id.clone().into()],
    );
    a.sync();
    assert_eq!(
        server_dump(&s, "team:1")[&("items".into(), id)],
        json!({"title": "made before the update", "qty": 3, "note": null})
    );
}

// ------------------------------------------------------------------ oversized batches

#[test]
fn batch_refused_as_too_large_is_split() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    for i in 0..10 {
        a.insert_item("team:1", &format!("item {i}"), i);
    }
    *a.transport.intercept.lock().unwrap() = Some(Box::new(|req| {
        if req.method != Method::Post {
            return None;
        }
        let body: serde_json::Value = serde_json::from_slice(&req.body).unwrap();
        (body["changes"].as_array().unwrap().len() > 3)
            .then(|| HttpResponse::json(413, &json!({"error": {"code": "too_large"}})))
    }));
    let r = a.sync();
    assert_eq!(r.pushed, 10);
    assert_eq!(a.unsent(), 0);
    assert_eq!(server_dump(&s, "team:1").len(), 10);
}

#[test]
fn single_change_too_large_is_rejected_and_undone() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    let id = a.insert_item("team:1", "huge", 1);
    *a.transport.intercept.lock().unwrap() = Some(Box::new(|req| {
        (req.method == Method::Post)
            .then(|| HttpResponse::json(413, &json!({"error": {"code": "too_large"}})))
    }));
    a.sync();
    assert_eq!(a.unsent(), 0);
    assert!(!a.dump("team:1").contains_key(&("items".into(), id)));
    assert!(events(&rec)
        .iter()
        .any(|e| matches!(e, SyncEvent::ChangeRejected { code, .. } if code == "too_large")));
}

// ------------------------------------------------------------------ server rejections

#[test]
fn server_rejection_restores_the_server_state() {
    let mut cfg = server_config();
    cfg.tables[0].required = vec!["title".into()];
    let s = Arc::new(RefServer::new(cfg).unwrap());
    let a = Device::new("a", "alice", &s, 1);
    let rec = Arc::new(Recorder::default());
    a.e().add_listener(rec.clone());
    let id = a.insert_item("team:1", "valid", 1);
    a.sync();
    a.exec(
        "UPDATE items SET title = '' WHERE id = ?1",
        &[id.clone().into()],
    );
    a.exec(
        "UPDATE items SET qty = 99 WHERE id = ?1",
        &[id.clone().into()],
    );
    let bad = a.e().new_id();
    a.exec(
        "INSERT INTO items (id, scope_id, title) VALUES (?1, 'team:1', '')",
        &[bad.clone().into()],
    );
    let r = a.sync();
    assert_eq!(r.rejected, 2);
    let local = a.dump("team:1");
    assert_eq!(
        local[&("items".into(), id)],
        json!({"title": "valid", "qty": 1, "note": null})
    );
    assert!(!local.contains_key(&("items".into(), bad)));
    assert_eq!(local, server_dump(&s, "team:1"));
    assert!(events(&rec).iter().any(
        |e| matches!(e, SyncEvent::ChangeRejected { code, .. } if code == "validation_failed")
    ));
}

// ------------------------------------------------------------------ support

#[test]
fn debug_dump_describes_sync_state_without_data() {
    let s = server();
    let a = Device::new("a", "alice", &s, 1);
    a.insert_item("team:1", "secret title", 1);
    let before = a.e().debug_dump().unwrap();
    assert_eq!(before["unsent_changes"], 1);
    assert_eq!(before["unsent_by_table"], json!({"items": 1}));
    assert!(!before.to_string().contains("secret title"));
    a.sync();
    let after = a.e().debug_dump().unwrap();
    assert_eq!(after["unsent_changes"], 0);
    let scopes: Vec<&str> = after["scopes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["scope"].as_str().unwrap())
        .collect();
    assert_eq!(scopes, vec!["team:1", "team:2", "user:alice"]);
    assert_eq!(after["status"]["phase"], "idle");
}

// ------------------------------------------------------------------ first sync size

/// `cargo test --release -p sync-engine --test hardening first_sync_timing -- --ignored --nocapture`
#[test]
#[ignore]
fn first_sync_timing() {
    let s = server();
    let n = 20_000;
    let seed = Device::new("seed", "alice", &s, 9);
    for chunk in 0..(n / 500) {
        let stmts: Vec<(String, Vec<SqlValue>)> = (0..500)
            .map(|i| {
                (
                    "INSERT INTO items (id, scope_id, title, qty, note) VALUES (?1, 'team:1', ?2, ?3, ?4)".to_string(),
                    vec![
                        seed.e().new_id().into(),
                        format!("Item {chunk}-{i}").into(),
                        ((chunk * 500 + i) as i64).into(),
                        "Some realistic note text of a few dozen characters".into(),
                    ],
                )
            })
            .collect();
        seed.e().write_batch(&stmts).unwrap();
    }
    let t = std::time::Instant::now();
    seed.sync();
    let upload = t.elapsed();

    let mut fresh = Device::new("fresh", "bob", &s, 10);
    fresh.engine = None;
    fresh.open();
    let t = std::time::Instant::now();
    let mut cfg_dev = fresh;
    // Use realistic page size for the download.
    cfg_dev.engine = None;
    let mut c = EngineConfig::new(
        &cfg_dev.path,
        manifest(),
        "http://in-process/sync/v1",
        Arc::new(StaticToken("bob".into())),
    );
    c.migrations = migrations();
    c.default_scope = Some("user:bob".into());
    c.transport = Some(cfg_dev.transport.clone());
    c.pull_limit = 500;
    let engine = Engine::open(c).unwrap();
    let r = engine.sync_now().unwrap();
    let download = t.elapsed();
    assert_eq!(r.pulled, n);
    println!("upload of {n} records: {upload:?}; first download: {download:?} (in-process server, this machine)");
}
