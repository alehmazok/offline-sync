use super::*;
use serde_json::json;
use sync_core::protocol::Op;

const SCHEMA: &str = "
CREATE TABLE visits (
    id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    note TEXT,
    qty INTEGER,
    meta TEXT,
    local_flag INTEGER
);
CREATE TABLE settings (id TEXT PRIMARY KEY, theme TEXT);
";

fn manifest() -> Manifest {
    Manifest::from_json(
        r#"{"schema_version": 1, "tables": [
            {"name": "visits", "columns": ["note", "qty", "meta"], "json_columns": ["meta"], "scope": {"column": "scope_id"}},
            {"name": "settings", "columns": ["theme"], "scope": "default"}
        ]}"#,
    )
    .unwrap()
}

fn open() -> Store {
    Store::open(
        ":memory:",
        manifest(),
        &[Migration::sql(1, SCHEMA)],
        Some("user:1"),
    )
    .unwrap()
}

fn entries(s: &Store) -> Vec<sync_core::batch::OutboxEntry> {
    state::pending(s.conn(), 1000).unwrap()
}

fn t(s: &str) -> SqlValue {
    SqlValue::Text(s.into())
}

#[test]
fn insert_captures_all_synced_columns() {
    let s = open();
    s.execute(
        "INSERT INTO visits (id, scope_id, note, qty, meta, local_flag) VALUES ('v1', 'team:1', 'hi', 3, '{\"a\":[1,2]}', 1)",
        &[],
    )
    .unwrap();
    let e = entries(&s);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].op, Op::Upsert);
    assert_eq!(e[0].scope, "team:1");
    assert_eq!(e[0].table, "visits");
    assert_eq!(e[0].row_id, "v1");
    assert_eq!(e[0].base_version, None);
    assert_eq!(
        serde_json::Value::Object(e[0].fields.clone()),
        json!({"note": "hi", "qty": 3, "meta": {"a": [1, 2]}})
    );
    assert_eq!(e[0].change_id.len(), 36);
}

#[test]
fn update_captures_only_changed_synced_columns() {
    let s = open();
    s.execute(
        "INSERT INTO visits (id, scope_id, note, qty) VALUES ('v1', 'team:1', 'a', 1)",
        &[],
    )
    .unwrap();
    s.execute(
        "UPDATE visits SET qty = 2, local_flag = 9 WHERE id = 'v1'",
        &[],
    )
    .unwrap();
    s.execute("UPDATE visits SET local_flag = 10 WHERE id = 'v1'", &[])
        .unwrap();
    s.execute("UPDATE visits SET note = 'a' WHERE id = 'v1'", &[])
        .unwrap();
    s.execute("UPDATE visits SET note = NULL WHERE id = 'v1'", &[])
        .unwrap();
    let e = entries(&s);
    assert_eq!(e.len(), 3, "{e:?}");
    assert_eq!(
        serde_json::Value::Object(e[1].fields.clone()),
        json!({"qty": 2})
    );
    assert_eq!(
        serde_json::Value::Object(e[2].fields.clone()),
        json!({"note": null})
    );
}

#[test]
fn delete_is_captured_with_base_version() {
    let s = open();
    s.execute(
        "INSERT INTO visits (id, scope_id) VALUES ('v1', 'team:1')",
        &[],
    )
    .unwrap();
    state::set_pulled(s.conn(), "visits", "v1", "team:1", 41, 0).unwrap();
    state::set_known(s.conn(), "visits", "v1", "team:1", 45).unwrap();
    s.execute("DELETE FROM visits WHERE id = 'v1'", &[])
        .unwrap();
    let e = entries(&s);
    assert_eq!(e[1].op, Op::Delete);
    assert_eq!(e[1].base_version, Some(45));
    assert!(e[1].fields.is_empty());
}

#[test]
fn default_scope_tables_use_the_configured_scope() {
    let s = open();
    s.execute(
        "INSERT INTO settings (id, theme) VALUES ('me', 'dark')",
        &[],
    )
    .unwrap();
    assert_eq!(entries(&s)[0].scope, "user:1");
}

#[test]
fn capture_off_records_nothing() {
    let mut s = open();
    s.with_capture_off(|tx| {
        tx.execute(
            "INSERT INTO visits (id, scope_id, note) VALUES ('v1', 'team:1', 'x')",
            [],
        )?;
        Ok(())
    })
    .unwrap();
    assert!(entries(&s).is_empty());
    s.execute("UPDATE visits SET note = 'y'", &[]).unwrap();
    assert_eq!(entries(&s).len(), 1);
}

#[test]
fn guards_reject_missing_scope_and_identity_changes() {
    let s = open();
    assert!(s
        .execute("INSERT INTO visits (id, note) VALUES ('v1', 'x')", &[])
        .is_err());
    s.execute(
        "INSERT INTO visits (id, scope_id) VALUES ('v1', 'team:1')",
        &[],
    )
    .unwrap();
    assert!(s.execute("UPDATE visits SET id = 'v2'", &[]).is_err());
    assert!(s
        .execute("UPDATE visits SET scope_id = 'team:2'", &[])
        .is_err());
}

#[test]
fn batch_writes_are_atomic() {
    let mut s = open();
    let r = s.write_batch(&[
        (
            "INSERT INTO visits (id, scope_id) VALUES (?1, ?2)".into(),
            vec![t("v1"), t("team:1")],
        ),
        ("INSERT INTO nowhere VALUES (1)".into(), vec![]),
    ]);
    assert!(r.is_err());
    assert!(entries(&s).is_empty());
    assert!(s
        .query("SELECT * FROM visits", &[])
        .unwrap()
        .rows
        .is_empty());
}

#[test]
fn engine_tables_are_off_limits() {
    let s = open();
    assert!(s.execute("DELETE FROM _sync_outbox", &[]).is_err());
}

#[test]
fn server_records_keep_local_only_columns() {
    let mut s = open();
    s.execute(
        "INSERT INTO visits (id, scope_id, note, local_flag) VALUES ('v1', 'team:1', 'a', 7)",
        &[],
    )
    .unwrap();
    let spec = s.manifest().table("visits").unwrap().clone();
    s.with_capture_off(|tx| {
        let data = json!({"note": "from server", "qty": 5, "meta": {"k": true}, "unknown": 1});
        write_record(tx, &spec, "team:1", "v1", data.as_object().unwrap())?;
        write_record(
            tx,
            &spec,
            "team:1",
            "v2",
            json!({"note": "new"}).as_object().unwrap(),
        )?;
        Ok(())
    })
    .unwrap();
    let r = s
        .query(
            "SELECT id, note, qty, meta, local_flag FROM visits ORDER BY id",
            &[],
        )
        .unwrap();
    assert_eq!(
        r.rows[0],
        vec![
            t("v1"),
            t("from server"),
            SqlValue::Integer(5),
            t("{\"k\":true}"),
            SqlValue::Integer(7)
        ]
    );
    assert_eq!(r.rows[1][1], t("new"));
    assert_eq!(
        entries(&s).len(),
        1,
        "only the app's own insert is captured"
    );
}

#[test]
fn changed_tables_and_read_tables() {
    let mut s = open();
    s.execute(
        "INSERT INTO visits (id, scope_id) VALUES ('v1', 'team:1')",
        &[],
    )
    .unwrap();
    assert_eq!(
        s.take_changed_tables().into_iter().collect::<Vec<_>>(),
        vec!["visits"]
    );
    assert!(s.take_changed_tables().is_empty());
    let read = s
        .tables_read_by("SELECT v.note, s.theme FROM visits v JOIN settings s ON 1")
        .unwrap();
    assert_eq!(
        read.into_iter().collect::<Vec<_>>(),
        vec!["settings", "visits"]
    );
}

#[test]
fn manifest_is_checked_against_tables() {
    let m = |cols: &str| {
        Manifest::from_json(&format!(
            r#"{{"schema_version":1,"tables":[{{"name":"visits","columns":{cols},"scope":{{"column":"scope_id"}}}}]}}"#
        ))
        .unwrap()
    };
    let mig = [Migration::sql(1, SCHEMA)];
    assert!(Store::open(":memory:", m(r#"["note"]"#), &mig, None).is_ok());
    assert!(Store::open(":memory:", m(r#"["missing"]"#), &mig, None).is_err());
    assert!(
        Store::open(":memory:", m(r#"["note"]"#), &[], None).is_err(),
        "table does not exist"
    );
    let bad_pk = [Migration::sql(
        1,
        "CREATE TABLE visits (rowid_pk INTEGER PRIMARY KEY, id TEXT, scope_id TEXT, note TEXT)",
    )];
    assert!(Store::open(":memory:", m(r#"["note"]"#), &bad_pk, None).is_err());
    assert!(
        Store::open(":memory:", manifest(), &mig, None).is_err(),
        "default scope required"
    );
}

#[test]
fn migrations_run_once_and_rewrite_unsent_changes() {
    let dir = std::env::temp_dir().join(format!("sync-store-{}", sync_core::new_id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("db.sqlite");
    let path = path.to_str().unwrap();

    let v1 = vec![Migration::sql(1, SCHEMA)];
    {
        let s = Store::open(path, manifest(), &v1, Some("user:1")).unwrap();
        s.execute(
            "INSERT INTO visits (id, scope_id, note, qty) VALUES ('new', 'team:1', 'n', 1)",
            &[],
        )
        .unwrap();
        state::set_pulled(s.conn(), "visits", "old", "team:1", 5, 0).unwrap();
        s.execute(
            "INSERT INTO visits (id, scope_id, note, qty) VALUES ('old', 'team:1', 'o', 1)",
            &[],
        )
        .unwrap();
        s.execute("UPDATE visits SET qty = 2 WHERE id = 'old'", &[])
            .unwrap();
        s.execute(
            "INSERT INTO settings (id, theme) VALUES ('me', 'dark')",
            &[],
        )
        .unwrap();
    }

    // Version 2: `note` becomes `comment`, `qty` is dropped, `settings` goes.
    let manifest2 = Manifest::from_json(
        r#"{"schema_version": 2, "tables": [
            {"name": "visits", "columns": ["comment", "meta"], "json_columns": ["meta"], "scope": {"column": "scope_id"}}
        ]}"#,
    )
    .unwrap();
    let mut v2 = v1.clone();
    v2.push(
        Migration::sql(
            2,
            "ALTER TABLE visits RENAME COLUMN note TO comment;
             ALTER TABLE visits DROP COLUMN qty;
             DROP TABLE settings;",
        )
        .with(OutboxRewrite::RenameColumn {
            table: "visits".into(),
            from: "note".into(),
            to: "comment".into(),
        })
        .with(OutboxRewrite::DropColumn {
            table: "visits".into(),
            column: "qty".into(),
        })
        .with(OutboxRewrite::DropTable {
            table: "settings".into(),
        }),
    );
    let s = Store::open(path, manifest2.clone(), &v2, None).unwrap();
    let e = entries(&s);
    let summary: Vec<(String, serde_json::Value)> = e
        .iter()
        .map(|e| {
            (
                e.row_id.clone(),
                serde_json::Value::Object(e.fields.clone()),
            )
        })
        .collect();
    assert_eq!(
        summary,
        vec![
            ("new".to_string(), json!({"comment": "n", "meta": null})),
            ("old".to_string(), json!({"comment": "o", "meta": null})),
        ],
        "the update that only set qty is dropped; the creation is kept"
    );
    drop(s);
    // Reopening does not run migration 2 again.
    let s = Store::open(path, manifest2, &v2, None).unwrap();
    assert_eq!(entries(&s).len(), 2);
    s.execute("UPDATE visits SET comment = 'c' WHERE id = 'new'", &[])
        .unwrap();
    assert_eq!(
        entries(&s).len(),
        3,
        "triggers are installed for the new schema"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn bad_migration_list_is_refused() {
    let r = Store::open(
        ":memory:",
        manifest(),
        &[Migration::sql(2, SCHEMA), Migration::sql(1, "")],
        Some("u"),
    );
    assert!(r.is_err());
}
