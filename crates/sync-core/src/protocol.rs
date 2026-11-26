//! Wire format of sync protocol v1. See `docs/02-protocol.md`.
//!
//! Unknown fields are ignored on deserialisation (protocol section 1), which
//! is serde's default behaviour.

use serde::{Deserialize, Serialize};

pub type Fields = serde_json::Map<String, serde_json::Value>;

pub const PROTOCOL_VERSION: u32 = 1;

pub const HEADER_SCHEMA: &str = "X-Sync-Schema";
pub const HEADER_DEVICE: &str = "X-Sync-Device";

// ---------------------------------------------------------------- session

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub max_push_changes: usize,
    pub max_push_bytes: usize,
    pub max_pull_limit: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_push_changes: 500,
            max_push_bytes: 1024 * 1024,
            max_pull_limit: 1000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Access {
    Read,
    ReadWrite,
}

impl Access {
    pub fn can_write(self) -> bool {
        self == Access::ReadWrite
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::ReadWrite => "read_write",
        }
    }

    pub fn parse(s: &str) -> Option<Access> {
        match s {
            "read" => Some(Access::Read),
            "read_write" => Some(Access::ReadWrite),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScopeGrant {
    pub id: String,
    pub access: Access,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub protocol: u32,
    pub schema_version: i64,
    pub min_schema_version: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server_time: Option<String>,
    pub limits: Limits,
    pub scopes: Vec<ScopeGrant>,
}

// ---------------------------------------------------------------- policy

/// Conflict policy of a table (protocol section 6). Not sent on the wire;
/// configured identically on the server and in the client manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Policy {
    #[default]
    FieldMerge,
    ServerWins,
}

// ---------------------------------------------------------------- records

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Record {
    pub table: String,
    pub id: String,
    pub version: i64,
    pub deleted: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Fields>,
}

// ---------------------------------------------------------------- push

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Op {
    Upsert,
    Delete,
}

impl Op {
    pub fn as_str(self) -> &'static str {
        match self {
            Op::Upsert => "upsert",
            Op::Delete => "delete",
        }
    }

    pub fn parse(s: &str) -> Option<Op> {
        match s {
            "upsert" => Some(Op::Upsert),
            "delete" => Some(Op::Delete),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Change {
    pub change_id: String,
    pub table: String,
    pub id: String,
    pub op: Op,
    #[serde(default)]
    pub base_version: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fields: Option<Fields>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushRequest {
    pub changes: Vec<Change>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultStatus {
    Applied,
    Conflict,
    Rejected,
}

impl ResultStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultStatus::Applied => "applied",
            ResultStatus::Conflict => "conflict",
            ResultStatus::Rejected => "rejected",
        }
    }

    pub fn parse(s: &str) -> Option<ResultStatus> {
        match s {
            "applied" => Some(ResultStatus::Applied),
            "conflict" => Some(ResultStatus::Conflict),
            "rejected" => Some(ResultStatus::Rejected),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    Stale,
    Deleted,
}

impl ConflictReason {
    pub fn as_str(self) -> &'static str {
        match self {
            ConflictReason::Stale => "stale",
            ConflictReason::Deleted => "deleted",
        }
    }

    pub fn parse(s: &str) -> Option<ConflictReason> {
        match s {
            "stale" => Some(ConflictReason::Stale),
            "deleted" => Some(ConflictReason::Deleted),
            _ => None,
        }
    }
}

/// Standard rejection codes (protocol section 4.3).
pub mod reject_code {
    pub const VALIDATION_FAILED: &str = "validation_failed";
    pub const FORBIDDEN: &str = "forbidden";
    pub const READ_ONLY: &str = "read_only";
    pub const UNKNOWN_TABLE: &str = "unknown_table";
    pub const UNKNOWN_FIELD: &str = "unknown_field";
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ChangeResult {
    pub change_id: String,
    pub status: ResultStatus,
    /// Present for `applied`. `None` inside `applied` means the server never
    /// knew the record (a delete of an unknown ID).
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub version: Option<Option<i64>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ConflictReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// Present for `conflict` and `rejected`. `Some(None)` is an explicit
    /// `null`: the server has no such record.
    #[serde(
        default,
        skip_serializing_if = "Option::is_none",
        with = "double_option"
    )]
    pub record: Option<Option<Record>>,
}

impl ChangeResult {
    pub fn applied(change_id: impl Into<String>, version: Option<i64>) -> Self {
        ChangeResult {
            change_id: change_id.into(),
            status: ResultStatus::Applied,
            version: Some(version),
            reason: None,
            code: None,
            message: None,
            record: None,
        }
    }

    pub fn conflict(
        change_id: impl Into<String>,
        reason: ConflictReason,
        record: Option<Record>,
    ) -> Self {
        ChangeResult {
            change_id: change_id.into(),
            status: ResultStatus::Conflict,
            version: None,
            reason: Some(reason),
            code: None,
            message: None,
            record: Some(record),
        }
    }

    pub fn rejected(
        change_id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
        record: Option<Record>,
    ) -> Self {
        ChangeResult {
            change_id: change_id.into(),
            status: ResultStatus::Rejected,
            version: None,
            reason: None,
            code: Some(code.into()),
            message: Some(message.into()),
            record: Some(record),
        }
    }

    /// The applied version, flattened.
    pub fn applied_version(&self) -> Option<i64> {
        self.version.flatten()
    }

    /// The attached record, flattened.
    pub fn current_record(&self) -> Option<&Record> {
        self.record.as_ref().and_then(|r| r.as_ref())
    }
}

/// Serialise `Option<Option<T>>` so that `Some(None)` becomes `null` and
/// `None` is skipped; on input, a present `null` becomes `Some(None)`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<T: Serialize, S: Serializer>(
        value: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, T: Deserialize<'de>, D: Deserializer<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PushResponse {
    pub results: Vec<ChangeResult>,
}

// ---------------------------------------------------------------- pull

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PullResponse {
    pub records: Vec<Record>,
    pub cursor: String,
    pub has_more: bool,
}

// ---------------------------------------------------------------- errors

/// Request-level error codes (protocol section 7).
pub mod error_code {
    pub const BAD_REQUEST: &str = "bad_request";
    pub const UNAUTHORIZED: &str = "unauthorized";
    pub const SCOPE_FORBIDDEN: &str = "scope_forbidden";
    pub const SCOPE_NOT_FOUND: &str = "scope_not_found";
    pub const SCHEMA_AHEAD: &str = "schema_ahead";
    pub const CURSOR_EXPIRED: &str = "cursor_expired";
    pub const TOO_LARGE: &str = "too_large";
    pub const UPGRADE_REQUIRED: &str = "upgrade_required";
    pub const RATE_LIMITED: &str = "rate_limited";
    pub const INTERNAL: &str = "internal";
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorDetail {
    pub code: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    /// With `upgrade_required`: the oldest schema version still served.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub min_schema_version: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorBody {
    pub error: ErrorDetail,
}

impl ErrorBody {
    pub fn new(code: &str, message: impl Into<String>) -> Self {
        ErrorBody {
            error: ErrorDetail {
                code: code.to_string(),
                message: Some(message.into()),
                min_schema_version: None,
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn change_result_shapes() {
        let applied = ChangeResult::applied("c1", Some(5));
        assert_eq!(
            serde_json::to_value(&applied).unwrap(),
            json!({"change_id": "c1", "status": "applied", "version": 5})
        );
        let applied_null = ChangeResult::applied("c2", None);
        assert_eq!(
            serde_json::to_value(&applied_null).unwrap(),
            json!({"change_id": "c2", "status": "applied", "version": null})
        );
        let rejected = ChangeResult::rejected("c3", "read_only", "no", None);
        assert_eq!(
            serde_json::to_value(&rejected).unwrap(),
            json!({"change_id": "c3", "status": "rejected", "code": "read_only", "message": "no", "record": null})
        );
        let back: ChangeResult = serde_json::from_value(json!({
            "change_id": "c3", "status": "rejected", "code": "x", "record": null, "unknown": 1
        }))
        .unwrap();
        assert_eq!(back.record, Some(None));
        let back: ChangeResult = serde_json::from_value(
            json!({"change_id": "c2", "status": "applied", "version": null}),
        )
        .unwrap();
        assert_eq!(back.version, Some(None));
    }

    #[test]
    fn record_tombstone_has_no_data() {
        let r = Record {
            table: "t".into(),
            id: "1".into(),
            version: 3,
            deleted: true,
            data: None,
        };
        assert_eq!(
            serde_json::to_value(&r).unwrap(),
            json!({"table": "t", "id": "1", "version": 3, "deleted": true})
        );
    }
}
