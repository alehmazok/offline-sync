//! The Dart-facing API, translated by flutter_rust_bridge.
//!
//! This file only adapts `sync-ffi` to the bridge: Dart callbacks become
//! token providers, and stream sinks become listeners. All logic lives in
//! the engine.
//!
//! Streams are opened with the `subscribe_*` functions under an ID chosen
//! by Dart, and closed with [`SyncEngine::unsubscribe`]. App code does not
//! call these: `lib/src/streams.dart` wraps them as `watch`, `statusStream`,
//! `eventStream` and `tablesChangedStream`, whose cancel calls
//! `unsubscribe`. Dropping the registration drops the Rust sink, which
//! closes the Dart stream; without that, cancelling a bridge stream only
//! completes when Rust next sends to it.
//!
//! As a fallback, a registration whose Dart stream is gone without an
//! `unsubscribe` is dropped after a send to it fails.
//!
//! Every function except `new_id` is async from Dart, so it runs on a Rust worker thread. That matters: the token callback is
//! Dart code, and the engine waits for it; on the Dart main thread that
//! wait would deadlock.

use crate::adapter::{DartTokens, Dead, Listener, Reaper, RowsSink, Subscriptions};
use crate::frb_generated::StreamSink;
use flutter_rust_bridge::{frb, DartFnFuture};
use std::sync::{Arc, Mutex};
use sync_ffi::FfiEngine;
pub use sync_ffi::{
    FfiConfig, FfiError, FfiEvent, FfiReport, FfiRows, FfiStatement, FfiStatus, FfiValue,
};

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

#[frb(opaque)]
pub struct SyncEngine {
    inner: FfiEngine,
    reaper: Arc<Reaper>,
    subscriptions: Mutex<Subscriptions>,
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
            reaper: Arc::new(Reaper::default()),
            subscriptions: Mutex::new(Subscriptions::default()),
        })
    }

    fn unregister(&self, d: Dead) {
        match d {
            Dead::Watch(id) => self.inner.unwatch(id),
            Dead::Listener(id) => self.inner.remove_listener(id),
        }
    }

    /// Drop watches and listeners whose Dart streams went away without an
    /// `unsubscribe`.
    fn reap(&self) {
        let dead = self.reaper.take();
        if dead.is_empty() {
            return;
        }
        let mut subs = self.subscriptions.lock().unwrap();
        subs.active.retain(|_, d| !dead.contains(d));
        drop(subs);
        for d in dead {
            self.unregister(d);
        }
    }

    /// Record a new registration under the Dart ID, or drop it at once if
    /// Dart already cancelled it.
    fn track(&self, subscription: i64, d: Dead) {
        let mut subs = self.subscriptions.lock().unwrap();
        if subs.cancelled_early.remove(&subscription) {
            drop(subs);
            self.unregister(d);
        } else {
            subs.active.insert(subscription, d);
        }
    }

    /// Close a stream opened with a `subscribe_*` function. Its Dart stream
    /// then ends.
    pub fn unsubscribe(&self, subscription: i64) {
        let mut subs = self.subscriptions.lock().unwrap();
        match subs.active.remove(&subscription) {
            Some(d) => {
                drop(subs);
                self.unregister(d);
            }
            None => {
                subs.cancelled_early.insert(subscription);
            }
        }
    }

    /// Run a statement that changes data; returns the number of rows changed.
    pub fn execute(&self, sql: String, params: Vec<FfiValue>) -> Result<i64, FfiError> {
        self.reap();
        Ok(self.inner.execute(sql, params)? as i64)
    }

    /// Run several statements in one transaction.
    pub fn write_batch(&self, statements: Vec<FfiStatement>) -> Result<(), FfiError> {
        self.reap();
        self.inner.write_batch(statements)
    }

    pub fn query(&self, sql: String, params: Vec<FfiValue>) -> Result<FfiRows, FfiError> {
        self.reap();
        self.inner.query(sql, params)
    }

    /// Emits the query result now and again whenever a table it reads
    /// changes. Use `watch` from Dart.
    pub fn subscribe_watch(
        &self,
        subscription: i64,
        sql: String,
        params: Vec<FfiValue>,
        sink: StreamSink<FfiRows>,
    ) -> Result<(), FfiError> {
        self.reap();
        let watcher = RowsSink::new(sink, self.reaper.clone());
        let id = self.inner.watch(sql, params, watcher.clone())?;
        watcher.set_id(id);
        drop(watcher);
        self.track(subscription, Dead::Watch(id));
        Ok(())
    }

    pub fn sync_now(&self) -> Result<FfiReport, FfiError> {
        self.reap();
        self.inner.sync_now()
    }

    pub fn status(&self) -> FfiStatus {
        self.reap();
        self.inner.status()
    }

    /// Status changes. Use `statusStream` from Dart.
    pub fn subscribe_status(&self, subscription: i64, sink: StreamSink<FfiStatus>) {
        self.reap();
        let l = Listener::status(sink, self.reaper.clone());
        let id = self.inner.add_listener(l.clone());
        l.set_id(id);
        drop(l);
        self.track(subscription, Dead::Listener(id));
    }

    /// Things to show the user, such as `change_rejected` and
    /// `upgrade_required`. Use `eventStream` from Dart.
    pub fn subscribe_events(&self, subscription: i64, sink: StreamSink<FfiEvent>) {
        self.reap();
        let l = Listener::events(sink, self.reaper.clone());
        let id = self.inner.add_listener(l.clone());
        l.set_id(id);
        drop(l);
        self.track(subscription, Dead::Listener(id));
    }

    /// Names of tables changed by local writes or by sync. Use
    /// `tablesChangedStream` from Dart.
    pub fn subscribe_tables(&self, subscription: i64, sink: StreamSink<Vec<String>>) {
        self.reap();
        let l = Listener::tables(sink, self.reaper.clone());
        let id = self.inner.add_listener(l.clone());
        l.set_id(id);
        drop(l);
        self.track(subscription, Dead::Listener(id));
    }

    /// A new record ID (UUIDv7).
    #[frb(sync)]
    pub fn new_id(&self) -> String {
        self.inner.new_id()
    }

    pub fn device_id(&self) -> Result<String, FfiError> {
        self.inner.device_id()
    }

    pub fn sign_out(&self, discard_pending: bool) -> Result<(), FfiError> {
        self.reap();
        self.inner.sign_out(discard_pending)
    }

    /// Sync state as JSON, for support. Contains no record data.
    pub fn debug_dump(&self) -> Result<String, FfiError> {
        self.inner.debug_dump()
    }
}
