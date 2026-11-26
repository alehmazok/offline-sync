use rusqlite::types::{FromSql, FromSqlResult, ToSql, ToSqlOutput, Value as RValue, ValueRef};
use serde_json::Value as Json;

/// A SQLite value as it crosses the engine's public API.
#[derive(Debug, Clone, PartialEq)]
pub enum SqlValue {
    Null,
    Integer(i64),
    Real(f64),
    Text(String),
    Blob(Vec<u8>),
}

impl ToSql for SqlValue {
    fn to_sql(&self) -> rusqlite::Result<ToSqlOutput<'_>> {
        Ok(match self {
            SqlValue::Null => ToSqlOutput::Owned(RValue::Null),
            SqlValue::Integer(i) => ToSqlOutput::Owned(RValue::Integer(*i)),
            SqlValue::Real(f) => ToSqlOutput::Owned(RValue::Real(*f)),
            SqlValue::Text(s) => ToSqlOutput::Borrowed(ValueRef::Text(s.as_bytes())),
            SqlValue::Blob(b) => ToSqlOutput::Borrowed(ValueRef::Blob(b)),
        })
    }
}

impl FromSql for SqlValue {
    fn column_result(v: ValueRef<'_>) -> FromSqlResult<Self> {
        Ok(match v {
            ValueRef::Null => SqlValue::Null,
            ValueRef::Integer(i) => SqlValue::Integer(i),
            ValueRef::Real(f) => SqlValue::Real(f),
            ValueRef::Text(t) => SqlValue::Text(String::from_utf8_lossy(t).into_owned()),
            ValueRef::Blob(b) => SqlValue::Blob(b.to_vec()),
        })
    }
}

impl From<&str> for SqlValue {
    fn from(s: &str) -> Self {
        SqlValue::Text(s.to_string())
    }
}
impl From<String> for SqlValue {
    fn from(s: String) -> Self {
        SqlValue::Text(s)
    }
}
impl From<i64> for SqlValue {
    fn from(i: i64) -> Self {
        SqlValue::Integer(i)
    }
}
impl From<f64> for SqlValue {
    fn from(f: f64) -> Self {
        SqlValue::Real(f)
    }
}
impl From<bool> for SqlValue {
    fn from(b: bool) -> Self {
        SqlValue::Integer(b as i64)
    }
}
impl<T: Into<SqlValue>> From<Option<T>> for SqlValue {
    fn from(v: Option<T>) -> Self {
        v.map(Into::into).unwrap_or(SqlValue::Null)
    }
}

/// Convert a JSON value from the wire into a value for a local column.
/// `json_column` columns store JSON text of the value as it is; other
/// columns store scalars natively and objects or arrays as JSON text.
pub fn json_to_sql(v: &Json, json_column: bool) -> SqlValue {
    if json_column {
        return match v {
            Json::Null => SqlValue::Null,
            other => SqlValue::Text(other.to_string()),
        };
    }
    match v {
        Json::Null => SqlValue::Null,
        Json::Bool(b) => SqlValue::Integer(*b as i64),
        Json::Number(n) => match n.as_i64() {
            Some(i) => SqlValue::Integer(i),
            None => SqlValue::Real(n.as_f64().unwrap_or(0.0)),
        },
        Json::String(s) => SqlValue::Text(s.clone()),
        other => SqlValue::Text(other.to_string()),
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct QueryResult {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<SqlValue>>,
}
