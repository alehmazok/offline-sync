//! Binding-friendly facade over [`sync_engine`].
//!
//! Everything that crosses the boundary is plain data (strings, numbers,
//! vectors, flat structs and simple enums) or one of three callback traits.
//! The Flutter binding wraps this crate with flutter_rust_bridge; Swift and
//! Kotlin bindings can wrap the same surface with UniFFI. No binding needs
//! to know the engine's internal types.
//!
//! Configuration that has structure (manifest, migrations) is passed as
//! JSON text, in the formats documented in `docs/04-client-engine.md`.

use std::sync::Arc;
use sync_engine::{
    Engine, EngineConfig, EngineError, EngineListener, Manifest, Migration, Phase, QueryResult,
    SqlValue, SyncEvent, SyncReport, SyncStatus, TokenProvider,
};

// ------------------------------------------------------------------ data

#[derive(Debug, Clone, PartialEq)]
pub enum FfiValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl From<FfiValue> for SqlValue {
    fn from(v: FfiValue) -> SqlValue {
        match v {
            FfiValue::Null => SqlValue::Null,
            FfiValue::Integer(i) => SqlValue::Integer(i),
            FfiValue::Real(f) => SqlValue::Real(f),
            FfiValue::Text(s) => SqlValue::Text(s),
            FfiValue::Blob(b) => SqlValue::Blob(b),
        }
    }
}

impl From<SqlValue> for FfiValue {
    fn from(v: SqlValue) -> FfiValue {
        match v {
            SqlValue::Null => FfiValue::Null,
            SqlValue::Integer(i) => FfiValue::Integer(i),
            SqlValue::Real(f) => FfiValue::Real(f),
            SqlValue::Text(s) => FfiValue::Text(s),
            SqlValue::Blob(b) => FfiValue::Blob(b),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FfiRows {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<FfiValue>>,
}

impl From<QueryResult> for FfiRows {
    fn from(q: QueryResult) -> FfiRows {
        FfiRows {
            columns: q.columns,
            rows: q
                .rows
                .into_iter()
                .map(|r| r.into_iter().map(Into::into).collect())
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfiStatement {
    pub sql: String,
    pub params: Vec<FfiValue>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfiConfig {
    pub db_path: String,
    /// The manifest as JSON.
    pub manifest_json: String,
    /// A JSON array of migrations: `[{"version": 1, "sql": "...", "outbox": [...]}]`.
    pub migrations_json: String,
    pub default_scope: Option<String>,
    pub base_url: String,
    /// Records per pull page; `None` for the default of 500.
    pub pull_limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FfiStatus {
    /// `idle`, `syncing`, `offline`, `error` or `upgrade_required`.
    pub phase: String,
    pub pending_changes: i64,
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
}

impl From<&SyncStatus> for FfiStatus {
    fn from(s: &SyncStatus) -> FfiStatus {
        FfiStatus {
            phase: match s.phase {
                Phase::Idle => "idle",
                Phase::Syncing => "syncing",
                Phase::Offline => "offline",
                Phase::Error => "error",
                Phase::UpgradeRequired => "upgrade_required",
            }
            .into(),
            pending_changes: s.pending_changes,
            last_sync_at: s.last_sync_at,
            last_error: s.last_error.clone(),
        }
    }
}

/// A sync event, flattened: `kind` says which fields are set.
///
/// | kind                 | fields                         |
/// |----------------------|--------------------------------|
/// | `change_rejected`    | table, id, code, message       |
/// | `change_conflicted`  | table, id, code (the reason)   |
/// | `scope_added`        | scope                          |
/// | `scope_removed`      | scope, dropped_changes         |
/// | `resync_started`     | scope                          |
/// | `resync_finished`    | scope                          |
/// | `upgrade_required`   | min_schema_version             |
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FfiEvent {
    pub kind: String,
    pub scope: Option<String>,
    pub table: Option<String>,
    pub id: Option<String>,
    pub code: Option<String>,
    pub message: Option<String>,
    pub dropped_changes: Option<i64>,
    pub min_schema_version: Option<i64>,
}

impl From<&SyncEvent> for FfiEvent {
    fn from(e: &SyncEvent) -> FfiEvent {
        let kind = |k: &str| FfiEvent {
            kind: k.into(),
            ..FfiEvent::default()
        };
        match e {
            SyncEvent::ChangeRejected {
                table,
                id,
                code,
                message,
            } => FfiEvent {
                table: Some(table.clone()),
                id: Some(id.clone()),
                code: Some(code.clone()),
                message: Some(message.clone()),
                ..kind("change_rejected")
            },
            SyncEvent::ChangeConflicted { table, id, reason } => FfiEvent {
                table: Some(table.clone()),
                id: Some(id.clone()),
                code: Some(reason.clone()),
                ..kind("change_conflicted")
            },
            SyncEvent::ScopeAdded { scope } => FfiEvent {
                scope: Some(scope.clone()),
                ..kind("scope_added")
            },
            SyncEvent::ScopeRemoved {
                scope,
                dropped_changes,
            } => FfiEvent {
                scope: Some(scope.clone()),
                dropped_changes: Some(*dropped_changes),
                ..kind("scope_removed")
            },
            SyncEvent::ResyncStarted { scope } => FfiEvent {
                scope: Some(scope.clone()),
                ..kind("resync_started")
            },
            SyncEvent::ResyncFinished { scope } => FfiEvent {
                scope: Some(scope.clone()),
                ..kind("resync_finished")
            },
            SyncEvent::UpgradeRequired { min_schema_version } => FfiEvent {
                min_schema_version: Some(*min_schema_version),
                ..kind("upgrade_required")
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct FfiReport {
    pub pushed: u32,
    pub conflicts: u32,
    pub rejected: u32,
    pub pulled: u32,
    pub cycles: u32,
    pub coalesced: bool,
}

impl From<SyncReport> for FfiReport {
    fn from(r: SyncReport) -> FfiReport {
        FfiReport {
            pushed: r.pushed as u32,
            conflicts: r.conflicts as u32,
            rejected: r.rejected as u32,
            pulled: r.pulled as u32,
            cycles: r.cycles as u32,
            coalesced: r.coalesced,
        }
    }
}

/// An error with a stable machine-readable `code`:
/// `config`, `database`, `network`, `server`, `unauthorized`,
/// `upgrade_required`, `schema_ahead`, `protocol`, `pending_changes`.
#[derive(Debug, Clone, PartialEq)]
pub struct FfiError {
    pub code: String,
    pub message: String,
}

impl std::fmt::Display for FfiError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for FfiError {}

impl From<EngineError> for FfiError {
    fn from(e: EngineError) -> FfiError {
        let code = match &e {
            EngineError::Store(_) => "database",
            EngineError::Network(_) => "network",
            EngineError::Server(_) => "server",
            EngineError::Unauthorized => "unauthorized",
            EngineError::UpgradeRequired { .. } => "upgrade_required",
            EngineError::SchemaAhead => "schema_ahead",
            EngineError::Protocol(_) | EngineError::Crashed(_) => "protocol",
            EngineError::PendingChanges(_) => "pending_changes",
        };
        FfiError {
            code: code.into(),
            message: e.to_string(),
        }
    }
}

fn config_error(message: String) -> FfiError {
    FfiError {
        code: "config".into(),
        message,
    }
}

pub type FfiResult<T> = Result<T, FfiError>;

// ------------------------------------------------------------------ callbacks

/// Supplies the bearer token. Called from a background thread, never from
/// the thread that called into the engine's async API.
pub trait FfiTokenProvider: Send + Sync {
    fn token(&self, force_refresh: bool) -> Result<String, String>;
}

pub trait FfiListener: Send + Sync {
    fn on_status(&self, status: FfiStatus);
    fn on_event(&self, event: FfiEvent);
    fn on_tables_changed(&self, tables: Vec<String>);
}

pub trait FfiWatcher: Send + Sync {
    fn on_rows(&self, rows: FfiRows);
    fn on_error(&self, message: String);
}

struct Tokens(Arc<dyn FfiTokenProvider>);

impl TokenProvider for Tokens {
    fn token(&self, force_refresh: bool) -> Result<String, String> {
        self.0.token(force_refresh)
    }
}

struct Listener(Arc<dyn FfiListener>);

impl EngineListener for Listener {
    fn on_status(&self, s: &SyncStatus) {
        self.0.on_status(s.into());
    }
    fn on_event(&self, e: &SyncEvent) {
        self.0.on_event(e.into());
    }
    fn on_tables_changed(&self, t: &[String]) {
        self.0.on_tables_changed(t.to_vec());
    }
}

// ------------------------------------------------------------------ engine

pub struct FfiEngine {
    engine: Engine,
}

impl FfiEngine {
    pub fn open(config: FfiConfig, tokens: Arc<dyn FfiTokenProvider>) -> FfiResult<FfiEngine> {
        Self::open_with(config, tokens, None)
    }

    /// Like [`FfiEngine::open`] with a custom transport; for tests.
    pub fn open_with(
        config: FfiConfig,
        tokens: Arc<dyn FfiTokenProvider>,
        transport: Option<Arc<dyn sync_engine::Transport>>,
    ) -> FfiResult<FfiEngine> {
        let manifest =
            Manifest::from_json(&config.manifest_json).map_err(|e| config_error(e.to_string()))?;
        let migrations: Vec<Migration> = if config.migrations_json.trim().is_empty() {
            Vec::new()
        } else {
            serde_json::from_str(&config.migrations_json)
                .map_err(|e| config_error(format!("invalid migrations: {e}")))?
        };
        let mut cfg = EngineConfig::new(
            &config.db_path,
            manifest,
            &config.base_url,
            Arc::new(Tokens(tokens)),
        );
        cfg.migrations = migrations;
        cfg.default_scope = config.default_scope;
        cfg.transport = transport;
        if let Some(l) = config.pull_limit {
            cfg.pull_limit = l as usize;
        }
        Ok(FfiEngine {
            engine: Engine::open(cfg)?,
        })
    }

    pub fn execute(&self, sql: String, params: Vec<FfiValue>) -> FfiResult<u64> {
        let p: Vec<SqlValue> = params.into_iter().map(Into::into).collect();
        Ok(self.engine.execute(&sql, &p)? as u64)
    }

    pub fn write_batch(&self, statements: Vec<FfiStatement>) -> FfiResult<()> {
        let s: Vec<(String, Vec<SqlValue>)> = statements
            .into_iter()
            .map(|st| (st.sql, st.params.into_iter().map(Into::into).collect()))
            .collect();
        Ok(self.engine.write_batch(&s)?)
    }

    pub fn query(&self, sql: String, params: Vec<FfiValue>) -> FfiResult<FfiRows> {
        let p: Vec<SqlValue> = params.into_iter().map(Into::into).collect();
        Ok(self.engine.query(&sql, &p)?.into())
    }

    pub fn watch(
        &self,
        sql: String,
        params: Vec<FfiValue>,
        watcher: Arc<dyn FfiWatcher>,
    ) -> FfiResult<u64> {
        let p: Vec<SqlValue> = params.into_iter().map(Into::into).collect();
        Ok(self.engine.watch(&sql, p, move |r| match r {
            Ok(rows) => watcher.on_rows(rows.into()),
            Err(e) => watcher.on_error(e),
        })?)
    }

    pub fn unwatch(&self, id: u64) {
        self.engine.unwatch(id);
    }

    pub fn sync_now(&self) -> FfiResult<FfiReport> {
        Ok(self.engine.sync_now()?.into())
    }

    pub fn status(&self) -> FfiStatus {
        (&self.engine.status()).into()
    }

    pub fn add_listener(&self, listener: Arc<dyn FfiListener>) -> u64 {
        self.engine.add_listener(Arc::new(Listener(listener)))
    }

    pub fn remove_listener(&self, id: u64) {
        self.engine.remove_listener(id);
    }

    pub fn new_id(&self) -> String {
        self.engine.new_id()
    }

    pub fn device_id(&self) -> FfiResult<String> {
        Ok(self.engine.device_id()?)
    }

    pub fn sign_out(&self, discard_pending: bool) -> FfiResult<()> {
        Ok(self.engine.sign_out(discard_pending)?)
    }

    /// Sync state as a JSON string, for support. Contains no record data.
    pub fn debug_dump(&self) -> FfiResult<String> {
        Ok(self.engine.debug_dump()?.to_string())
    }
}

#[cfg(test)]
mod tests;
