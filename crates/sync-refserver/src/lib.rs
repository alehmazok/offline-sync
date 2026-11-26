//! Reference server for sync protocol v1.
//!
//! It follows `docs/03-server-guide.md` in the simplest possible way: one
//! SQLite connection behind a mutex, so every request is serialised. That
//! makes it correct by construction and slow, which is what a reference
//! needs. Project data is stored as JSON documents in one `records` table.
//!
//! [`RefServer::handle`] is a pure request-to-response function, used both
//! by the HTTP front end ([`serve`]) and directly in tests.

mod config;
mod http;
mod time;

pub use config::{Policy, ServerConfig, TableConfig, UserConfig};
pub use http::{serve, ServerHandle};

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::{Mutex, RwLock};
use sync_core::http::{HttpRequest, HttpResponse, Method};
use sync_core::protocol::*;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS sync_scopes (
    scope_id TEXT PRIMARY KEY,
    seq      INTEGER NOT NULL,
    min_seq  INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS sync_index (
    scope_id   TEXT NOT NULL,
    tbl        TEXT NOT NULL,
    record_id  TEXT NOT NULL,
    seq        INTEGER NOT NULL,
    deleted    INTEGER NOT NULL,
    changed_at INTEGER NOT NULL,
    PRIMARY KEY (tbl, record_id)
);
CREATE UNIQUE INDEX IF NOT EXISTS sync_index_scope_seq ON sync_index (scope_id, seq);
CREATE TABLE IF NOT EXISTS sync_seen (
    change_id  TEXT PRIMARY KEY,
    scope_id   TEXT NOT NULL,
    status     TEXT NOT NULL,
    version    INTEGER,
    reason     TEXT,
    code       TEXT,
    message    TEXT,
    created_at INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS records (
    tbl  TEXT NOT NULL,
    id   TEXT NOT NULL,
    data TEXT NOT NULL,
    PRIMARY KEY (tbl, id)
);
";

pub struct RefServer {
    config: ServerConfig,
    users: RwLock<HashMap<String, Vec<ScopeGrant>>>,
    db: Mutex<Connection>,
}

/// Result type for request handlers: an error is a ready HTTP response.
type Handled = Result<HttpResponse, HttpResponse>;

fn error(status: u16, code: &str, message: impl Into<String>) -> HttpResponse {
    HttpResponse::json(status, &ErrorBody::new(code, message))
}

fn internal(e: impl std::fmt::Display) -> HttpResponse {
    log::error!("refserver internal error: {e}");
    error(500, error_code::INTERNAL, e.to_string())
}

/// Cursor format of the reference server. Opaque to clients.
///
/// - `"<n>"`: everything up to change number `n` has been delivered.
/// - `"d<n>.<m>"`: a download from the beginning is in progress, up to
///   `n`, started when the scope's `min_seq` was `m`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Cursor {
    Start,
    After(i64),
    Download { after: i64, min_seq: i64 },
}

impl Cursor {
    fn parse(s: &str) -> Option<Cursor> {
        let num = |t: &str| t.parse::<i64>().ok().filter(|v| *v >= 0);
        match s.strip_prefix('d') {
            Some(rest) => {
                let (a, m) = rest.split_once('.')?;
                Some(Cursor::Download {
                    after: num(a)?,
                    min_seq: num(m)?,
                })
            }
            None => num(s).map(Cursor::After),
        }
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Cursor::Start => write!(f, "0"),
            Cursor::After(n) => write!(f, "{n}"),
            Cursor::Download { after, min_seq } => write!(f, "d{after}.{min_seq}"),
        }
    }
}

struct IndexRow {
    scope: String,
    seq: i64,
    deleted: bool,
}

impl RefServer {
    /// A server with an in-memory database.
    pub fn new(config: ServerConfig) -> rusqlite::Result<RefServer> {
        Self::with_connection(config, Connection::open_in_memory()?)
    }

    /// A server whose database is the SQLite file at `path`.
    pub fn open(config: ServerConfig, path: &str) -> rusqlite::Result<RefServer> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "journal_mode", "WAL")?;
        Self::with_connection(config, conn)
    }

    fn with_connection(config: ServerConfig, conn: Connection) -> rusqlite::Result<RefServer> {
        conn.execute_batch(SCHEMA)?;
        let users = config
            .users
            .iter()
            .map(|u| (u.token.clone(), u.scopes.clone()))
            .collect();
        Ok(RefServer {
            config,
            users: RwLock::new(users),
            db: Mutex::new(conn),
        })
    }

    pub fn config(&self) -> &ServerConfig {
        &self.config
    }

    /// Handle one request. `req.path` may include the `/sync/v1` prefix.
    pub fn handle(&self, req: &HttpRequest) -> HttpResponse {
        let path = req.path.strip_prefix("/sync/v1").unwrap_or(&req.path);
        let parts: Vec<&str> = path.trim_matches('/').split('/').collect();
        let result = match (&req.method, parts.as_slice()) {
            (_, ["_test", hook]) if self.config.test_hooks => self.test_hook(hook, req),
            (Method::Get, ["session"]) => self.session(req),
            (Method::Get, ["scopes", scope, "changes"]) => self.pull(req, scope),
            (Method::Post, ["scopes", scope, "changes"]) => self.push(req, scope),
            _ => Err(error(404, error_code::BAD_REQUEST, "no such endpoint")),
        };
        result.unwrap_or_else(|e| e)
    }

    // ------------------------------------------------------------ checks

    fn authenticate(&self, req: &HttpRequest) -> Result<Vec<ScopeGrant>, HttpResponse> {
        let token = req
            .header("Authorization")
            .and_then(|h| h.strip_prefix("Bearer "))
            .ok_or_else(|| error(401, error_code::UNAUTHORIZED, "missing token"))?;
        let users = self.users.read().unwrap();
        users
            .get(token.trim())
            .cloned()
            .ok_or_else(|| error(401, error_code::UNAUTHORIZED, "unknown token"))
    }

    fn check_schema(&self, req: &HttpRequest) -> Result<(), HttpResponse> {
        let v: i64 = req
            .header(HEADER_SCHEMA)
            .and_then(|h| h.trim().parse().ok())
            .ok_or_else(|| {
                error(
                    400,
                    error_code::BAD_REQUEST,
                    "missing or invalid X-Sync-Schema",
                )
            })?;
        if v < self.config.min_schema_version {
            let mut body = ErrorBody::new(error_code::UPGRADE_REQUIRED, "client schema is too old");
            body.error.min_schema_version = Some(self.config.min_schema_version);
            return Err(HttpResponse::json(426, &body));
        }
        if v > self.config.schema_version {
            return Err(error(
                409,
                error_code::SCHEMA_AHEAD,
                "client schema is newer than the server's",
            ));
        }
        Ok(())
    }

    fn scope_access(&self, req: &HttpRequest, scope: &str) -> Result<Access, HttpResponse> {
        let grants = self.authenticate(req)?;
        self.check_schema(req)?;
        grants
            .iter()
            .find(|g| g.id == scope)
            .map(|g| g.access)
            .ok_or_else(|| error(403, error_code::SCOPE_FORBIDDEN, "no access to this scope"))
    }

    // ------------------------------------------------------------ session

    fn session(&self, req: &HttpRequest) -> Handled {
        let grants = self.authenticate(req)?;
        self.check_schema(req)?;
        Ok(HttpResponse::json(
            200,
            &Session {
                protocol: PROTOCOL_VERSION,
                schema_version: self.config.schema_version,
                min_schema_version: self.config.min_schema_version,
                server_time: Some(time::now_rfc3339()),
                limits: self.config.limits.clone(),
                scopes: grants,
            },
        ))
    }

    // ------------------------------------------------------------ push

    fn push(&self, req: &HttpRequest, scope: &str) -> Handled {
        let access = self.scope_access(req, scope)?;
        if !access.can_write() {
            return Err(error(
                403,
                error_code::SCOPE_FORBIDDEN,
                "scope is read-only",
            ));
        }
        if req.body.len() > self.config.limits.max_push_bytes {
            return Err(error(413, error_code::TOO_LARGE, "request body too large"));
        }
        let body: PushRequest = serde_json::from_slice(&req.body)
            .map_err(|e| error(400, error_code::BAD_REQUEST, format!("invalid body: {e}")))?;
        if body.changes.len() > self.config.limits.max_push_changes {
            return Err(error(413, error_code::TOO_LARGE, "too many changes"));
        }

        let mut conn = self.db.lock().unwrap();
        let tx = conn.transaction().map_err(internal)?;
        let mut results = Vec::with_capacity(body.changes.len());
        for change in &body.changes {
            results.push(self.push_one(&tx, scope, change).map_err(internal)?);
        }
        tx.commit().map_err(internal)?;
        Ok(HttpResponse::json(200, &PushResponse { results }))
    }

    fn push_one(
        &self,
        tx: &Transaction,
        scope: &str,
        c: &Change,
    ) -> rusqlite::Result<ChangeResult> {
        if let Some(mut seen) = self.seen(tx, &c.change_id)? {
            if seen.status != ResultStatus::Applied {
                seen.record = Some(self.record_in_scope(tx, scope, &c.table, &c.id)?);
            }
            return Ok(seen);
        }
        let result = self.decide(tx, scope, c)?;
        self.remember(tx, scope, &result)?;
        Ok(result)
    }

    fn decide(&self, tx: &Transaction, scope: &str, c: &Change) -> rusqlite::Result<ChangeResult> {
        let reject = |code: &str, msg: &str, record: Option<Record>| {
            Ok(ChangeResult::rejected(
                c.change_id.clone(),
                code,
                msg,
                record,
            ))
        };

        let Some(table) = self.config.table(&c.table) else {
            return reject(reject_code::UNKNOWN_TABLE, "unknown table", None);
        };
        if c.id.is_empty() || c.id.len() > 64 {
            return reject(reject_code::VALIDATION_FAILED, "invalid record id", None);
        }
        let idx = self.index_row(tx, &c.table, &c.id)?;
        if let Some(i) = &idx {
            if i.scope != scope {
                return reject(
                    reject_code::FORBIDDEN,
                    "record belongs to another scope",
                    None,
                );
            }
        }
        let current = || self.record_in_scope(tx, scope, &c.table, &c.id);
        if table.read_only {
            return reject(reject_code::READ_ONLY, "table is read-only", current()?);
        }

        match c.op {
            Op::Upsert => {
                let fields = c.fields.clone().unwrap_or_default();
                if let Some(bad) = fields.keys().find(|k| !table.fields.contains(k)) {
                    return reject(
                        reject_code::UNKNOWN_FIELD,
                        &format!("unknown field {bad}"),
                        current()?,
                    );
                }
                if let Some(i) = &idx {
                    if i.deleted {
                        return Ok(ChangeResult::conflict(
                            c.change_id.clone(),
                            ConflictReason::Deleted,
                            current()?,
                        ));
                    }
                    if table.conflict == Policy::ServerWins && c.base_version != Some(i.seq) {
                        return Ok(ChangeResult::conflict(
                            c.change_id.clone(),
                            ConflictReason::Stale,
                            current()?,
                        ));
                    }
                }
                let mut data = match &idx {
                    Some(_) => self.data(tx, &c.table, &c.id)?.unwrap_or_default(),
                    None => Fields::new(),
                };
                for (k, v) in fields {
                    data.insert(k, v);
                }
                for f in &table.required {
                    let empty = match data.get(f) {
                        None | Some(Value::Null) => true,
                        Some(Value::String(s)) => s.is_empty(),
                        _ => false,
                    };
                    if empty {
                        return reject(
                            reject_code::VALIDATION_FAILED,
                            &format!("{f} must not be empty"),
                            current()?,
                        );
                    }
                }
                let seq = self.write(tx, scope, &c.table, &c.id, Some(&data))?;
                Ok(ChangeResult::applied(c.change_id.clone(), Some(seq)))
            }
            Op::Delete => match &idx {
                None => Ok(ChangeResult::applied(c.change_id.clone(), None)),
                Some(i) if i.deleted => Ok(ChangeResult::applied(c.change_id.clone(), Some(i.seq))),
                Some(i)
                    if table.conflict == Policy::ServerWins && c.base_version != Some(i.seq) =>
                {
                    Ok(ChangeResult::conflict(
                        c.change_id.clone(),
                        ConflictReason::Stale,
                        current()?,
                    ))
                }
                Some(_) => {
                    let seq = self.write(tx, scope, &c.table, &c.id, None)?;
                    Ok(ChangeResult::applied(c.change_id.clone(), Some(seq)))
                }
            },
        }
    }

    /// The write path (server guide section 3.3). `data` is `None` for a
    /// delete. Returns the new version.
    fn write(
        &self,
        tx: &Transaction,
        scope: &str,
        tbl: &str,
        id: &str,
        data: Option<&Fields>,
    ) -> rusqlite::Result<i64> {
        let seq: i64 = tx.query_row(
            "INSERT INTO sync_scopes (scope_id, seq, min_seq) VALUES (?1, 1, 0)
             ON CONFLICT (scope_id) DO UPDATE SET seq = seq + 1
             RETURNING seq",
            params![scope],
            |r| r.get(0),
        )?;
        match data {
            Some(d) => {
                tx.execute(
                    "INSERT INTO records (tbl, id, data) VALUES (?1, ?2, ?3)
                     ON CONFLICT (tbl, id) DO UPDATE SET data = excluded.data",
                    params![tbl, id, Value::Object(d.clone()).to_string()],
                )?;
            }
            None => {
                tx.execute(
                    "DELETE FROM records WHERE tbl = ?1 AND id = ?2",
                    params![tbl, id],
                )?;
            }
        }
        tx.execute(
            "INSERT INTO sync_index (scope_id, tbl, record_id, seq, deleted, changed_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)
             ON CONFLICT (tbl, record_id) DO UPDATE
             SET seq = excluded.seq, deleted = excluded.deleted, changed_at = excluded.changed_at",
            params![scope, tbl, id, seq, data.is_none(), time::now_secs()],
        )?;
        Ok(seq)
    }

    fn index_row(
        &self,
        tx: &Connection,
        tbl: &str,
        id: &str,
    ) -> rusqlite::Result<Option<IndexRow>> {
        tx.query_row(
            "SELECT scope_id, seq, deleted FROM sync_index WHERE tbl = ?1 AND record_id = ?2",
            params![tbl, id],
            |r| {
                Ok(IndexRow {
                    scope: r.get(0)?,
                    seq: r.get(1)?,
                    deleted: r.get(2)?,
                })
            },
        )
        .optional()
    }

    fn data(&self, tx: &Connection, tbl: &str, id: &str) -> rusqlite::Result<Option<Fields>> {
        let text: Option<String> = tx
            .query_row(
                "SELECT data FROM records WHERE tbl = ?1 AND id = ?2",
                params![tbl, id],
                |r| r.get(0),
            )
            .optional()?;
        Ok(text
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .and_then(|v| match v {
                Value::Object(m) => Some(m),
                _ => None,
            }))
    }

    /// The record as a pull would return it: every field of the table, with
    /// unset fields as `null`.
    fn full_record(
        &self,
        tx: &Connection,
        tbl: &str,
        id: &str,
        idx: &IndexRow,
    ) -> rusqlite::Result<Option<Record>> {
        if idx.deleted {
            return Ok(Some(Record {
                table: tbl.to_string(),
                id: id.to_string(),
                version: idx.seq,
                deleted: true,
                data: None,
            }));
        }
        let Some(stored) = self.data(tx, tbl, id)? else {
            return Ok(None);
        };
        let fields = self
            .config
            .table(tbl)
            .map(|t| t.fields.clone())
            .unwrap_or_default();
        let data = fields
            .into_iter()
            .map(|f| {
                let v = stored.get(&f).cloned().unwrap_or(Value::Null);
                (f, v)
            })
            .collect();
        Ok(Some(Record {
            table: tbl.to_string(),
            id: id.to_string(),
            version: idx.seq,
            deleted: false,
            data: Some(data),
        }))
    }

    /// Current server state of a record, but only if it belongs to `scope`;
    /// a record of another scope is never revealed.
    fn record_in_scope(
        &self,
        tx: &Connection,
        scope: &str,
        tbl: &str,
        id: &str,
    ) -> rusqlite::Result<Option<Record>> {
        match self.index_row(tx, tbl, id)? {
            Some(idx) if idx.scope == scope => self.full_record(tx, tbl, id, &idx),
            _ => Ok(None),
        }
    }

    fn seen(&self, tx: &Connection, change_id: &str) -> rusqlite::Result<Option<ChangeResult>> {
        tx.query_row(
            "SELECT status, version, reason, code, message FROM sync_seen WHERE change_id = ?1",
            params![change_id],
            |r| {
                let status: String = r.get(0)?;
                let status = ResultStatus::parse(&status).unwrap_or(ResultStatus::Rejected);
                let reason: Option<String> = r.get(2)?;
                Ok(ChangeResult {
                    change_id: change_id.to_string(),
                    status,
                    version: (status == ResultStatus::Applied)
                        .then(|| r.get(1))
                        .transpose()?,
                    reason: reason.as_deref().and_then(ConflictReason::parse),
                    code: r.get(3)?,
                    message: r.get(4)?,
                    record: None,
                })
            },
        )
        .optional()
    }

    fn remember(&self, tx: &Connection, scope: &str, r: &ChangeResult) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT INTO sync_seen (change_id, scope_id, status, version, reason, code, message, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                r.change_id,
                scope,
                r.status.as_str(),
                r.applied_version(),
                r.reason.map(|x| x.as_str()),
                r.code,
                r.message,
                time::now_secs()
            ],
        )?;
        Ok(())
    }

    // ------------------------------------------------------------ pull

    fn pull(&self, req: &HttpRequest, scope: &str) -> Handled {
        self.scope_access(req, scope)?;
        let cursor = match req.query_param("cursor").filter(|c| !c.is_empty()) {
            None => Cursor::Start,
            Some(c) => Cursor::parse(c)
                .ok_or_else(|| error(400, error_code::BAD_REQUEST, "invalid cursor"))?,
        };
        let limit: usize = match req.query_param("limit") {
            None => 500,
            Some(l) => l
                .parse()
                .ok()
                .filter(|v: &usize| *v >= 1)
                .ok_or_else(|| error(400, error_code::BAD_REQUEST, "invalid limit"))?,
        }
        .min(self.config.limits.max_pull_limit);

        let conn = self.db.lock().unwrap();
        let (scope_seq, min_seq): (i64, i64) = conn
            .query_row(
                "SELECT seq, min_seq FROM sync_scopes WHERE scope_id = ?1",
                params![scope],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(internal)?
            .unwrap_or((0, 0));
        let expired = || {
            error(
                410,
                error_code::CURSOR_EXPIRED,
                "cursor is older than retained tombstones",
            )
        };
        let after = match cursor {
            Cursor::Start => 0,
            Cursor::After(a) if a < min_seq => return Err(expired()),
            Cursor::After(a) => a,
            // A download from the beginning still in progress is valid as
            // long as no tombstones were pruned since it started.
            Cursor::Download { min_seq: m, .. } if m != min_seq => return Err(expired()),
            Cursor::Download { after, .. } => after,
        };
        let downloading = !matches!(cursor, Cursor::After(_));

        let mut stmt = conn
            .prepare(
                "SELECT tbl, record_id, seq, deleted FROM sync_index
                 WHERE scope_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
            )
            .map_err(internal)?;
        let rows: Vec<(String, String, IndexRow)> = stmt
            .query_map(params![scope, after, (limit + 1) as i64], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    IndexRow {
                        scope: scope.to_string(),
                        seq: r.get(2)?,
                        deleted: r.get(3)?,
                    },
                ))
            })
            .and_then(|it| it.collect())
            .map_err(internal)?;

        let has_more = rows.len() > limit;
        let mut last = after;
        let mut records = Vec::new();
        for (tbl, id, idx) in rows.into_iter().take(limit) {
            last = idx.seq;
            // Only the first page of a download may leave tombstones out:
            // the device holds nothing yet. Later pages must carry them,
            // because a record sent on an earlier page may since be deleted.
            if idx.deleted && cursor == Cursor::Start {
                continue;
            }
            if let Some(r) = self.full_record(&conn, &tbl, &id, &idx).map_err(internal)? {
                records.push(r);
            }
        }
        let cursor = if has_more && downloading {
            Cursor::Download {
                after: last,
                min_seq,
            }
        } else if has_more {
            Cursor::After(last)
        } else {
            // Caught up: every number up to the counter has been seen. The
            // counter can be ahead of the last index row when the newest
            // changes were tombstones that have been pruned.
            Cursor::After(last.max(scope_seq))
        };
        Ok(HttpResponse::json(
            200,
            &PullResponse {
                records,
                cursor: cursor.to_string(),
                has_more,
            },
        ))
    }

    // ------------------------------------------------------------ test hooks

    fn test_hook(&self, hook: &str, req: &HttpRequest) -> Handled {
        #[derive(Deserialize)]
        struct AccessBody {
            token: String,
            scope: String,
            access: Option<Access>,
        }
        #[derive(Deserialize)]
        struct WriteBody {
            scope: String,
            table: String,
            id: String,
            fields: Option<Fields>,
        }
        let bad = |e: serde_json::Error| error(400, error_code::BAD_REQUEST, e.to_string());
        match hook {
            // Remove every tombstone and move each scope's min_seq past it.
            "prune" => {
                let mut conn = self.db.lock().unwrap();
                let tx = conn.transaction().map_err(internal)?;
                tx.execute_batch(
                    "UPDATE sync_scopes SET min_seq = MAX(min_seq, COALESCE(
                         (SELECT MAX(seq) FROM sync_index i
                          WHERE i.scope_id = sync_scopes.scope_id AND i.deleted = 1), min_seq));
                     DELETE FROM sync_index WHERE deleted = 1;",
                )
                .map_err(internal)?;
                tx.commit().map_err(internal)?;
                Ok(HttpResponse::json(200, &json!({"ok": true})))
            }
            // Grant, change or revoke a user's access to a scope.
            "access" => {
                let b: AccessBody = serde_json::from_slice(&req.body).map_err(bad)?;
                let mut users = self.users.write().unwrap();
                let grants = users.entry(b.token).or_default();
                grants.retain(|g| g.id != b.scope);
                if let Some(access) = b.access {
                    grants.push(ScopeGrant {
                        id: b.scope,
                        access,
                    });
                }
                Ok(HttpResponse::json(200, &json!({"ok": true})))
            }
            // A server-side write, as an admin panel or a job would make.
            // `fields: null` deletes. Fields are merged like a push.
            "write" => {
                let b: WriteBody = serde_json::from_slice(&req.body).map_err(bad)?;
                let mut conn = self.db.lock().unwrap();
                let tx = conn.transaction().map_err(internal)?;
                let data = match b.fields {
                    Some(f) => {
                        let mut d = self
                            .data(&tx, &b.table, &b.id)
                            .map_err(internal)?
                            .unwrap_or_default();
                        d.extend(f);
                        Some(d)
                    }
                    None => None,
                };
                let seq = self
                    .write(&tx, &b.scope, &b.table, &b.id, data.as_ref())
                    .map_err(internal)?;
                tx.commit().map_err(internal)?;
                Ok(HttpResponse::json(200, &json!({"version": seq})))
            }
            // Every live record of a scope, for test assertions.
            "dump" => {
                let scope = req.query_param("scope").unwrap_or_default().to_string();
                let conn = self.db.lock().unwrap();
                let mut stmt = conn
                    .prepare("SELECT tbl, record_id, seq FROM sync_index WHERE scope_id = ?1 AND deleted = 0 ORDER BY tbl, record_id")
                    .map_err(internal)?;
                let rows: Vec<(String, String, i64)> = stmt
                    .query_map(params![scope], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
                    .and_then(|it| it.collect())
                    .map_err(internal)?;
                let mut records = Vec::new();
                for (tbl, id, seq) in rows {
                    let idx = IndexRow {
                        scope: scope.clone(),
                        seq,
                        deleted: false,
                    };
                    if let Some(r) = self.full_record(&conn, &tbl, &id, &idx).map_err(internal)? {
                        records.push(r);
                    }
                }
                Ok(HttpResponse::json(200, &json!({"records": records})))
            }
            _ => Err(error(404, error_code::BAD_REQUEST, "no such test hook")),
        }
    }
}
