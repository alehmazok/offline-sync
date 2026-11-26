//! Change capture by generated SQLite triggers
//! (`docs/04-client-engine.md`, section 4.4).
//!
//! Each synced table gets triggers that append to `_sync_outbox` in the
//! same transaction as the write. They fire only while the connection
//! function `sync_capture()` returns 1; the engine switches it off while it
//! writes data received from the server.

use crate::manifest::{Manifest, TableSpec};
use rusqlite::Connection;

pub const PREFIX: &str = "_sync_trg_";

fn q(ident: &str) -> String {
    format!("\"{}\"", ident.replace('"', "\"\""))
}

fn lit(s: &str) -> String {
    format!("'{}'", s.replace('\'', "''"))
}

/// Value of a column for `json_object`: JSON columns are parsed so they
/// travel as JSON values.
fn col_value(t: &TableSpec, row: &str, c: &str) -> String {
    if t.json_columns.iter().any(|j| j == c) {
        format!("json({row}.{})", q(c))
    } else {
        format!("{row}.{}", q(c))
    }
}

fn scope_expr(t: &TableSpec, row: &str) -> String {
    match t.scope.column() {
        Some(c) => format!("{row}.{}", q(c)),
        None => "(SELECT value FROM _sync_meta WHERE key = 'default_scope')".to_string(),
    }
}

fn base_expr(t: &TableSpec, row: &str) -> String {
    format!(
        "(SELECT MAX(pulled_version, known_version) FROM _sync_rows WHERE tbl = {} AND row_id = {row}.id)",
        lit(&t.name)
    )
}

fn insert_outbox(t: &TableSpec, row: &str, op: &str, fields: &str) -> String {
    format!(
        "INSERT INTO _sync_outbox (change_id, scope, tbl, row_id, op, fields, base_version) \
         VALUES (sync_new_id(), {scope}, {tbl}, {row}.id, '{op}', {fields}, {base});",
        scope = scope_expr(t, row),
        tbl = lit(&t.name),
        base = base_expr(t, row),
    )
}

/// The SQL that creates every trigger for one table.
pub fn table_triggers(t: &TableSpec) -> Vec<String> {
    let name = |kind: &str| q(&format!("{PREFIX}{}_{kind}", t.name));
    let table = q(&t.name);
    let mut out = Vec::new();

    // Guards that hold even while capture is off.
    let mut guard = "OLD.id IS NOT NEW.id".to_string();
    if let Some(sc) = t.scope.column() {
        guard.push_str(&format!(" OR OLD.{c} IS NOT NEW.{c}", c = q(sc)));
        out.push(format!(
            "CREATE TRIGGER {} BEFORE INSERT ON {table} WHEN NEW.{c} IS NULL BEGIN \
             SELECT RAISE(ABORT, 'sync: scope column {sc} of {t} must be set'); END;",
            name("scope"),
            c = q(sc),
            t = t.name,
        ));
    }
    out.push(format!(
        "CREATE TRIGGER {} BEFORE UPDATE ON {table} WHEN {guard} BEGIN \
         SELECT RAISE(ABORT, 'sync: id and scope of a synced row cannot change'); END;",
        name("guard")
    ));

    let all_fields = if t.columns.is_empty() {
        "'{}'".to_string()
    } else {
        let pairs: Vec<String> = t
            .columns
            .iter()
            .map(|c| format!("{}, {}", lit(c), col_value(t, "NEW", c)))
            .collect();
        format!("json_object({})", pairs.join(", "))
    };

    out.push(format!(
        "CREATE TRIGGER {} AFTER INSERT ON {table} WHEN sync_capture() BEGIN {} END;",
        name("ins"),
        insert_outbox(t, "NEW", "upsert", &all_fields)
    ));

    if !t.columns.is_empty() {
        let changed: Vec<String> = t
            .columns
            .iter()
            .map(|c| format!("OLD.{c} IS NOT NEW.{c}", c = q(c)))
            .collect();
        // Start from every column and remove the unchanged ones; removing a
        // path that does not exist is a no-op.
        let removals: Vec<String> = t
            .columns
            .iter()
            .map(|c| {
                format!(
                    "CASE WHEN OLD.{qc} IS NEW.{qc} THEN '$.{c}' ELSE '$.__none' END",
                    qc = q(c)
                )
            })
            .collect();
        let fields = format!("json_remove({all_fields}, {})", removals.join(", "));
        out.push(format!(
            "CREATE TRIGGER {} AFTER UPDATE ON {table} WHEN sync_capture() AND ({}) BEGIN {} END;",
            name("upd"),
            changed.join(" OR "),
            insert_outbox(t, "NEW", "upsert", &fields)
        ));
    }

    out.push(format!(
        "CREATE TRIGGER {} AFTER DELETE ON {table} WHEN sync_capture() BEGIN {} END;",
        name("del"),
        insert_outbox(t, "OLD", "delete", "'{}'")
    ));
    out
}

/// Drop every engine trigger and create the ones the manifest needs.
pub fn install(conn: &Connection, manifest: &Manifest) -> rusqlite::Result<()> {
    let existing: Vec<String> = conn
        .prepare(
            "SELECT name FROM sqlite_master WHERE type = 'trigger' AND substr(name, 1, ?1) = ?2",
        )?
        .query_map(rusqlite::params![PREFIX.len() as i64, PREFIX], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    for name in existing {
        conn.execute_batch(&format!("DROP TRIGGER IF EXISTS {};", q(&name)))?;
    }
    for t in &manifest.tables {
        for sql in table_triggers(t) {
            conn.execute_batch(&sql)?;
        }
    }
    Ok(())
}
