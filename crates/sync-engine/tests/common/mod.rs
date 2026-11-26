//! Test harness: an in-process reference server, a transport that can lose
//! requests and responses, and devices that can be killed and reopened.

#![allow(dead_code)]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use sync_core::http::{HttpRequest, HttpResponse, Method};
use sync_core::protocol::{Access, Policy, ScopeGrant};
use sync_engine::*;
use sync_refserver::{RefServer, ServerConfig, TableConfig, UserConfig};

pub const ITEMS_SQL: &str = "
CREATE TABLE items (
    id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    title TEXT,
    qty INTEGER,
    note TEXT,
    local_note TEXT
);
CREATE TABLE locked (
    id TEXT PRIMARY KEY,
    scope_id TEXT NOT NULL,
    title TEXT,
    qty INTEGER
);
CREATE TABLE prefs (id TEXT PRIMARY KEY, theme TEXT);
";

pub fn manifest() -> Manifest {
    Manifest::from_json(
        r#"{"schema_version": 1, "tables": [
            {"name": "items", "columns": ["title", "qty", "note"], "scope": {"column": "scope_id"}},
            {"name": "locked", "columns": ["title", "qty"], "scope": {"column": "scope_id"}, "conflict": "server_wins"},
            {"name": "prefs", "columns": ["theme"], "scope": "default"}
        ]}"#,
    )
    .unwrap()
}

pub fn migrations() -> Vec<Migration> {
    vec![Migration::sql(1, ITEMS_SQL)]
}

pub fn server_config() -> ServerConfig {
    let rw = |id: &str| ScopeGrant {
        id: id.into(),
        access: Access::ReadWrite,
    };
    let table = |name: &str, fields: &[&str], conflict| TableConfig {
        name: name.into(),
        fields: fields.iter().map(|f| f.to_string()).collect(),
        conflict,
        read_only: false,
        required: vec![],
    };
    ServerConfig {
        schema_version: 1,
        min_schema_version: 1,
        limits: Default::default(),
        tables: vec![
            table("items", &["title", "qty", "note"], Policy::FieldMerge),
            table("locked", &["title", "qty"], Policy::ServerWins),
            table("prefs", &["theme"], Policy::FieldMerge),
        ],
        users: vec![
            UserConfig {
                token: "alice".into(),
                scopes: vec![rw("team:1"), rw("team:2"), rw("user:alice")],
            },
            UserConfig {
                token: "bob".into(),
                scopes: vec![rw("team:1"), rw("user:bob")],
            },
        ],
        test_hooks: true,
    }
}

// ------------------------------------------------------------------ rng

/// xorshift64*: small, seedable, good enough for test schedules.
#[derive(Clone)]
pub struct Rng(u64);

impl Rng {
    pub fn new(seed: u64) -> Rng {
        Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1)
    }
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    pub fn below(&mut self, n: u64) -> u64 {
        self.next() % n.max(1)
    }
    pub fn chance(&mut self, p: f64) -> bool {
        ((self.next() >> 11) as f64 / (1u64 << 53) as f64) < p
    }
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> Option<&'a T> {
        if items.is_empty() {
            None
        } else {
            Some(&items[self.below(items.len() as u64) as usize])
        }
    }
}

// ------------------------------------------------------------------ transport

#[derive(Default)]
pub struct Faults {
    pub drop_request: f64,
    pub drop_response: f64,
    /// Probability of losing the response to a push only.
    pub drop_push_response: f64,
    /// Fail every request: the device is offline.
    pub offline: bool,
}

/// Answers a request instead of the server when it returns `Some`.
pub type Intercept = Box<dyn Fn(&HttpRequest) -> Option<HttpResponse> + Send + Sync>;

pub struct InProcess {
    pub server: Arc<RefServer>,
    pub faults: Mutex<Faults>,
    rng: Mutex<Rng>,
    pub requests: AtomicU64,
    /// Optional hook to rewrite responses (status codes) for a test.
    pub intercept: Mutex<Option<Intercept>>,
}

impl InProcess {
    pub fn new(server: Arc<RefServer>, seed: u64) -> Arc<InProcess> {
        Arc::new(InProcess {
            server,
            faults: Mutex::new(Faults::default()),
            rng: Mutex::new(Rng::new(seed)),
            requests: AtomicU64::new(0),
            intercept: Mutex::new(None),
        })
    }

    pub fn set_faults(&self, f: Faults) {
        *self.faults.lock().unwrap() = f;
    }
}

impl Transport for InProcess {
    fn send(&self, mut req: HttpRequest) -> Result<HttpResponse, TransportError> {
        self.requests.fetch_add(1, Ordering::SeqCst);
        let (drop_req, drop_resp) = {
            let f = self.faults.lock().unwrap();
            if f.offline {
                return Err(TransportError("offline".into()));
            }
            let mut rng = self.rng.lock().unwrap();
            let is_push = req.method == Method::Post;
            (
                rng.chance(f.drop_request),
                rng.chance(f.drop_response) || (is_push && rng.chance(f.drop_push_response)),
            )
        };
        if drop_req {
            return Err(TransportError("request lost".into()));
        }
        if let Some(i) = self.intercept.lock().unwrap().as_ref() {
            if let Some(resp) = i(&req) {
                return Ok(resp);
            }
        }
        req.path = format!("/sync/v1{}", req.path);
        let resp = self.server.handle(&req);
        if drop_resp {
            return Err(TransportError("response lost".into()));
        }
        Ok(resp)
    }
}

// ------------------------------------------------------------------ server helpers

pub fn hook(server: &RefServer, name: &str, body: Value) -> Value {
    let resp = server.handle(&HttpRequest {
        method: Method::Post,
        path: format!("/_test/{name}"),
        query: vec![],
        headers: vec![],
        body: body.to_string().into_bytes(),
    });
    assert_eq!(
        resp.status,
        200,
        "hook {name}: {}",
        String::from_utf8_lossy(&resp.body)
    );
    serde_json::from_slice(&resp.body).unwrap()
}

/// A server-side write, as an admin panel would make. `None` deletes.
pub fn admin_write(
    server: &RefServer,
    scope: &str,
    table: &str,
    id: &str,
    fields: Option<Value>,
) -> i64 {
    hook(
        server,
        "write",
        json!({"scope": scope, "table": table, "id": id, "fields": fields}),
    )["version"]
        .as_i64()
        .unwrap()
}

/// Live records of a scope: (table, id) to data.
pub fn server_dump(server: &RefServer, scope: &str) -> BTreeMap<(String, String), Value> {
    let resp = server.handle(&HttpRequest {
        method: Method::Get,
        path: "/_test/dump".into(),
        query: vec![("scope".into(), scope.into())],
        headers: vec![],
        body: vec![],
    });
    let v: Value = serde_json::from_slice(&resp.body).unwrap();
    v["records"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                (
                    r["table"].as_str().unwrap().to_string(),
                    r["id"].as_str().unwrap().to_string(),
                ),
                r["data"].clone(),
            )
        })
        .collect()
}

// ------------------------------------------------------------------ devices

pub struct CrashAt {
    pub point: Mutex<Option<FaultPoint>>,
    pub rng: Mutex<Option<(Rng, f64)>>,
}

impl FaultHook for CrashAt {
    fn crash_at(&self, point: FaultPoint) -> bool {
        let mut p = self.point.lock().unwrap();
        if *p == Some(point) {
            *p = None;
            return true;
        }
        if let Some((rng, prob)) = self.rng.lock().unwrap().as_mut() {
            return rng.chance(*prob);
        }
        false
    }
}

pub struct Device {
    pub name: String,
    pub path: String,
    pub token: String,
    pub default_scope: String,
    pub transport: Arc<InProcess>,
    pub crash: Arc<CrashAt>,
    pub engine: Option<Engine>,
    pub manifest: Manifest,
    pub migrations: Vec<Migration>,
    /// Replaces the static token when set.
    pub tokens: Option<Arc<dyn TokenProvider>>,
}

static COUNTER: AtomicU64 = AtomicU64::new(0);

pub fn temp_db(name: &str) -> String {
    let dir = std::env::temp_dir().join("sync-engine-tests");
    std::fs::create_dir_all(&dir).unwrap();
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let p = dir.join(format!(
        "{name}-{}-{n}-{}.sqlite",
        std::process::id(),
        sync_core::new_id()
    ));
    p.to_str().unwrap().to_string()
}

impl Device {
    pub fn new(name: &str, token: &str, server: &Arc<RefServer>, seed: u64) -> Device {
        let mut d = Device {
            name: name.into(),
            path: temp_db(name),
            token: token.into(),
            default_scope: format!("user:{token}"),
            transport: InProcess::new(server.clone(), seed),
            crash: Arc::new(CrashAt {
                point: Mutex::new(None),
                rng: Mutex::new(None),
            }),
            engine: None,
            manifest: manifest(),
            migrations: migrations(),
            tokens: None,
        };
        d.open();
        d
    }

    pub fn open(&mut self) {
        let mut cfg = EngineConfig::new(
            &self.path,
            self.manifest.clone(),
            "http://in-process/sync/v1",
            match &self.tokens {
                Some(t) => t.clone(),
                None => Arc::new(StaticToken(self.token.clone())),
            },
        );
        cfg.migrations = self.migrations.clone();
        cfg.default_scope = Some(self.default_scope.clone());
        cfg.transport = Some(self.transport.clone());
        cfg.retry = RetryPolicy::immediate(2);
        cfg.pull_limit = 7;
        cfg.fault_hook = Some(self.crash.clone());
        self.engine = Some(Engine::open(cfg).unwrap());
    }

    /// Kill the app and start it again.
    pub fn restart(&mut self) {
        self.engine = None;
        self.open();
    }

    pub fn e(&self) -> &Engine {
        self.engine.as_ref().unwrap()
    }

    pub fn sync(&self) -> SyncReport {
        self.e()
            .sync_now()
            .unwrap_or_else(|e| panic!("{}: sync failed: {e}", self.name))
    }

    pub fn exec(&self, sql: &str, params: &[SqlValue]) {
        self.e()
            .execute(sql, params)
            .unwrap_or_else(|e| panic!("{}: {sql}: {e}", self.name));
    }

    pub fn insert_item(&self, scope: &str, title: &str, qty: i64) -> String {
        let id = self.e().new_id();
        self.exec(
            "INSERT INTO items (id, scope_id, title, qty) VALUES (?1, ?2, ?3, ?4)",
            &[id.clone().into(), scope.into(), title.into(), qty.into()],
        );
        id
    }

    /// Synced data of a scope as the device holds it, in the same shape as
    /// [`server_dump`].
    pub fn dump(&self, scope: &str) -> BTreeMap<(String, String), Value> {
        let mut out = BTreeMap::new();
        for t in &self.manifest.tables {
            let (sql, params): (String, Vec<SqlValue>) = match t.scope.column() {
                Some(c) => (
                    format!(
                        "SELECT id, {} FROM {} WHERE {c} = ?1",
                        t.columns.join(", "),
                        t.name
                    ),
                    vec![scope.into()],
                ),
                None if scope == self.default_scope => (
                    format!("SELECT id, {} FROM {}", t.columns.join(", "), t.name),
                    vec![],
                ),
                None => continue,
            };
            let r = self.e().query(&sql, &params).unwrap();
            for row in r.rows {
                let SqlValue::Text(id) = &row[0] else {
                    panic!("id")
                };
                let data: serde_json::Map<String, Value> = t
                    .columns
                    .iter()
                    .zip(&row[1..])
                    .map(|(c, v)| (c.clone(), to_json(v)))
                    .collect();
                out.insert((t.name.clone(), id.clone()), Value::Object(data));
            }
        }
        out
    }

    pub fn unsent(&self) -> i64 {
        self.e().status().pending_changes
    }
}

pub fn to_json(v: &SqlValue) -> Value {
    match v {
        SqlValue::Null => Value::Null,
        SqlValue::Integer(i) => json!(i),
        SqlValue::Real(f) => json!(f),
        SqlValue::Text(s) => json!(s),
        SqlValue::Blob(_) => panic!("blob"),
    }
}

pub fn server() -> Arc<RefServer> {
    Arc::new(RefServer::new(server_config()).unwrap())
}

/// Collects events for assertions.
#[derive(Default)]
pub struct Recorder {
    pub events: Mutex<Vec<SyncEvent>>,
    pub tables: Mutex<Vec<Vec<String>>>,
    pub statuses: Mutex<Vec<SyncStatus>>,
}

impl EngineListener for Recorder {
    fn on_event(&self, e: &SyncEvent) {
        self.events.lock().unwrap().push(e.clone());
    }
    fn on_tables_changed(&self, t: &[String]) {
        self.tables.lock().unwrap().push(t.to_vec());
    }
    fn on_status(&self, s: &SyncStatus) {
        self.statuses.lock().unwrap().push(s.clone());
    }
}

/// Make a local write the moment the first pull request goes out, so the
/// pulled page meets an unsent local edit.
pub fn write_during_pull(d: &Device, sql: &'static str, params: Vec<SqlValue>) {
    let engine = d.e().clone();
    let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
    *d.transport.intercept.lock().unwrap() = Some(Box::new(move |req| {
        if req.method == Method::Get
            && req.path.ends_with("/changes")
            && !done.swap(true, Ordering::SeqCst)
        {
            engine.execute(sql, &params).unwrap();
        }
        None
    }));
}

pub fn events(rec: &Recorder) -> Vec<SyncEvent> {
    rec.events.lock().unwrap().clone()
}
