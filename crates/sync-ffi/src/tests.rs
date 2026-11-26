use super::*;
use std::sync::Mutex;
use sync_core::http::{HttpRequest, HttpResponse};
use sync_refserver::{RefServer, ServerConfig};

struct InProcess(Arc<RefServer>);

impl sync_engine::Transport for InProcess {
    fn send(&self, mut req: HttpRequest) -> Result<HttpResponse, sync_engine::TransportError> {
        req.path = format!("/sync/v1{}", req.path);
        Ok(self.0.handle(&req))
    }
}

struct Token;
impl FfiTokenProvider for Token {
    fn token(&self, _: bool) -> Result<String, String> {
        Ok("token-a".into())
    }
}

#[derive(Default)]
struct Collect {
    events: Mutex<Vec<FfiEvent>>,
    statuses: Mutex<Vec<FfiStatus>>,
    rows: Mutex<Vec<usize>>,
}

impl FfiListener for Collect {
    fn on_status(&self, s: FfiStatus) {
        self.statuses.lock().unwrap().push(s);
    }
    fn on_event(&self, e: FfiEvent) {
        self.events.lock().unwrap().push(e);
    }
    fn on_tables_changed(&self, _: Vec<String>) {}
}

impl FfiWatcher for Collect {
    fn on_rows(&self, rows: FfiRows) {
        self.rows.lock().unwrap().push(rows.rows.len());
    }
    fn on_error(&self, _: String) {}
}

fn config() -> FfiConfig {
    FfiConfig {
        db_path: ":memory:".into(),
        manifest_json: r#"{"schema_version": 1, "tables": [
            {"name": "conf_items", "columns": ["title", "qty", "note"], "scope": {"column": "scope_id"}}
        ]}"#
        .into(),
        migrations_json: r#"[{"version": 1, "sql": "CREATE TABLE conf_items (id TEXT PRIMARY KEY, scope_id TEXT NOT NULL, title TEXT, qty INTEGER, note TEXT)"}]"#.into(),
        default_scope: None,
        base_url: "http://unused/sync/v1".into(),
        pull_limit: Some(10),
    }
}

#[test]
fn round_trip_through_the_facade() {
    let server = Arc::new(RefServer::new(ServerConfig::conformance()).unwrap());
    let e =
        FfiEngine::open_with(config(), Arc::new(Token), Some(Arc::new(InProcess(server)))).unwrap();
    let c = Arc::new(Collect::default());
    e.add_listener(c.clone());
    e.watch("SELECT id FROM conf_items".into(), vec![], c.clone())
        .unwrap();

    let id = e.new_id();
    e.execute(
        "INSERT INTO conf_items (id, scope_id, title, qty) VALUES (?1, ?2, ?3, ?4)".into(),
        vec![
            FfiValue::Text(id.clone()),
            FfiValue::Text("team:conf1".into()),
            FfiValue::Text("x".into()),
            FfiValue::Integer(2),
        ],
    )
    .unwrap();
    assert_eq!(e.status().pending_changes, 1);
    let r = e.sync_now().unwrap();
    assert_eq!(r.pushed, 1);
    assert_eq!(e.status().phase, "idle");

    let rows = e
        .query(
            "SELECT title, qty FROM conf_items WHERE id = ?1".into(),
            vec![FfiValue::Text(id)],
        )
        .unwrap();
    assert_eq!(rows.columns, vec!["title", "qty"]);
    assert_eq!(
        rows.rows[0],
        vec![FfiValue::Text("x".into()), FfiValue::Integer(2)]
    );
    assert!(c
        .events
        .lock()
        .unwrap()
        .iter()
        .any(|e| e.kind == "scope_added" && e.scope.as_deref() == Some("team:conf1")));
    assert_eq!(c.rows.lock().unwrap()[..2], [0, 1]);
    assert!(serde_json::from_str::<serde_json::Value>(&e.debug_dump().unwrap()).is_ok());

    let err = e.sign_out(false);
    assert!(err.is_ok(), "nothing pending: {err:?}");
}

#[test]
fn errors_have_stable_codes() {
    let mut bad = config();
    bad.manifest_json = "{".into();
    let err = FfiEngine::open(bad, Arc::new(Token)).err().unwrap();
    assert_eq!(err.code, "config");

    let e = FfiEngine::open(config(), Arc::new(Token)).unwrap();
    let err = e
        .execute("INSERT INTO nowhere VALUES (1)".into(), vec![])
        .unwrap_err();
    assert_eq!(err.code, "database");
}
