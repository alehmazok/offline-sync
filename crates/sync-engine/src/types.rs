use serde::Serialize;
use sync_net::ClientError;
use sync_store::StoreError;

#[derive(Debug, thiserror::Error)]
pub enum EngineError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("network: {0}")]
    Network(String),
    #[error("server: {0}")]
    Server(String),
    #[error("not signed in or the token was refused")]
    Unauthorized,
    #[error("the app must be updated to keep syncing (server needs schema {min_schema_version} or newer)")]
    UpgradeRequired { min_schema_version: i64 },
    #[error("the server is older than the app; try again later")]
    SchemaAhead,
    #[error("protocol error: {0}")]
    Protocol(String),
    #[error("{0} changes are not uploaded yet")]
    PendingChanges(i64),
    /// A test fault hook stopped the engine at a crash point.
    #[error("stopped at crash point {0:?}")]
    Crashed(FaultPoint),
}

impl From<sync_store::rusqlite::Error> for EngineError {
    fn from(e: sync_store::rusqlite::Error) -> Self {
        EngineError::Store(StoreError::Sqlite(e))
    }
}

impl From<serde_json::Error> for EngineError {
    fn from(e: serde_json::Error) -> Self {
        EngineError::Store(StoreError::Json(e))
    }
}

impl EngineError {
    pub(crate) fn from_client(e: ClientError) -> EngineError {
        match e {
            ClientError::Network(m) => EngineError::Network(m),
            ClientError::Server {
                status,
                code,
                message,
            } => EngineError::Server(format!("{status} {code} {message}")),
            ClientError::Unauthorized | ClientError::Token(_) => EngineError::Unauthorized,
            ClientError::SchemaAhead => EngineError::SchemaAhead,
            ClientError::UpgradeRequired => EngineError::UpgradeRequired {
                min_schema_version: 0,
            },
            other => EngineError::Protocol(other.to_string()),
        }
    }
}

pub(crate) type Result<T> = std::result::Result<T, EngineError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Phase {
    Idle,
    Syncing,
    Offline,
    Error,
    UpgradeRequired,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SyncStatus {
    pub phase: Phase,
    /// Local changes not yet confirmed by the server.
    pub pending_changes: i64,
    /// Unix seconds of the last cycle that completed.
    pub last_sync_at: Option<i64>,
    pub last_error: Option<String>,
}

/// Things the app may need to tell the user about.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SyncEvent {
    ChangeRejected {
        table: String,
        id: String,
        code: String,
        message: String,
    },
    ChangeConflicted {
        table: String,
        id: String,
        reason: String,
    },
    ScopeAdded {
        scope: String,
    },
    ScopeRemoved {
        scope: String,
        dropped_changes: i64,
    },
    ResyncStarted {
        scope: String,
    },
    ResyncFinished {
        scope: String,
    },
    UpgradeRequired {
        min_schema_version: i64,
    },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct SyncReport {
    /// Changes the server applied.
    pub pushed: usize,
    pub conflicts: usize,
    pub rejected: usize,
    /// Records received and written locally.
    pub pulled: usize,
    /// Cycles run: more than one when sync was requested during a cycle.
    pub cycles: usize,
    /// True when another cycle was already running; this request was folded
    /// into it.
    pub coalesced: bool,
}

impl SyncReport {
    pub(crate) fn add(&mut self, o: &SyncReport) {
        self.pushed += o.pushed;
        self.conflicts += o.conflicts;
        self.rejected += o.rejected;
        self.pulled += o.pulled;
        self.cycles += o.cycles;
    }
}

/// Points between two local transactions where a test may stop the engine,
/// as if the app were killed there.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// A batch is recorded as sent, nothing transmitted yet.
    BatchRecorded,
    /// The server answered a push; the results are not applied.
    PushAnswered,
    /// Push results are applied.
    PushApplied,
    /// A pull page arrived; it is not applied.
    PageReceived,
    /// A pull page and its cursor are applied.
    PageApplied,
}

/// Test hook: return true to stop the engine at `point`.
pub trait FaultHook: Send + Sync {
    fn crash_at(&self, point: FaultPoint) -> bool;
}

/// Receives status changes, events, and data changes. Called on the
/// thread that caused them, with no engine lock held.
pub trait EngineListener: Send + Sync {
    fn on_status(&self, _status: &SyncStatus) {}
    fn on_event(&self, _event: &SyncEvent) {}
    /// Synced or local tables whose contents changed.
    fn on_tables_changed(&self, _tables: &[String]) {}
}
