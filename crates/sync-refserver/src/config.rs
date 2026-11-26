use serde::{Deserialize, Serialize};
pub use sync_core::protocol::Policy;
use sync_core::protocol::{Access, Limits, ScopeGrant};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TableConfig {
    pub name: String,
    pub fields: Vec<String>,
    #[serde(default)]
    pub conflict: Policy,
    #[serde(default)]
    pub read_only: bool,
    /// Fields that must be present and not empty (`null` or `""`). A
    /// stand-in for project validation code.
    #[serde(default)]
    pub required: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserConfig {
    pub token: String,
    pub scopes: Vec<ScopeGrant>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerConfig {
    pub schema_version: i64,
    pub min_schema_version: i64,
    #[serde(default)]
    pub limits: Limits,
    pub tables: Vec<TableConfig>,
    pub users: Vec<UserConfig>,
    /// Enables the `/_test/...` endpoints. Never in production.
    #[serde(default)]
    pub test_hooks: bool,
}

impl ServerConfig {
    pub fn table(&self, name: &str) -> Option<&TableConfig> {
        self.tables.iter().find(|t| t.name == name)
    }

    /// The fixture described in `docs/05-conformance-tests.md`, section 5.1.
    pub fn conformance() -> ServerConfig {
        let grant = |id: &str, access| ScopeGrant {
            id: id.to_string(),
            access,
        };
        ServerConfig {
            schema_version: 1,
            min_schema_version: 1,
            limits: Limits::default(),
            tables: vec![
                TableConfig {
                    name: "conf_items".into(),
                    fields: vec!["title".into(), "qty".into(), "note".into()],
                    conflict: Policy::FieldMerge,
                    read_only: false,
                    required: vec!["title".into()],
                },
                TableConfig {
                    name: "conf_locked".into(),
                    fields: vec!["title".into(), "qty".into()],
                    conflict: Policy::ServerWins,
                    read_only: false,
                    required: vec![],
                },
                TableConfig {
                    name: "conf_readonly".into(),
                    fields: vec!["title".into()],
                    conflict: Policy::FieldMerge,
                    read_only: true,
                    required: vec![],
                },
            ],
            users: vec![
                UserConfig {
                    token: "token-a".into(),
                    scopes: vec![
                        grant("team:conf1", Access::ReadWrite),
                        grant("team:conf2", Access::ReadWrite),
                    ],
                },
                UserConfig {
                    token: "token-b".into(),
                    scopes: vec![grant("team:conf1", Access::Read)],
                },
            ],
            test_hooks: true,
        }
    }
}
