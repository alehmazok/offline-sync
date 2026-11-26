//! Offline-first sync engine.
//!
//! The engine owns the app's local SQLite database. The app reads and
//! writes through [`Engine::query`], [`Engine::execute`] and
//! [`Engine::write_batch`]; every write to a synced table is captured by
//! triggers into an outbox. [`Engine::sync_now`] runs one sync cycle with a
//! server that speaks protocol v1: session, push, pull.
//!
//! Design: `docs/04-client-engine.md`. Protocol: `docs/02-protocol.md`.
//!
//! The engine is synchronous and thread-safe. Calls block; bindings run
//! them on a background thread. The host decides when to sync.

mod cycle;
mod types;

pub use sync_core::protocol::Policy;
pub use sync_net::{
    RetryPolicy, StaticToken, TokenProvider, Transport, TransportError, UreqTransport,
};
pub use sync_store::{
    Manifest, Migration, OutboxRewrite, QueryResult, ScopeRule, SqlValue, TableSpec,
};
pub use types::*;

use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use sync_net::Client;
use sync_store::{state, Store};

pub struct EngineConfig {
    /// Path of the SQLite file, or `":memory:"`.
    pub db_path: String,
    pub manifest: Manifest,
    /// The app's local schema steps, oldest first.
    pub migrations: Vec<Migration>,
    /// Scope for tables whose manifest entry says `"scope": "default"`.
    pub default_scope: Option<String>,
    /// Protocol base URL, for example `https://api.example.com/sync/v1`.
    pub base_url: String,
    pub token_provider: Arc<dyn TokenProvider>,
    /// Defaults to HTTPS with [`UreqTransport`].
    pub transport: Option<Arc<dyn Transport>>,
    pub retry: RetryPolicy,
    /// Records per pull page.
    pub pull_limit: usize,
    /// Test hook to simulate the app being killed. Leave `None`.
    pub fault_hook: Option<Arc<dyn FaultHook>>,
}

impl EngineConfig {
    pub fn new(
        db_path: &str,
        manifest: Manifest,
        base_url: &str,
        token_provider: Arc<dyn TokenProvider>,
    ) -> EngineConfig {
        EngineConfig {
            db_path: db_path.to_string(),
            manifest,
            migrations: Vec::new(),
            default_scope: None,
            base_url: base_url.to_string(),
            token_provider,
            transport: None,
            retry: RetryPolicy::default(),
            pull_limit: 500,
            fault_hook: None,
        }
    }
}

type WatchCallback = Arc<dyn Fn(std::result::Result<QueryResult, String>) + Send + Sync>;

struct Watch {
    id: u64,
    sql: String,
    params: Vec<SqlValue>,
    tables: BTreeSet<String>,
    callback: WatchCallback,
}

pub(crate) struct Inner {
    pub(crate) store: Mutex<Store>,
    pub(crate) client: Client,
    pub(crate) manifest: Manifest,
    pub(crate) pull_limit: usize,
    pub(crate) fault: Option<Arc<dyn FaultHook>>,
    sync_lock: Mutex<()>,
    rerun: AtomicBool,
    status: Mutex<SyncStatus>,
    listeners: Mutex<Vec<(u64, Arc<dyn EngineListener>)>>,
    watches: Mutex<Vec<Watch>>,
    next_id: AtomicU64,
}

impl Inner {
    pub(crate) fn store(&self) -> MutexGuard<'_, Store> {
        self.store.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn fault(&self, point: FaultPoint) -> Result<()> {
        match &self.fault {
            Some(h) if h.crash_at(point) => Err(EngineError::Crashed(point)),
            _ => Ok(()),
        }
    }
}

/// Handle to an open engine. Cheap to clone; all clones share one database.
#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

impl Engine {
    /// Open or create the database, run migrations and install change
    /// capture.
    pub fn open(config: EngineConfig) -> Result<Engine> {
        let store = Store::open(
            &config.db_path,
            config.manifest.clone(),
            &config.migrations,
            config.default_scope.as_deref(),
        )?;
        let device = store.device_id()?;
        let transport = config
            .transport
            .unwrap_or_else(|| Arc::new(UreqTransport::new(&config.base_url)));
        let client = Client::new(
            transport,
            config.token_provider,
            config.manifest.schema_version,
            device,
            config.retry,
        );
        let pending = state::count_unsent(store.conn())?;
        let last_sync_at =
            state::meta_get(store.conn(), "last_sync_at")?.and_then(|v| v.parse().ok());
        Ok(Engine {
            inner: Arc::new(Inner {
                store: Mutex::new(store),
                client,
                manifest: config.manifest,
                pull_limit: config.pull_limit.clamp(1, 1000),
                fault: config.fault_hook,
                sync_lock: Mutex::new(()),
                rerun: AtomicBool::new(false),
                status: Mutex::new(SyncStatus {
                    phase: Phase::Idle,
                    pending_changes: pending,
                    last_sync_at,
                    last_error: None,
                }),
                listeners: Mutex::new(Vec::new()),
                watches: Mutex::new(Vec::new()),
                next_id: AtomicU64::new(1),
            }),
        })
    }

    // -------------------------------------------------------------- data

    /// Run one write statement. Returns the number of rows changed.
    pub fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<usize> {
        let n = self.inner.store().execute(sql, params)?;
        self.after_change(Vec::new());
        Ok(n)
    }

    /// Run several write statements in one atomic transaction.
    pub fn write_batch(&self, statements: &[(String, Vec<SqlValue>)]) -> Result<()> {
        self.inner.store().write_batch(statements)?;
        self.after_change(Vec::new());
        Ok(())
    }

    pub fn query(&self, sql: &str, params: &[SqlValue]) -> Result<QueryResult> {
        Ok(self.inner.store().query(sql, params)?)
    }

    /// Call `callback` with the result of the query now, and again whenever
    /// a table it reads changes. Returns an ID for [`Engine::unwatch`].
    pub fn watch(
        &self,
        sql: &str,
        params: Vec<SqlValue>,
        callback: impl Fn(std::result::Result<QueryResult, String>) + Send + Sync + 'static,
    ) -> Result<u64> {
        let (tables, first) = {
            let mut store = self.inner.store();
            let tables = store.tables_read_by(sql)?;
            (tables, store.query(sql, &params).map_err(|e| e.to_string()))
        };
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let callback: WatchCallback = Arc::new(callback);
        self.inner.watches.lock().unwrap().push(Watch {
            id,
            sql: sql.to_string(),
            params,
            tables,
            callback: callback.clone(),
        });
        callback(first);
        Ok(id)
    }

    pub fn unwatch(&self, id: u64) {
        self.inner.watches.lock().unwrap().retain(|w| w.id != id);
    }

    /// A new record ID (UUID version 7).
    pub fn new_id(&self) -> String {
        sync_core::new_id()
    }

    pub fn device_id(&self) -> Result<String> {
        Ok(self.inner.store().device_id()?)
    }

    // -------------------------------------------------------------- sync

    /// Run a sync cycle: session, push, pull. If a cycle is already running
    /// on another thread, this returns at once with `coalesced` set and the
    /// running cycle does one more round when it finishes.
    pub fn sync_now(&self) -> Result<SyncReport> {
        let _guard = match self.inner.sync_lock.try_lock() {
            Ok(g) => g,
            Err(_) => {
                self.inner.rerun.store(true, Ordering::SeqCst);
                return Ok(SyncReport {
                    coalesced: true,
                    ..SyncReport::default()
                });
            }
        };
        self.update_status(|s| {
            s.phase = Phase::Syncing;
        });
        let mut total = SyncReport::default();
        loop {
            self.inner.rerun.store(false, Ordering::SeqCst);
            let mut events = Vec::new();
            let result = self.inner.run_cycle(&mut events);
            if let Err(EngineError::UpgradeRequired { min_schema_version }) = &result {
                events.push(SyncEvent::UpgradeRequired {
                    min_schema_version: *min_schema_version,
                });
            }
            self.after_change(events);
            match result {
                Ok(r) => total.add(&r),
                Err(e) => {
                    let phase = match &e {
                        EngineError::Network(_) => Phase::Offline,
                        EngineError::UpgradeRequired { .. } => Phase::UpgradeRequired,
                        _ => Phase::Error,
                    };
                    log::warn!("sync cycle failed: {e}");
                    self.update_status(|s| {
                        s.phase = phase;
                        s.last_error = Some(e.to_string());
                    });
                    return Err(e);
                }
            }
            if !self.inner.rerun.load(Ordering::SeqCst) {
                break;
            }
        }
        let now = now_secs();
        {
            let store = self.inner.store();
            let _ = state::meta_set(store.conn(), "last_sync_at", &now.to_string());
        }
        self.update_status(|s| {
            s.phase = Phase::Idle;
            s.last_error = None;
            s.last_sync_at = Some(now);
        });
        Ok(total)
    }

    pub fn status(&self) -> SyncStatus {
        self.inner.status.lock().unwrap().clone()
    }

    pub fn add_listener(&self, listener: Arc<dyn EngineListener>) -> u64 {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        self.inner.listeners.lock().unwrap().push((id, listener));
        id
    }

    pub fn remove_listener(&self, id: u64) {
        self.inner
            .listeners
            .lock()
            .unwrap()
            .retain(|(i, _)| *i != id);
    }

    /// Remove all synced data and sync state from the device. Refused while
    /// there are unsent changes, unless `discard_pending` is true.
    pub fn sign_out(&self, discard_pending: bool) -> Result<()> {
        let _guard = self
            .inner
            .sync_lock
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        {
            let mut store = self.inner.store();
            let unsent = state::count_unsent(store.conn())?;
            if unsent > 0 && !discard_pending {
                return Err(EngineError::PendingChanges(unsent));
            }
            let tables: Vec<String> = self
                .inner
                .manifest
                .tables
                .iter()
                .map(|t| t.name.clone())
                .collect();
            capture_off(&mut store, |tx| {
                for t in &tables {
                    tx.execute(&format!("DELETE FROM \"{t}\""), [])?;
                }
                state::clear_all(tx)?;
                state::meta_delete(tx, "last_sync_at")?;
                Ok(())
            })?;
        }
        self.update_status(|s| {
            s.last_sync_at = None;
            s.last_error = None;
            s.phase = Phase::Idle;
        });
        self.after_change(Vec::new());
        Ok(())
    }

    /// Sync state as JSON, for support and debugging. Contains no record
    /// data.
    pub fn debug_dump(&self) -> Result<serde_json::Value> {
        let store = self.inner.store();
        let conn = store.conn();
        let scopes: Vec<serde_json::Value> = state::scopes(conn)?
            .into_iter()
            .map(|s| {
                serde_json::json!({
                    "scope": s.scope, "access": s.access.as_str(), "cursor": s.cursor,
                    "state": s.state, "resync_gen": s.resync_gen,
                })
            })
            .collect();
        let batch = state::sent_batch(conn)?
            .map(|b| serde_json::json!({"scope": b.scope, "changes": b.map.len()}));
        let by_table: Vec<(String, i64)> = conn
            .prepare("SELECT tbl, COUNT(*) FROM _sync_outbox GROUP BY tbl ORDER BY tbl")?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        Ok(serde_json::json!({
            "device_id": store.device_id()?,
            "schema_version": self.inner.manifest.schema_version,
            "default_scope": store.default_scope()?,
            "scopes": scopes,
            "unsent_changes": state::count_unsent(conn)?,
            "unsent_by_table": by_table.into_iter().map(|(k, v)| (k, serde_json::Value::from(v))).collect::<serde_json::Map<_, _>>(),
            "sent_batch": batch,
            "status": self.status(),
        }))
    }

    /// Close the engine. Other clones keep it open until they are dropped.
    pub fn close(self) {}

    // -------------------------------------------------------------- internals

    fn update_status(&self, f: impl FnOnce(&mut SyncStatus)) {
        let snapshot = {
            let mut s = self.inner.status.lock().unwrap();
            f(&mut s);
            s.clone()
        };
        for l in self.listeners() {
            l.on_status(&snapshot);
        }
    }

    fn listeners(&self) -> Vec<Arc<dyn EngineListener>> {
        self.inner
            .listeners
            .lock()
            .unwrap()
            .iter()
            .map(|(_, l)| l.clone())
            .collect()
    }

    /// Tell listeners and watches about events and changed tables. Runs the
    /// affected watch queries under the store lock, then calls back with no
    /// lock held.
    fn after_change(&self, events: Vec<SyncEvent>) {
        let (tables, pending, results) = {
            let store = self.inner.store();
            let tables = store.take_changed_tables();
            let pending = state::count_unsent(store.conn()).unwrap_or(-1);
            let watches = self.inner.watches.lock().unwrap();
            let results: Vec<(WatchCallback, std::result::Result<QueryResult, String>)> = watches
                .iter()
                .filter(|w| w.tables.iter().any(|t| tables.contains(t)))
                .map(|w| {
                    (
                        w.callback.clone(),
                        store.query(&w.sql, &w.params).map_err(|e| e.to_string()),
                    )
                })
                .collect();
            (tables, pending, results)
        };
        let listeners = self.listeners();
        for e in &events {
            for l in &listeners {
                l.on_event(e);
            }
        }
        if !tables.is_empty() {
            let list: Vec<String> = tables.into_iter().collect();
            for l in &listeners {
                l.on_tables_changed(&list);
            }
        }
        for (cb, r) in results {
            cb(r);
        }
        let changed = {
            let mut s = self.inner.status.lock().unwrap();
            let changed = s.pending_changes != pending && pending >= 0;
            if changed {
                s.pending_changes = pending;
            }
            changed.then(|| s.clone())
        };
        if let Some(s) = changed {
            for l in &listeners {
                l.on_status(&s);
            }
        }
    }
}

/// A transaction with change capture off, in the engine's error type.
pub(crate) fn capture_off<T>(
    store: &mut Store,
    f: impl FnOnce(&sync_store::rusqlite::Transaction) -> Result<T>,
) -> Result<T> {
    store.capture_off(f)
}

fn now_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}
