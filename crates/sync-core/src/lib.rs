//! Protocol types and pure sync rules for offline-sync.
//!
//! This crate does no I/O. It holds the wire format of protocol v1
//! (`docs/02-protocol.md`), plain HTTP request and response values shared by
//! the transports and the reference server, and the outbox batching rules
//! (`docs/04-client-engine.md`, section 4.6).

pub mod batch;
pub mod http;
pub mod protocol;

/// Generate a new record or change ID: a UUID version 7, which sorts by
/// creation time.
pub fn new_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Name pattern for tables: `[a-z][a-z0-9_]{0,62}`.
pub fn is_valid_table_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    !bytes.is_empty()
        && bytes.len() <= 63
        && bytes[0].is_ascii_lowercase()
        && bytes
            .iter()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || *b == b'_')
}

/// Scope ID pattern: `[A-Za-z0-9_.:-]{1,128}`.
pub fn is_valid_scope_id(scope: &str) -> bool {
    !scope.is_empty()
        && scope.len() <= 128
        && scope
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'.' | b':' | b'-'))
}

/// Field names follow the same pattern as table names.
pub fn is_valid_field_name(name: &str) -> bool {
    is_valid_table_name(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names() {
        assert!(is_valid_table_name("visit_photos"));
        assert!(!is_valid_table_name("Visits"));
        assert!(!is_valid_table_name("1abc"));
        assert!(!is_valid_table_name(""));
        assert!(is_valid_scope_id("user:1042"));
        assert!(is_valid_scope_id("team.a-b_c"));
        assert!(!is_valid_scope_id("a b"));
        assert!(!is_valid_scope_id(""));
    }

    #[test]
    fn ids_are_unique_and_sortable() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
