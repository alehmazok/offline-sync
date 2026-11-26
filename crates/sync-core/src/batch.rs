//! Building a push batch from pending outbox entries
//! (`docs/04-client-engine.md`, section 4.6).
//!
//! Pending entries of the same row are merged:
//!
//! | Pending entries for one row        | Sent as                         |
//! |------------------------------------|---------------------------------|
//! | upsert, upsert                     | one upsert, later values win    |
//! | upsert(s), then delete             | one delete                      |
//! | insert, then delete, never synced  | nothing; all entries dropped    |
//!
//! A merged upsert takes the position of its first entry, a delete the
//! position of its last, so a parent created before a child is still sent
//! first. A delete followed by a new upsert of the same row is sent as two
//! changes.

use crate::protocol::{Change, Fields, Op};

#[derive(Debug, Clone, PartialEq)]
pub struct OutboxEntry {
    /// Local order number. Entries are processed in ascending order.
    pub seq: i64,
    pub change_id: String,
    pub scope: String,
    pub table: String,
    pub row_id: String,
    pub op: Op,
    /// For upserts, the fields this entry sets.
    pub fields: Fields,
    pub base_version: Option<i64>,
}

/// One change of a batch and the outbox entries it stands for.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedChange {
    pub change: Change,
    pub entry_seqs: Vec<i64>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub scope: String,
    pub changes: Vec<PlannedChange>,
    /// Entries that cancel out and are to be removed without sending.
    pub dropped: Vec<i64>,
}

impl Batch {
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty() && self.dropped.is_empty()
    }
}

struct Out {
    pos: i64,
    op: Op,
    fields: Fields,
    base: Option<i64>,
    change_id: String,
    table: String,
    row_id: String,
    seqs: Vec<i64>,
}

/// Plan the next batch. `pending` must be in ascending `seq` order. Only
/// entries of the first entry's scope are considered. `max_changes` and
/// `max_bytes` come from the session limits; at least one change is always
/// returned when there is something to send.
pub fn build_batch(pending: &[OutboxEntry], max_changes: usize, max_bytes: usize) -> Option<Batch> {
    let scope = pending.first()?.scope.clone();
    let entries: Vec<&OutboxEntry> = pending.iter().filter(|e| e.scope == scope).collect();

    // Per row, the list of outgoing operations, in local order.
    let mut rows: Vec<((String, String), Vec<Out>)> = Vec::new();
    let mut dropped = Vec::new();

    for e in entries {
        let key = (e.table.clone(), e.row_id.clone());
        let idx = match rows.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                rows.push((key, Vec::new()));
                rows.len() - 1
            }
        };
        let outs = &mut rows[idx].1;
        match e.op {
            Op::Upsert => match outs.last_mut() {
                Some(last) if last.op == Op::Upsert => {
                    for (k, v) in &e.fields {
                        last.fields.insert(k.clone(), v.clone());
                    }
                    last.seqs.push(e.seq);
                }
                _ => outs.push(Out {
                    pos: e.seq,
                    op: Op::Upsert,
                    fields: e.fields.clone(),
                    base: e.base_version,
                    change_id: e.change_id.clone(),
                    table: e.table.clone(),
                    row_id: e.row_id.clone(),
                    seqs: vec![e.seq],
                }),
            },
            Op::Delete => {
                // A delete absorbs the upserts directly before it.
                let mut absorbed = Vec::new();
                let mut created_locally = false;
                if let Some(last) = outs.last() {
                    if last.op == Op::Upsert {
                        let last = outs.pop().unwrap();
                        created_locally = last.base.is_none();
                        absorbed = last.seqs;
                    }
                }
                if created_locally && outs.is_empty() {
                    // Created and deleted before the server ever saw it.
                    dropped.extend(absorbed);
                    dropped.push(e.seq);
                    continue;
                }
                let mut seqs = absorbed;
                seqs.push(e.seq);
                outs.push(Out {
                    pos: e.seq,
                    op: Op::Delete,
                    fields: Fields::new(),
                    base: e.base_version,
                    change_id: e.change_id.clone(),
                    table: e.table.clone(),
                    row_id: e.row_id.clone(),
                    seqs,
                });
            }
        }
    }

    let mut outs: Vec<Out> = rows.into_iter().flat_map(|(_, o)| o).collect();
    outs.sort_by_key(|o| o.pos);

    let mut changes = Vec::new();
    let mut bytes = 2usize;
    for o in outs {
        let change = Change {
            change_id: o.change_id,
            table: o.table,
            id: o.row_id,
            op: o.op,
            base_version: o.base,
            fields: match o.op {
                Op::Upsert => Some(o.fields),
                Op::Delete => None,
            },
        };
        let size = serde_json::to_vec(&change).map(|v| v.len()).unwrap_or(0) + 1;
        if !changes.is_empty() && (changes.len() >= max_changes || bytes + size > max_bytes) {
            break;
        }
        bytes += size;
        changes.push(PlannedChange {
            change,
            entry_seqs: o.seqs,
        });
    }

    Some(Batch {
        scope,
        changes,
        dropped,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fields(v: serde_json::Value) -> Fields {
        v.as_object().unwrap().clone()
    }

    fn up(seq: i64, row: &str, f: serde_json::Value, base: Option<i64>) -> OutboxEntry {
        OutboxEntry {
            seq,
            change_id: format!("c{seq}"),
            scope: "s".into(),
            table: "t".into(),
            row_id: row.into(),
            op: Op::Upsert,
            fields: fields(f),
            base_version: base,
        }
    }

    fn del(seq: i64, row: &str, base: Option<i64>) -> OutboxEntry {
        OutboxEntry {
            op: Op::Delete,
            fields: Fields::new(),
            ..up(seq, row, json!({}), base)
        }
    }

    #[test]
    fn merges_upserts_later_values_win() {
        let b = build_batch(
            &[
                up(1, "a", json!({"x": 1, "y": 1}), Some(3)),
                up(2, "a", json!({"x": 2}), Some(3)),
            ],
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(b.changes.len(), 1);
        let c = &b.changes[0];
        assert_eq!(c.change.change_id, "c1");
        assert_eq!(
            c.change.fields.as_ref().unwrap(),
            &fields(json!({"x": 2, "y": 1}))
        );
        assert_eq!(c.change.base_version, Some(3));
        assert_eq!(c.entry_seqs, vec![1, 2]);
    }

    #[test]
    fn upsert_then_delete_of_synced_row_is_one_delete() {
        let b = build_batch(
            &[up(1, "a", json!({"x": 1}), Some(3)), del(2, "a", Some(3))],
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(b.changes.len(), 1);
        assert_eq!(b.changes[0].change.op, Op::Delete);
        assert_eq!(b.changes[0].change.fields, None);
        assert_eq!(b.changes[0].entry_seqs, vec![1, 2]);
        assert!(b.dropped.is_empty());
    }

    #[test]
    fn local_create_then_delete_is_dropped() {
        let b = build_batch(
            &[
                up(1, "a", json!({"x": 1}), None),
                up(2, "a", json!({"x": 2}), None),
                del(3, "a", None),
            ],
            100,
            1 << 20,
        )
        .unwrap();
        assert!(b.changes.is_empty());
        assert_eq!(b.dropped, vec![1, 2, 3]);
    }

    #[test]
    fn positions_keep_parent_before_child() {
        // Parent p created, child c created, parent edited again.
        let b = build_batch(
            &[
                up(1, "p", json!({"n": 1}), None),
                up(2, "c", json!({"parent": "p"}), None),
                up(3, "p", json!({"n": 2}), None),
            ],
            100,
            1 << 20,
        )
        .unwrap();
        let ids: Vec<_> = b.changes.iter().map(|c| c.change.id.as_str()).collect();
        assert_eq!(ids, vec!["p", "c"]);
    }

    #[test]
    fn delete_takes_last_position() {
        let b = build_batch(
            &[
                up(1, "a", json!({"x": 1}), Some(1)),
                up(2, "b", json!({"x": 1}), Some(1)),
                del(3, "a", Some(1)),
            ],
            100,
            1 << 20,
        )
        .unwrap();
        let ids: Vec<_> = b.changes.iter().map(|c| c.change.id.as_str()).collect();
        assert_eq!(ids, vec!["b", "a"]);
    }

    #[test]
    fn delete_then_recreate_is_two_changes() {
        let b = build_batch(
            &[del(1, "a", Some(4)), up(2, "a", json!({"x": 1}), Some(4))],
            100,
            1 << 20,
        )
        .unwrap();
        let ops: Vec<_> = b.changes.iter().map(|c| c.change.op).collect();
        assert_eq!(ops, vec![Op::Delete, Op::Upsert]);
    }

    #[test]
    fn one_scope_per_batch() {
        let mut other = up(2, "b", json!({"x": 1}), None);
        other.scope = "other".into();
        let b = build_batch(
            &[
                up(1, "a", json!({}), None),
                other,
                up(3, "c", json!({}), None),
            ],
            100,
            1 << 20,
        )
        .unwrap();
        assert_eq!(b.scope, "s");
        assert_eq!(b.changes.len(), 2);
    }

    #[test]
    fn limits_are_respected_but_never_empty() {
        let entries: Vec<_> = (1..=10)
            .map(|i| up(i, &format!("r{i}"), json!({"x": i}), None))
            .collect();
        assert_eq!(build_batch(&entries, 3, 1 << 20).unwrap().changes.len(), 3);
        assert_eq!(build_batch(&entries, 100, 10).unwrap().changes.len(), 1);
    }

    #[test]
    fn empty_input() {
        assert!(build_batch(&[], 10, 10).is_none());
    }
}
