//! The Dart-facing API, translated by flutter_rust_bridge.
//!
//! This file only adapts `sync-ffi` to the bridge: Dart callbacks become
//! token providers, and stream sinks become listeners. All logic lives in
//! the engine.
//!
//! Every function here is async from Dart (none is marked `#[frb(sync)]`),
//! so it runs on a Rust worker thread. That matters: the token callback is
//! Dart code, and the engine waits for it; on the Dart main thread that
//! wait would deadlock.

use crate::frb_generated::StreamSink;
use flutter_rust_bridge::{frb, DartFnFuture};
use std::sync::{Arc, Mutex};
pub use sync_ffi::{
    FfiConfig, FfiError, FfiEvent, FfiReport, FfiRows, FfiStatement, FfiStatus, FfiValue,
};
use sync_ffi::{FfiEngine, FfiListener, FfiTokenProvider, FfiWatcher};

// Mirrors of the facade's plain types, so the bridge generates Dart
// classes for them.

#[frb(mirror(FfiValue))]
pub enum _FfiValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

#[frb(mirror(FfiRows))]
pub struct _FfiRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<FfiValue>>,
}

#[frb(mirror(FfiStatement))]
pub struct _FfiStatement {
    pub sql: String,
    pub params: Vec<FfiValue>,
}

#[frb(mirror(FfiConfig))]
pub struct _FfiConfig {
    pub db_path: String,
    pub manifest_json: String,
    pub migrations_json: String,
    pub default_scope: Option<String>,
    pub base_url: String,
    pub pull_limit: Option<u32>,
}

#[frb(mirror(FfiStatus))]
pub struct _FfiStatus {
    pub phase: String,
    pub pending_changes: i64,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
}

#[frb(mirror(FfiEvent))]
pub struct _FfiEvent {
    pub kind: String,
    pub scope: Option<String>,
    pub table: Option<String>,
    pub id: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub dropped_changes: Option<i64>,
    pub min_schema_version: Option<i64>,
}

#[frb(mirror(FfiReport))]
pub struct _FfiReport {
    pub pushed: u32,
    pub conflicts: u32,
    pub rejected: u32,
    pub pulled: u32,
    pub cycles: u32,
    pub coalesced: bool,
}

#[frb(mirror(FfiError))]
pub struct _FfiError {
    pub code: String,
    pub message: String,
}

type DartTokenFn = dyn Fn(bool) -> DartFnFuture<String> + Send + Sync;

struct DartTokens(Box<DartTokenFn>);

impl FfiTokenProvider for DartTokens {
    fn token(&self, force_refresh: bool) -> Result<String, String> {
        let token = futures::executor::block_on((self.0)(force_refresh));
        if token.is_empty() {
            Err("no token".into())
        } else {
            Ok(token)
        }
    }
}

#[derive(Default)]
struct Sinks {
    status: Option<Mutex<StreamSink<FfiStatus>>>,
    events: Option<Mutex<StreamSink<FfiEvent>>>,
    tables: Option<Mutex<StreamSink<Vec<String>>>>,
}

impl FfiListener for Sinks {
    fn on_status(&self, s: FfiStatus) {
        if let Some(sink) = &self.status {
            let _ = sink.lock().unwrap().add(s);
        }
    }
    fn on_event(&self, e: FfiEvent) {
        if let Some(sink) = &self.events {
            let _ = sink.lock().unwrap().add(e);
        }
    }
    fn on_tables_changed(&self, t: Vec<String>) {
        if let Some(sink) = &self.tables {
            let _ = sink.lock().unwrap().add(t);
        }
    }
}

struct RowsSink(Mutex<StreamSink<FfiRows>>);

impl FfiWatcher for RowsSink {
    fn on_rows(&self, rows: FfiRows) {
        let _ = self.0.lock().unwrap().add(rows);
    }
    fn on_error(&self, message: String) {
        let _ = self.0.lock().unwrap().add_error(FfiError {
            code: "database".into(),
            message,
        });
    }
}

#[frb(opaque)]
pub struct SyncEngine {
    inner: FfiEngine,
}

impl SyncEngine {
    /// Open the engine. `token_provider` is called with `true` after the
    /// server refused a token; return an empty string when signed out.
    pub fn open(
        config: FfiConfig,
        token_provider: impl Fn(bool) -> DartFnFuture<String> + Send + Sync + 'static,
    ) -> Result<SyncEngine, FfiError> {
        let tokens = Arc::new(DartTokens(Box::new(token_provider)));
        Ok(SyncEngine {
            inner: FfiEngine::open(config, tokens)?,
        })
    }

    pub fn execute(&self, sql: String, params: Vec<FfiValue>) -> Result<u64, FfiError> {
        self.inner.execute(sql, params)
    }

    pub fn write_batch(&self, statements: Vec<FfiStatement>) -> Result<(), FfiError> {
        self.inner.write_batch(statements)
    }

    pub fn query(&self, sql: String, params: Vec<FfiValue>) -> Result<FfiRows, FfiError> {
        self.inner.query(sql, params)
    }

    /// Emits the query result now and again whenever a table it reads
    /// changes. Returns an ID for [`SyncEngine::unwatch`].
    pub fn watch(
        &self,
        sql: String,
        params: Vec<FfiValue>,
        sink: StreamSink<FfiRows>,
    ) -> Result<u64, FfiError> {
        self.inner
            .watch(sql, params, Arc::new(RowsSink(Mutex::new(sink))))
    }

    pub fn unwatch(&self, id: u64) {
        self.inner.unwatch(id)
    }

    pub fn sync_now(&self) -> Result<FfiReport, FfiError> {
        self.inner.sync_now()
    }

    pub fn status(&self) -> FfiStatus {
        self.inner.status()
    }

    pub fn status_stream(&self, sink: StreamSink<FfiStatus>) {
        self.inner.add_listener(Arc::new(Sinks {
            status: Some(Mutex::new(sink)),
            ..Sinks::default()
        }));
    }

    pub fn event_stream(&self, sink: StreamSink<FfiEvent>) {
        self.inner.add_listener(Arc::new(Sinks {
            events: Some(Mutex::new(sink)),
            ..Sinks::default()
        }));
    }

    pub fn tables_changed_stream(&self, sink: StreamSink<Vec<String>>) {
        self.inner.add_listener(Arc::new(Sinks {
            tables: Some(Mutex::new(sink)),
            ..Sinks::default()
        }));
    }

    #[frb(sync)]
    pub fn new_id(&self) -> String {
        self.inner.new_id()
    }

    pub fn device_id(&self) -> Result<String, FfiError> {
        self.inner.device_id()
    }

    pub fn sign_out(&self, discard_pending: bool) -> Result<(), FfiError> {
        self.inner.sign_out(discard_pending)
    }

    pub fn debug_dump(&self) -> Result<String, FfiError> {
        self.inner.debug_dump()
    }
}
