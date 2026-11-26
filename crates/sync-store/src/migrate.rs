//! Local schema migrations that keep unsent changes valid
//! (`docs/04-client-engine.md`, section 4.9).

use crate::StoreError;
use rusqlite::{params, Connection};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Migration {
    /// Positive, strictly increasing across the list.
    pub version: i64,
    pub sql: String,
    /// How unsent changes must follow the schema change.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outbox: Vec<OutboxRewrite>,
}

impl Migration {
    pub fn sql(version: i64, sql: impl Into<String>) -> Migration {
        Migration {
            version,
            sql: sql.into(),
            outbox: Vec::new(),
        }
    }

    pub fn with(mut self, rewrite: OutboxRewrite) -> Migration {
        self.outbox.push(rewrite);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum OutboxRewrite {
    RenameTable {
        from: String,
        to: String,
    },
    RenameColumn {
        table: String,
        from: String,
        to: String,
    },
    /// Removes the field from unsent changes. Updates left with no fields
    /// are dropped; creations are kept.
    DropColumn {
        table: String,
        column: String,
    },
    /// Drops unsent changes of the table.
    DropTable {
        table: String,
    },
}

pub(crate) fn current_version(conn: &Connection) -> Result<i64, StoreError> {
    Ok(crate::state::meta_get(conn, "migration_version")?
        .and_then(|v| v.parse().ok())
        .unwrap_or(0))
}

pub(crate) fn validate(migrations: &[Migration]) -> Result<(), StoreError> {
    let mut last = 0;
    for m in migrations {
        if m.version <= last {
            return Err(StoreError::Migration(format!(
                "migration versions must be positive and increasing; {} follows {last}",
                m.version
            )));
        }
        last = m.version;
    }
    Ok(())
}

/// Apply the migrations newer than the stored version. Runs inside the
/// caller's transaction, with change capture off.
pub(crate) fn apply(conn: &Connection, migrations: &[Migration]) -> Result<usize, StoreError> {
    let current = current_version(conn)?;
    let mut applied = 0;
    for m in migrations.iter().filter(|m| m.version > current) {
        conn.execute_batch(&m.sql)
            .map_err(|e| StoreError::Migration(format!("migration {}: {e}", m.version)))?;
        for r in &m.outbox {
            rewrite(conn, r)?;
        }
        crate::state::meta_set(conn, "migration_version", &m.version.to_string())?;
        applied += 1;
    }
    Ok(applied)
}

fn rewrite(conn: &Connection, r: &OutboxRewrite) -> Result<(), StoreError> {
    match r {
        OutboxRewrite::RenameTable { from, to } => {
            conn.execute(
                "UPDATE _sync_outbox SET tbl = ?2 WHERE tbl = ?1",
                params![from, to],
            )?;
            conn.execute(
                "UPDATE _sync_rows SET tbl = ?2 WHERE tbl = ?1",
                params![from, to],
            )?;
        }
        OutboxRewrite::DropTable { table } => {
            conn.execute(
                "DELETE FROM _sync_outbox WHERE tbl = ?1 AND state = 'pending'",
                params![table],
            )?;
            conn.execute("DELETE FROM _sync_rows WHERE tbl = ?1", params![table])?;
        }
        OutboxRewrite::RenameColumn { table, from, to } => {
            edit_fields(conn, table, |fields, _| {
                if let Some(v) = fields.remove(from) {
                    fields.insert(to.clone(), v);
                }
                true
            })?;
        }
        OutboxRewrite::DropColumn { table, column } => {
            edit_fields(conn, table, |fields, is_update| {
                fields.remove(column);
                !(is_update && fields.is_empty())
            })?;
        }
    }
    Ok(())
}

/// Edit the fields of every pending upsert of `table`. The closure returns
/// false to drop the entry. Entries already sent are never touched: the
/// server may have applied them under their change ID.
fn edit_fields(
    conn: &Connection,
    table: &str,
    mut f: impl FnMut(&mut serde_json::Map<String, Value>, bool) -> bool,
) -> Result<(), StoreError> {
    let rows: Vec<(i64, String, Option<i64>)> = conn
        .prepare("SELECT seq, fields, base_version FROM _sync_outbox WHERE tbl = ?1 AND op = 'upsert' AND state = 'pending'")?
        .query_map(params![table], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .collect::<Result<_, _>>()?;
    for (seq, text, base) in rows {
        let mut fields = match serde_json::from_str::<Value>(&text)? {
            Value::Object(m) => m,
            _ => continue,
        };
        if f(&mut fields, base.is_some()) {
            conn.execute(
                "UPDATE _sync_outbox SET fields = ?2 WHERE seq = ?1",
                params![seq, Value::Object(fields).to_string()],
            )?;
        } else {
            conn.execute("DELETE FROM _sync_outbox WHERE seq = ?1", params![seq])?;
        }
    }
    Ok(())
}
