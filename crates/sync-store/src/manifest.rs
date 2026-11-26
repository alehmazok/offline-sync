//! The app's description of its synced data (`docs/04-client-engine.md`,
//! section 4.2).

use crate::StoreError;
use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sync_core::protocol::Policy;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Manifest {
    pub schema_version: i64,
    pub tables: Vec<TableSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TableSpec {
    pub name: String,
    /// Columns that sync. Other columns of the table are local-only.
    pub columns: Vec<String>,
    /// Columns among `columns` that hold JSON text and sync as JSON values
    /// (objects, arrays) rather than as strings.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub json_columns: Vec<String>,
    pub scope: ScopeRule,
    #[serde(default)]
    pub conflict: Policy,
}

/// How a row maps to its scope.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum ScopeRule {
    /// `{"column": "scope_id"}`: this column holds the scope ID.
    Column { column: String },
    /// `"default"`: the default scope from the engine configuration.
    Default(DefaultScope),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultScope {
    Default,
}

impl ScopeRule {
    pub fn column(&self) -> Option<&str> {
        match self {
            ScopeRule::Column { column } => Some(column),
            ScopeRule::Default(_) => None,
        }
    }
}

impl Manifest {
    pub fn from_json(text: &str) -> Result<Manifest, StoreError> {
        serde_json::from_str(text).map_err(|e| StoreError::Manifest(format!("invalid manifest: {e}")))
    }

    pub fn table(&self, name: &str) -> Option<&TableSpec> {
        self.tables.iter().find(|t| t.name == name)
    }

    pub fn uses_default_scope(&self) -> bool {
        self.tables.iter().any(|t| t.scope.column().is_none())
    }

    /// Check the manifest on its own, before any table is looked at.
    pub fn validate(&self) -> Result<(), StoreError> {
        let err = |m: String| Err(StoreError::Manifest(m));
        let mut seen = std::collections::HashSet::new();
        for t in &self.tables {
            if !sync_core::is_valid_table_name(&t.name) || t.name.starts_with("_sync") {
                return err(format!("invalid table name {:?}", t.name));
            }
            if !seen.insert(&t.name) {
                return err(format!("table {} listed twice", t.name));
            }
            let mut cols = std::collections::HashSet::new();
            for c in &t.columns {
                if !sync_core::is_valid_field_name(c) {
                    return err(format!("{}: invalid column name {c:?}", t.name));
                }
                if c == "id" {
                    return err(format!("{}: `id` is implicit and must not be listed", t.name));
                }
                if Some(c.as_str()) == t.scope.column() {
                    return err(format!("{}: the scope column must not be listed as a synced column", t.name));
                }
                if !cols.insert(c) {
                    return err(format!("{}: column {c} listed twice", t.name));
                }
            }
            for c in &t.json_columns {
                if !t.columns.contains(c) {
                    return err(format!("{}: json column {c} is not a synced column", t.name));
                }
            }
        }
        Ok(())
    }

    /// Check the manifest against the actual tables in the database.
    pub fn check_against(&self, conn: &Connection) -> Result<(), StoreError> {
        for t in &self.tables {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{}\")", t.name))?;
            let cols: Vec<(String, i64)> = stmt
                .query_map([], |r| Ok((r.get::<_, String>(1)?, r.get::<_, i64>(5)?)))?
                .collect::<Result<_, _>>()?;
            if cols.is_empty() {
                return Err(StoreError::Manifest(format!("table {} does not exist; create it in a migration", t.name)));
            }
            let pk: Vec<&str> = cols.iter().filter(|(_, pk)| *pk > 0).map(|(n, _)| n.as_str()).collect();
            if pk != ["id"] {
                return Err(StoreError::Manifest(format!("{}: the primary key must be a single column named id", t.name)));
            }
            let names: Vec<&str> = cols.iter().map(|(n, _)| n.as_str()).collect();
            for c in t.columns.iter().map(String::as_str).chain(t.scope.column()) {
                if !names.contains(&c) {
                    return Err(StoreError::Manifest(format!("{}: column {c} does not exist", t.name)));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_the_documented_example() {
        let m = Manifest::from_json(
            r#"{
              "schema_version": 7,
              "tables": [
                {"name": "stores", "columns": ["name"], "scope": {"column": "scope_id"}, "conflict": "server_wins"},
                {"name": "settings", "columns": ["theme"], "scope": "default"}
              ]
            }"#,
        )
        .unwrap();
        assert_eq!(m.tables[0].scope.column(), Some("scope_id"));
        assert_eq!(m.tables[0].conflict, Policy::ServerWins);
        assert_eq!(m.tables[1].scope, ScopeRule::Default(DefaultScope::Default));
        assert_eq!(m.tables[1].conflict, Policy::FieldMerge);
        m.validate().unwrap();
    }

    #[test]
    fn rejects_bad_manifests() {
        let bad = [
            r#"{"schema_version":1,"tables":[{"name":"T","columns":[],"scope":"default"}]}"#,
            r#"{"schema_version":1,"tables":[{"name":"t","columns":["id"],"scope":"default"}]}"#,
            r#"{"schema_version":1,"tables":[{"name":"t","columns":["s"],"scope":{"column":"s"}}]}"#,
            r#"{"schema_version":1,"tables":[{"name":"t","columns":["a","a"],"scope":"default"}]}"#,
            r#"{"schema_version":1,"tables":[{"name":"t","columns":["a"],"json_columns":["b"],"scope":"default"}]}"#,
        ];
        for b in bad {
            assert!(Manifest::from_json(b).unwrap().validate().is_err(), "{b}");
        }
    }
}
