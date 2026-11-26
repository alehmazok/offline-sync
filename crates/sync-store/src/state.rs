//! The engine's own tables and their access functions.
//!
//! | Table           | One row per                 |
//! |-----------------|-----------------------------|
//! | `_sync_outbox`  | unsent local change         |
//! | `_sync_batches` | batch sent, result not yet applied |
//! | `_sync_rows`    | record known to the server  |
//! | `_sync_scopes`  | scope                       |
//! | `_sync_meta`    | setting                     |

use crate::StoreError;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;
use sync_core::batch::OutboxEntry;
use sync_core::protocol::{Access, Fields, Op};

pub const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS _sync_meta (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS _sync_outbox (
    seq          INTEGER PRIMARY KEY AUTOINCREMENT,
    change_id    TEXT NOT NULL UNIQUE,
    scope        TEXT NOT NULL,
    tbl          TEXT NOT NULL,
    row_id       TEXT NOT NULL,
    op           TEXT NOT NULL,
    fields       TEXT NOT NULL DEFAULT '{}',
    base_version INTEGER,
    state        TEXT NOT NULL DEFAULT 'pending',
    batch_id     INTEGER
);
CREATE INDEX IF NOT EXISTS _sync_outbox_row ON _sync_outbox (tbl, row_id);
CREATE TABLE IF NOT EXISTS _sync_batches (
    batch_id INTEGER PRIMARY KEY AUTOINCREMENT,
    scope    TEXT NOT NULL,
    body     TEXT NOT NULL,
    map      TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS _sync_rows (
    tbl            TEXT NOT NULL,
    row_id         TEXT NOT NULL,
    scope          TEXT NOT NULL,
    pulled_version INTEGER NOT NULL DEFAULT 0,
    known_version  INTEGER NOT NULL DEFAULT 0,
    mark           INTEGER NOT NULL DEFAULT 0,
    PRIMARY KEY (tbl, row_id)
);
CREATE INDEX IF NOT EXISTS _sync_rows_scope ON _sync_rows (scope);
CREATE TABLE IF NOT EXISTS _sync_scopes (
    scope      TEXT PRIMARY KEY,
    access     TEXT NOT NULL,
    cursor     TEXT,
    state      TEXT NOT NULL DEFAULT 'active',
    resync_gen INTEGER NOT NULL DEFAULT 0
);
";

// ------------------------------------------------------------------ meta

pub fn meta_get(conn: &Connection, key: &str) -> rusqlite::Result<Option<String>> {
    conn.query_row(
        "SELECT value FROM _sync_meta WHERE key = ?1",
        params![key],
        |r| r.get(0),
    )
    .optional()
}

pub fn meta_set(conn: &Connection, key: &str, value: &str) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _sync_meta (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, value],
    )?;
    Ok(())
}

pub fn meta_delete(conn: &Connection, key: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM _sync_meta WHERE key = ?1", params![key])?;
    Ok(())
}

// ------------------------------------------------------------------ outbox

fn entry_from_row(r: &rusqlite::Row) -> rusqlite::Result<OutboxEntry> {
    let op: String = r.get(5)?;
    let fields: String = r.get(6)?;
    Ok(OutboxEntry {
        seq: r.get(0)?,
        change_id: r.get(1)?,
        scope: r.get(2)?,
        table: r.get(3)?,
        row_id: r.get(4)?,
        op: Op::parse(&op).unwrap_or(Op::Upsert),
        fields: match serde_json::from_str::<Value>(&fields) {
            Ok(Value::Object(m)) => m,
            _ => Fields::new(),
        },
        base_version: r.get(7)?,
    })
}

const ENTRY_COLS: &str = "seq, change_id, scope, tbl, row_id, op, fields, base_version";

/// Pending (unsent) entries in local order.
pub fn pending(conn: &Connection, limit: usize) -> rusqlite::Result<Vec<OutboxEntry>> {
    conn.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM _sync_outbox WHERE state = 'pending' ORDER BY seq LIMIT ?1"
    ))?
    .query_map(params![limit as i64], entry_from_row)?
    .collect()
}

/// Pending entries of one scope in local order.
pub fn pending_in_scope(
    conn: &Connection,
    scope: &str,
    limit: usize,
) -> rusqlite::Result<Vec<OutboxEntry>> {
    conn.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM _sync_outbox WHERE state = 'pending' AND scope = ?1 ORDER BY seq LIMIT ?2"
    ))?
    .query_map(params![scope, limit as i64], entry_from_row)?
    .collect()
}

/// All unsent entries, pending or sent, of one row, in local order.
pub fn entries_for_row(
    conn: &Connection,
    tbl: &str,
    row_id: &str,
) -> rusqlite::Result<Vec<OutboxEntry>> {
    conn.prepare(&format!(
        "SELECT {ENTRY_COLS} FROM _sync_outbox WHERE tbl = ?1 AND row_id = ?2 ORDER BY seq"
    ))?
    .query_map(params![tbl, row_id], entry_from_row)?
    .collect()
}

pub fn count_unsent(conn: &Connection) -> rusqlite::Result<i64> {
    conn.query_row("SELECT COUNT(*) FROM _sync_outbox", [], |r| r.get(0))
}

pub fn delete_entries(conn: &Connection, seqs: &[i64]) -> rusqlite::Result<()> {
    let mut stmt = conn.prepare("DELETE FROM _sync_outbox WHERE seq = ?1")?;
    for s in seqs {
        stmt.execute(params![s])?;
    }
    Ok(())
}

/// Remove pending entries of a row. Returns how many were removed.
pub fn delete_pending_for_row(
    conn: &Connection,
    tbl: &str,
    row_id: &str,
) -> rusqlite::Result<usize> {
    conn.execute(
        "DELETE FROM _sync_outbox WHERE tbl = ?1 AND row_id = ?2 AND state = 'pending'",
        params![tbl, row_id],
    )
}

/// Point every unsent entry of a row at a new base version.
pub fn rebase_row(conn: &Connection, tbl: &str, row_id: &str, base: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE _sync_outbox SET base_version = ?3 WHERE tbl = ?1 AND row_id = ?2 AND state = 'pending'",
        params![tbl, row_id, base],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ batches

/// A batch that was sent and whose results have not been applied yet.
#[derive(Debug, Clone)]
pub struct SentBatch {
    pub batch_id: i64,
    pub scope: String,
    /// The exact request body, so a resend is byte-identical.
    pub body: String,
    /// change_id to the outbox entries it stands for, in batch order.
    pub map: Vec<(String, Vec<i64>)>,
}

pub fn sent_batch(conn: &Connection) -> Result<Option<SentBatch>, StoreError> {
    let row: Option<(i64, String, String, String)> = conn
        .query_row(
            "SELECT batch_id, scope, body, map FROM _sync_batches ORDER BY batch_id LIMIT 1",
            [],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    Ok(match row {
        Some((batch_id, scope, body, map)) => Some(SentBatch {
            batch_id,
            scope,
            body,
            map: serde_json::from_str(&map)?,
        }),
        None => None,
    })
}

/// Record a batch as sent and freeze its entries. Returns the batch ID.
pub fn record_sent(
    conn: &Connection,
    scope: &str,
    body: &str,
    map: &[(String, Vec<i64>)],
) -> Result<i64, StoreError> {
    conn.execute(
        "INSERT INTO _sync_batches (scope, body, map) VALUES (?1, ?2, ?3)",
        params![scope, body, serde_json::to_string(map)?],
    )?;
    let id = conn.last_insert_rowid();
    let mut stmt =
        conn.prepare("UPDATE _sync_outbox SET state = 'sent', batch_id = ?2 WHERE seq = ?1")?;
    for (_, seqs) in map {
        for s in seqs {
            stmt.execute(params![s, id])?;
        }
    }
    Ok(id)
}

/// Forget a sent batch; its entries go back to pending unless deleted.
pub fn finish_batch(conn: &Connection, batch_id: i64) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE _sync_outbox SET state = 'pending', batch_id = NULL WHERE batch_id = ?1",
        params![batch_id],
    )?;
    conn.execute(
        "DELETE FROM _sync_batches WHERE batch_id = ?1",
        params![batch_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ rows

#[derive(Debug, Clone, PartialEq)]
pub struct RowState {
    pub scope: String,
    /// The version the local row was last refreshed from.
    pub pulled_version: i64,
    /// The newest version known to include this device's own changes.
    pub known_version: i64,
    pub mark: i64,
}

impl RowState {
    pub fn base(&self) -> i64 {
        self.pulled_version.max(self.known_version)
    }
}

pub fn row_state(conn: &Connection, tbl: &str, row_id: &str) -> rusqlite::Result<Option<RowState>> {
    conn.query_row(
        "SELECT scope, pulled_version, known_version, mark FROM _sync_rows WHERE tbl = ?1 AND row_id = ?2",
        params![tbl, row_id],
        |r| {
            Ok(RowState {
                scope: r.get(0)?,
                pulled_version: r.get(1)?,
                known_version: r.get(2)?,
                mark: r.get(3)?,
            })
        },
    )
    .optional()
}

/// The local row now reflects server version `version`.
pub fn set_pulled(
    conn: &Connection,
    tbl: &str,
    row_id: &str,
    scope: &str,
    version: i64,
    mark: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _sync_rows (tbl, row_id, scope, pulled_version, known_version, mark)
         VALUES (?1, ?2, ?3, ?4, ?4, ?5)
         ON CONFLICT (tbl, row_id) DO UPDATE SET
             scope = excluded.scope,
             pulled_version = excluded.pulled_version,
             known_version = CASE WHEN scope = excluded.scope
                                  THEN MAX(known_version, excluded.pulled_version)
                                  ELSE excluded.pulled_version END,
             mark = excluded.mark",
        params![tbl, row_id, scope, version, mark],
    )?;
    Ok(())
}

/// The server holds this device's change at `version`; the local row may
/// not yet reflect other changes merged into it.
pub fn set_known(
    conn: &Connection,
    tbl: &str,
    row_id: &str,
    scope: &str,
    version: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _sync_rows (tbl, row_id, scope, pulled_version, known_version)
         VALUES (?1, ?2, ?3, 0, ?4)
         ON CONFLICT (tbl, row_id) DO UPDATE SET known_version = MAX(known_version, excluded.known_version)",
        params![tbl, row_id, scope, version],
    )?;
    Ok(())
}

pub fn delete_row_state(conn: &Connection, tbl: &str, row_id: &str) -> rusqlite::Result<()> {
    conn.execute(
        "DELETE FROM _sync_rows WHERE tbl = ?1 AND row_id = ?2",
        params![tbl, row_id],
    )?;
    Ok(())
}

// ------------------------------------------------------------------ scopes

#[derive(Debug, Clone, PartialEq)]
pub struct ScopeState {
    pub scope: String,
    pub access: Access,
    pub cursor: Option<String>,
    /// `active` or `resyncing`.
    pub state: String,
    pub resync_gen: i64,
}

pub fn scopes(conn: &Connection) -> rusqlite::Result<Vec<ScopeState>> {
    conn.prepare(
        "SELECT scope, access, cursor, state, resync_gen FROM _sync_scopes ORDER BY scope",
    )?
    .query_map([], |r| {
        let access: String = r.get(1)?;
        Ok(ScopeState {
            scope: r.get(0)?,
            access: Access::parse(&access).unwrap_or(Access::Read),
            cursor: r.get(2)?,
            state: r.get(3)?,
            resync_gen: r.get(4)?,
        })
    })?
    .collect()
}

pub fn scope_state(conn: &Connection, scope: &str) -> rusqlite::Result<Option<ScopeState>> {
    Ok(scopes(conn)?.into_iter().find(|s| s.scope == scope))
}

pub fn upsert_scope(conn: &Connection, scope: &str, access: Access) -> rusqlite::Result<()> {
    conn.execute(
        "INSERT INTO _sync_scopes (scope, access) VALUES (?1, ?2)
         ON CONFLICT (scope) DO UPDATE SET access = excluded.access",
        params![scope, access.as_str()],
    )?;
    Ok(())
}

pub fn set_cursor(conn: &Connection, scope: &str, cursor: Option<&str>) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE _sync_scopes SET cursor = ?2 WHERE scope = ?1",
        params![scope, cursor],
    )?;
    Ok(())
}

pub fn set_scope_state(
    conn: &Connection,
    scope: &str,
    state: &str,
    resync_gen: i64,
) -> rusqlite::Result<()> {
    conn.execute(
        "UPDATE _sync_scopes SET state = ?2, resync_gen = ?3 WHERE scope = ?1",
        params![scope, state, resync_gen],
    )?;
    Ok(())
}

pub fn delete_scope(conn: &Connection, scope: &str) -> rusqlite::Result<()> {
    conn.execute("DELETE FROM _sync_scopes WHERE scope = ?1", params![scope])?;
    Ok(())
}

// ------------------------------------------------------------------ scope removal

/// Remove the engine's state for a scope: its unsent changes, sent batches,
/// row versions and the scope itself. Returns how many unsent changes were
/// dropped. The app's rows are removed separately with
/// [`crate::delete_scope_rows`].
pub fn purge_scope(conn: &Connection, scope: &str) -> rusqlite::Result<i64> {
    let dropped: i64 = conn.query_row(
        "SELECT COUNT(*) FROM _sync_outbox WHERE scope = ?1",
        params![scope],
        |r| r.get(0),
    )?;
    conn.execute("DELETE FROM _sync_outbox WHERE scope = ?1", params![scope])?;
    conn.execute("DELETE FROM _sync_batches WHERE scope = ?1", params![scope])?;
    conn.execute("DELETE FROM _sync_rows WHERE scope = ?1", params![scope])?;
    conn.execute("DELETE FROM _sync_scopes WHERE scope = ?1", params![scope])?;
    Ok(dropped)
}

/// Forget all sync state. Used on sign-out.
pub fn clear_all(conn: &Connection) -> rusqlite::Result<()> {
    conn.execute_batch(
        "DELETE FROM _sync_outbox; DELETE FROM _sync_batches; DELETE FROM _sync_rows; DELETE FROM _sync_scopes;",
    )
}

/// Pending entries of scopes other than `writable`, in local order.
pub fn pending_outside(
    conn: &Connection,
    writable: &[String],
) -> rusqlite::Result<Vec<OutboxEntry>> {
    Ok(pending(conn, i64::MAX as usize)?
        .into_iter()
        .filter(|e| !writable.contains(&e.scope))
        .collect())
}
