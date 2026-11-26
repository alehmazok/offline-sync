//! Local SQLite storage for offline-sync.
//!
//! [`Store`] owns the database file. It runs the app's migrations, installs
//! the change-capture triggers for the tables in the [`Manifest`], and
//! offers the reads and writes the engine needs on its own tables. It knows
//! nothing about the network.

pub mod manifest;
pub mod migrate;
pub mod state;
pub mod triggers;
mod value;

pub use manifest::{DefaultScope, Manifest, ScopeRule, TableSpec};
pub use migrate::{Migration, OutboxRewrite};
pub use rusqlite;
pub use value::{json_to_sql, QueryResult, SqlValue};

use rusqlite::functions::FunctionFlags;
use rusqlite::hooks::{AuthAction, AuthContext, Authorization};
use rusqlite::{params_from_iter, Connection, Transaction};
use std::collections::BTreeSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use sync_core::protocol::Fields;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("{0}")]
    Manifest(String),
    #[error("{0}")]
    Migration(String),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    Invalid(String),
}

pub type Result<T> = std::result::Result<T, StoreError>;

pub struct Store {
    conn: Connection,
    manifest: Manifest,
    capture: Arc<AtomicBool>,
    changed: Arc<Mutex<BTreeSet<String>>>,
}

impl Store {
    /// Open or create the database at `path` (`":memory:"` for tests), run
    /// pending migrations and install triggers for `manifest`.
    pub fn open(
        path: &str,
        manifest: Manifest,
        migrations: &[Migration],
        default_scope: Option<&str>,
    ) -> Result<Store> {
        manifest.validate()?;
        migrate::validate(migrations)?;
        if manifest.uses_default_scope() && default_scope.is_none() {
            return Err(StoreError::Manifest(
                "a table uses the default scope but none is configured".into(),
            ));
        }
        if let Some(s) = default_scope {
            if !sync_core::is_valid_scope_id(s) {
                return Err(StoreError::Manifest(format!("invalid default scope {s:?}")));
            }
        }

        let conn = Connection::open(path)?;
        if path != ":memory:" {
            conn.pragma_update(None, "journal_mode", "WAL")?;
            conn.pragma_update(None, "synchronous", "NORMAL")?;
        }
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;

        let capture = Arc::new(AtomicBool::new(true));
        {
            let capture = capture.clone();
            conn.create_scalar_function(
                "sync_capture",
                0,
                FunctionFlags::SQLITE_UTF8,
                move |_| Ok(capture.load(Ordering::SeqCst) as i64),
            )?;
        }
        conn.create_scalar_function("sync_new_id", 0, FunctionFlags::SQLITE_UTF8, |_| {
            Ok(sync_core::new_id())
        })?;
        conn.execute_batch(state::SCHEMA)?;

        let mut store = Store {
            conn,
            manifest,
            capture,
            changed: Arc::new(Mutex::new(BTreeSet::new())),
        };

        store.with_capture_off(|tx| {
            if state::meta_get(tx, "device_id")?.is_none() {
                state::meta_set(tx, "device_id", &uuid_v4())?;
            }
            match default_scope {
                Some(s) => state::meta_set(tx, "default_scope", s)?,
                None => state::meta_delete(tx, "default_scope")?,
            }
            // Triggers can block schema changes to the columns they use, so
            // they are removed while migrations run and installed after.
            triggers::install(
                tx,
                &Manifest {
                    schema_version: 0,
                    tables: vec![],
                },
            )?;
            let n = migrate::apply(tx, migrations)?;
            if n > 0 {
                log::info!("applied {n} local migration(s)");
            }
            Ok(())
        })?;
        store.manifest.check_against(&store.conn)?;
        triggers::install(&store.conn, &store.manifest)?;

        let changed = store.changed.clone();
        store
            .conn
            .update_hook(Some(move |_action, _db: &str, table: &str, _rowid| {
                if !table.starts_with("_sync_") && !table.starts_with("sqlite_") {
                    changed.lock().unwrap().insert(table.to_string());
                }
            }))?;
        // Writes made while opening (migrations) do not count as changes.
        store.changed.lock().unwrap().clear();
        Ok(store)
    }

    pub fn manifest(&self) -> &Manifest {
        &self.manifest
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn device_id(&self) -> Result<String> {
        Ok(state::meta_get(&self.conn, "device_id")?.unwrap_or_default())
    }

    pub fn default_scope(&self) -> Result<Option<String>> {
        Ok(state::meta_get(&self.conn, "default_scope")?)
    }

    // -------------------------------------------------------------- app API

    /// Run one statement with change capture on. Returns the number of rows
    /// changed.
    pub fn execute(&self, sql: &str, params: &[SqlValue]) -> Result<usize> {
        self.check_not_internal(sql)?;
        Ok(self.conn.execute(sql, params_from_iter(params.iter()))?)
    }

    /// Run several statements in one transaction, with change capture on.
    pub fn write_batch(&mut self, statements: &[(String, Vec<SqlValue>)]) -> Result<()> {
        for (sql, _) in statements {
            self.check_not_internal(sql)?;
        }
        let tx = self.conn.transaction()?;
        for (sql, params) in statements {
            tx.execute(sql, params_from_iter(params.iter()))?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn query(&self, sql: &str, params: &[SqlValue]) -> Result<QueryResult> {
        let mut stmt = self.conn.prepare(sql)?;
        let columns: Vec<String> = stmt.column_names().map(|s| s.to_string()).collect();
        let n = columns.len();
        let rows = stmt
            .query_map(params_from_iter(params.iter()), |r| {
                (0..n)
                    .map(|i| r.get::<_, SqlValue>(i))
                    .collect::<rusqlite::Result<Vec<_>>>()
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(QueryResult { columns, rows })
    }

    /// The tables a query reads, found by preparing it under an authorizer.
    pub fn tables_read_by(&mut self, sql: &str) -> Result<BTreeSet<String>> {
        let tables = Arc::new(Mutex::new(BTreeSet::new()));
        {
            let tables = tables.clone();
            self.conn.authorizer(Some(move |ctx: AuthContext<'_>| {
                if let AuthAction::Read { table_name, .. } = ctx.action {
                    if !table_name.starts_with("sqlite_") {
                        tables.lock().unwrap().insert(table_name.to_string());
                    }
                }
                Authorization::Allow
            }))?;
        }
        let prepared = self.conn.prepare(sql).map(|_| ());
        self.conn
            .authorizer(None::<fn(AuthContext<'_>) -> Authorization>)?;
        prepared?;
        let out = tables.lock().unwrap().clone();
        Ok(out)
    }

    /// Tables changed since the last call, other than the engine's own.
    pub fn take_changed_tables(&self) -> BTreeSet<String> {
        std::mem::take(&mut *self.changed.lock().unwrap())
    }

    fn check_not_internal(&self, sql: &str) -> Result<()> {
        if sql.to_ascii_lowercase().contains("_sync_") {
            return Err(StoreError::Invalid(
                "statements must not touch the engine's _sync_ tables".into(),
            ));
        }
        Ok(())
    }

    // -------------------------------------------------------------- engine API

    /// Run `f` in a transaction with change capture off. Used for every
    /// write of data that came from the server.
    pub fn with_capture_off<T>(&mut self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        self.capture_off(f)
    }

    /// [`Store::with_capture_off`] for callers with their own error type.
    pub fn capture_off<T, E: From<StoreError>>(
        &mut self,
        f: impl FnOnce(&Transaction) -> std::result::Result<T, E>,
    ) -> std::result::Result<T, E> {
        self.capture.store(false, Ordering::SeqCst);
        let result = (|| {
            let tx = self.conn.transaction().map_err(StoreError::from)?;
            let v = f(&tx)?;
            tx.commit().map_err(StoreError::from)?;
            Ok(v)
        })();
        self.capture.store(true, Ordering::SeqCst);
        result
    }

    /// Run `f` in a transaction with change capture on.
    pub fn transaction<T>(&mut self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let tx = self.conn.transaction()?;
        let v = f(&tx)?;
        tx.commit()?;
        Ok(v)
    }
}

// ------------------------------------------------------------------ row writes

/// Write a record received from the server into its local table. Only the
/// synced columns present in `data` are written; local-only columns are
/// left alone.
pub fn write_record(
    conn: &Connection,
    spec: &TableSpec,
    scope: &str,
    id: &str,
    data: &Fields,
) -> Result<()> {
    let mut cols = vec!["id".to_string()];
    let mut vals = vec![SqlValue::Text(id.to_string())];
    if let Some(sc) = spec.scope.column() {
        cols.push(sc.to_string());
        vals.push(SqlValue::Text(scope.to_string()));
    }
    let mut updates = Vec::new();
    for c in &spec.columns {
        if let Some(v) = data.get(c) {
            cols.push(c.clone());
            vals.push(json_to_sql(v, spec.json_columns.contains(c)));
            updates.push(format!("\"{c}\" = excluded.\"{c}\""));
        }
    }
    let placeholders: Vec<String> = (1..=cols.len()).map(|i| format!("?{i}")).collect();
    let quoted: Vec<String> = cols.iter().map(|c| format!("\"{c}\"")).collect();
    let conflict = if updates.is_empty() {
        "DO NOTHING".to_string()
    } else {
        format!("DO UPDATE SET {}", updates.join(", "))
    };
    let sql = format!(
        "INSERT INTO \"{}\" ({}) VALUES ({}) ON CONFLICT (id) {conflict}",
        spec.name,
        quoted.join(", "),
        placeholders.join(", ")
    );
    conn.execute(&sql, params_from_iter(vals.iter()))?;
    Ok(())
}

/// Set some synced fields of an existing local row.
pub fn update_fields(conn: &Connection, spec: &TableSpec, id: &str, fields: &Fields) -> Result<()> {
    let mut sets = Vec::new();
    let mut vals = Vec::new();
    for (k, v) in fields {
        if spec.columns.contains(k) {
            vals.push(json_to_sql(v, spec.json_columns.contains(k)));
            sets.push(format!("\"{k}\" = ?{}", vals.len()));
        }
    }
    if sets.is_empty() {
        return Ok(());
    }
    vals.push(SqlValue::Text(id.to_string()));
    let sql = format!(
        "UPDATE \"{}\" SET {} WHERE id = ?{}",
        spec.name,
        sets.join(", "),
        vals.len()
    );
    conn.execute(&sql, params_from_iter(vals.iter()))?;
    Ok(())
}

pub fn delete_row(conn: &Connection, spec: &TableSpec, id: &str) -> Result<()> {
    conn.execute(
        &format!("DELETE FROM \"{}\" WHERE id = ?1", spec.name),
        [id],
    )?;
    Ok(())
}

pub fn row_exists(conn: &Connection, spec: &TableSpec, id: &str) -> Result<bool> {
    Ok(conn
        .prepare(&format!("SELECT 1 FROM \"{}\" WHERE id = ?1", spec.name))?
        .exists([id])?)
}

/// Delete every local row of `scope` in every synced table.
pub fn delete_scope_rows(
    conn: &Connection,
    manifest: &Manifest,
    scope: &str,
    default_scope: Option<&str>,
) -> Result<()> {
    for t in &manifest.tables {
        match t.scope.column() {
            Some(c) => {
                conn.execute(
                    &format!("DELETE FROM \"{}\" WHERE \"{c}\" = ?1", t.name),
                    [scope],
                )?;
            }
            None if default_scope == Some(scope) => {
                conn.execute(&format!("DELETE FROM \"{}\"", t.name), [])?;
            }
            None => {}
        }
    }
    Ok(())
}

fn uuid_v4() -> String {
    // A v7 ID is fine here too; the device ID only has to be unique.
    sync_core::new_id()
}

#[cfg(test)]
mod tests;
