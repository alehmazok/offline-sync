//! Runs the example app's sync setup (`example/assets/*.json` and
//! `example/server.json`) through two engines and the reference server over
//! real HTTP. This checks the example's configuration without a device;
//! the Dart side is covered by `example/integration_test`.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use sync_ffi::{FfiConfig, FfiEngine, FfiTokenProvider, FfiValue};
use sync_refserver::{serve, RefServer, ServerConfig};

fn example(file: &str) -> String {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../example")
        .join(file);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

struct Demo;
impl FfiTokenProvider for Demo {
    fn token(&self, _: bool) -> Result<String, String> {
        Ok("demo".into())
    }
}

fn db_path(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("offline-sync-example-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    let _ = std::fs::remove_file(&p);
    p
}

fn device(base_url: &str, name: &str) -> FfiEngine {
    let config = FfiConfig {
        db_path: db_path(name).to_string_lossy().into_owned(),
        manifest_json: example("assets/sync_manifest.json"),
        migrations_json: example("assets/migrations.json"),
        default_scope: Some("team:demo".into()),
        base_url: base_url.into(),
        pull_limit: None,
    };
    FfiEngine::open(config, Arc::new(Demo)).expect("engine opens with the example config")
}

fn notes(e: &FfiEngine) -> Vec<Vec<FfiValue>> {
    e.query(
        "SELECT title, done FROM notes ORDER BY title".into(),
        vec![],
    )
    .unwrap()
    .rows
}

#[test]
fn two_devices_converge_with_the_example_setup() {
    let config: ServerConfig = serde_json::from_str(&example("server.json")).unwrap();
    let server = serve(Arc::new(RefServer::new(config).unwrap()), "127.0.0.1:0", 2).unwrap();
    let url = server.base_url();

    let a = device(&url, "a.sqlite");
    let b = device(&url, "b.sqlite");

    let id = a.new_id();
    a.execute(
        "INSERT INTO notes (id, title) VALUES (?1, ?2)".into(),
        vec![FfiValue::Text(id.clone()), FfiValue::Text("milk".into())],
    )
    .unwrap();
    assert_eq!(a.sync_now().unwrap().pushed, 1);
    b.sync_now().unwrap();
    assert_eq!(notes(&b), notes(&a));

    // Both edit while "offline", different fields of the same note, plus a
    // new note each; then both sync twice so each sees the other's work.
    a.execute(
        "UPDATE notes SET title = 'oat milk' WHERE id = ?1".into(),
        vec![FfiValue::Text(id.clone())],
    )
    .unwrap();
    b.execute(
        "UPDATE notes SET done = 1 WHERE id = ?1".into(),
        vec![FfiValue::Text(id.clone())],
    )
    .unwrap();
    for (e, title) in [(&a, "bread"), (&b, "eggs")] {
        e.execute(
            "INSERT INTO notes (id, title) VALUES (?1, ?2)".into(),
            vec![FfiValue::Text(e.new_id()), FfiValue::Text(title.into())],
        )
        .unwrap();
    }
    for _ in 0..2 {
        a.sync_now().unwrap();
        b.sync_now().unwrap();
    }

    let expected = vec![
        vec![FfiValue::Text("bread".into()), FfiValue::Integer(0)],
        vec![FfiValue::Text("eggs".into()), FfiValue::Integer(0)],
        vec![FfiValue::Text("oat milk".into()), FfiValue::Integer(1)],
    ];
    assert_eq!(notes(&a), expected);
    assert_eq!(notes(&b), expected);
    assert_eq!(a.status().pending_changes, 0);
    assert_eq!(b.status().pending_changes, 0);

    // A delete reaches the other device too.
    a.execute(
        "DELETE FROM notes WHERE id = ?1".into(),
        vec![FfiValue::Text(id)],
    )
    .unwrap();
    a.sync_now().unwrap();
    b.sync_now().unwrap();
    assert_eq!(notes(&b).len(), 2);
}
