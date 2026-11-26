//! One sync cycle (`docs/04-client-engine.md`, sections 4.5 to 4.8).
//!
//! Every step either commits one local transaction or changes nothing, so
//! the app can be killed anywhere without losing or duplicating work:
//!
//! 1. Session: versions, limits and the scopes this user may sync.
//! 2. Scopes: add new ones, purge removed ones, drop changes that can no
//!    longer be sent.
//! 3. Push: first resend a batch whose result was never applied, then send
//!    new batches until the outbox is empty.
//! 4. Pull: for each scope, fetch and apply pages until caught up.

use crate::types::*;
use crate::{capture_off, Inner};
use std::collections::HashMap;
use sync_core::batch::build_batch;
use sync_core::protocol::*;
use sync_net::ClientError;
use sync_store::rusqlite::{params, Connection};
use sync_store::state::{self, SentBatch};
use sync_store::{delete_row, delete_scope_rows, write_record, TableSpec};

const RESYNCING: &str = "resyncing";
const ACTIVE: &str = "active";

impl Inner {
    pub(crate) fn run_cycle(&self, events: &mut Vec<SyncEvent>) -> Result<SyncReport> {
        let mut report = SyncReport {
            cycles: 1,
            ..SyncReport::default()
        };
        let mut session = self.session()?;
        self.reconcile_scopes(&session, events)?;
        self.drop_unsendable(&session, events, &mut report)?;
        if self.push_all(&session, events, &mut report)? == PushOutcome::AccessChanged {
            // A push was refused for lack of access: refresh what we may
            // do, then try once more.
            session = self.session()?;
            self.reconcile_scopes(&session, events)?;
            self.drop_unsendable(&session, events, &mut report)?;
            if self.push_all(&session, events, &mut report)? == PushOutcome::AccessChanged {
                return Err(EngineError::Protocol(
                    "push refused for a scope the session grants".into(),
                ));
            }
        }
        self.pull_all(&session, events, &mut report)?;
        Ok(report)
    }

    fn session(&self) -> Result<Session> {
        let s = self.client.session().map_err(EngineError::from_client)?;
        if s.protocol != PROTOCOL_VERSION {
            return Err(EngineError::Protocol(format!(
                "server speaks protocol {}",
                s.protocol
            )));
        }
        if self.manifest.schema_version < s.min_schema_version {
            return Err(EngineError::UpgradeRequired {
                min_schema_version: s.min_schema_version,
            });
        }
        Ok(s)
    }

    // ------------------------------------------------------------------ scopes

    fn reconcile_scopes(&self, session: &Session, events: &mut Vec<SyncEvent>) -> Result<()> {
        let manifest = self.manifest.clone();
        let mut store = self.store();
        let default_scope = store.default_scope()?;
        capture_off(&mut store, |tx| {
            let local = state::scopes(tx)?;
            for l in &local {
                if !session.scopes.iter().any(|g| g.id == l.scope) {
                    delete_scope_rows(tx, &manifest, &l.scope, default_scope.as_deref())?;
                    let dropped = state::purge_scope(tx, &l.scope)?;
                    log::info!(
                        "scope {} removed; dropped {dropped} unsent changes",
                        l.scope
                    );
                    events.push(SyncEvent::ScopeRemoved {
                        scope: l.scope.clone(),
                        dropped_changes: dropped,
                    });
                }
            }
            for g in &session.scopes {
                match local.iter().find(|l| l.scope == g.id) {
                    None => {
                        state::upsert_scope(tx, &g.id, g.access)?;
                        events.push(SyncEvent::ScopeAdded {
                            scope: g.id.clone(),
                        });
                    }
                    Some(l) if l.access != g.access => state::upsert_scope(tx, &g.id, g.access)?,
                    Some(_) => {}
                }
            }
            Ok(())
        })?;
        Ok(())
    }

    /// Changes to scopes this user cannot write are dropped. In a read-only
    /// scope the scope is downloaded again, which restores the rows the app
    /// changed. In a scope the user has no access to, the rows are removed.
    fn drop_unsendable(
        &self,
        session: &Session,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let writable: Vec<String> = session
            .scopes
            .iter()
            .filter(|g| g.access.can_write())
            .map(|g| g.id.clone())
            .collect();
        let readable: Vec<String> = session.scopes.iter().map(|g| g.id.clone()).collect();
        let manifest = self.manifest.clone();
        let mut store = self.store();
        capture_off(&mut store, |tx| {
            if let Some(b) = state::sent_batch(tx)? {
                if !writable.contains(&b.scope) {
                    state::finish_batch(tx, b.batch_id)?;
                }
            }
            let entries = state::pending_outside(tx, &writable)?;
            let mut resync = Vec::new();
            for e in &entries {
                state::delete_entries(tx, &[e.seq])?;
                report.rejected += 1;
                let code = if readable.contains(&e.scope) {
                    if !resync.contains(&e.scope) {
                        resync.push(e.scope.clone());
                    }
                    // Forget the row's version so the download rewrites it
                    // even though the server copy has not changed.
                    state::delete_row_state(tx, &e.table, &e.row_id)?;
                    "read_only_scope"
                } else {
                    if let Some(spec) = manifest.table(&e.table) {
                        delete_row(tx, spec, &e.row_id)?;
                    }
                    state::delete_row_state(tx, &e.table, &e.row_id)?;
                    "scope_forbidden"
                };
                events.push(SyncEvent::ChangeRejected {
                    table: e.table.clone(),
                    id: e.row_id.clone(),
                    code: code.into(),
                    message: format!("no write access to scope {}", e.scope),
                });
            }
            for scope in resync {
                start_resync(tx, &scope, events)?;
            }
            Ok(())
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------ push

    fn push_all(
        &self,
        session: &Session,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<PushOutcome> {
        let mut max_changes = session.limits.max_push_changes.max(1);
        let max_bytes = session.limits.max_push_bytes.max(1024);

        let unfinished = state::sent_batch(self.store().conn())?;
        if let Some(b) = unfinished {
            log::info!("resending batch {} of {} changes", b.batch_id, b.map.len());
            if self.send_batch(b, &mut max_changes, events, report)? == PushOutcome::AccessChanged {
                return Ok(PushOutcome::AccessChanged);
            }
        }

        // Bounded so a host writing without pause cannot keep one cycle
        // running forever; what is left goes in the next cycle.
        let initial = state::count_unsent(self.store().conn())? as usize;
        let max_rounds = initial / max_changes + 20;
        for _ in 0..max_rounds {
            let planned = {
                let mut store = self.store();
                capture_off(&mut store, |tx| {
                    let pending = state::pending(tx, max_changes * 2 + 50)?;
                    let Some(batch) = build_batch(&pending, max_changes, max_bytes) else {
                        return Ok(Plan::Done);
                    };
                    state::delete_entries(tx, &batch.dropped)?;
                    if batch.changes.is_empty() {
                        return Ok(Plan::Again);
                    }
                    let body = serde_json::to_string(&PushRequest {
                        changes: batch.changes.iter().map(|p| p.change.clone()).collect(),
                    })?;
                    let map: Vec<(String, Vec<i64>)> = batch
                        .changes
                        .iter()
                        .map(|p| (p.change.change_id.clone(), p.entry_seqs.clone()))
                        .collect();
                    let batch_id = state::record_sent(tx, &batch.scope, &body, &map)?;
                    Ok(Plan::Send(SentBatch {
                        batch_id,
                        scope: batch.scope,
                        body,
                        map,
                    }))
                })?
            };
            match planned {
                Plan::Done => return Ok(PushOutcome::Done),
                Plan::Again => continue,
                Plan::Send(b) => {
                    self.fault(FaultPoint::BatchRecorded)?;
                    if self.send_batch(b, &mut max_changes, events, report)?
                        == PushOutcome::AccessChanged
                    {
                        return Ok(PushOutcome::AccessChanged);
                    }
                }
            }
        }
        Ok(PushOutcome::Done)
    }

    fn send_batch(
        &self,
        b: SentBatch,
        max_changes: &mut usize,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<PushOutcome> {
        match self.client.push(&b.scope, &b.body) {
            Ok(resp) => {
                self.fault(FaultPoint::PushAnswered)?;
                self.apply_push(&b, resp, events, report)?;
                self.fault(FaultPoint::PushApplied)?;
                Ok(PushOutcome::Done)
            }
            // Nothing was applied, so the batch may be split.
            Err(ClientError::TooLarge) => {
                let mut store = self.store();
                capture_off(&mut store, |tx| {
                    if b.map.len() > 1 {
                        *max_changes = (b.map.len() / 2).max(1);
                    } else {
                        let req: PushRequest = serde_json::from_str(&b.body)?;
                        if let Some((c, (_, seqs))) = req.changes.first().zip(b.map.first()) {
                            state::delete_entries(tx, seqs)?;
                            state::delete_pending_for_row(tx, &c.table, &c.id)?;
                            state::delete_row_state(tx, &c.table, &c.id)?;
                            report.rejected += 1;
                            events.push(SyncEvent::ChangeRejected {
                                table: c.table.clone(),
                                id: c.id.clone(),
                                code: "too_large".into(),
                                message: "the change is larger than the server accepts".into(),
                            });
                            // There is no server record to restore the row
                            // from, so download the scope again.
                            start_resync(tx, &b.scope, events)?;
                        }
                    }
                    state::finish_batch(tx, b.batch_id)?;
                    Ok(())
                })?;
                Ok(PushOutcome::Done)
            }
            // Nothing was applied; the entries go back to pending and the
            // session is refreshed.
            Err(ClientError::ScopeForbidden) => {
                let mut store = self.store();
                capture_off(&mut store, |tx| Ok(state::finish_batch(tx, b.batch_id)?))?;
                Ok(PushOutcome::AccessChanged)
            }
            // The batch stays recorded as sent and is resent first next time.
            Err(e) => Err(EngineError::from_client(e)),
        }
    }

    fn apply_push(
        &self,
        b: &SentBatch,
        resp: PushResponse,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let req: PushRequest = serde_json::from_str(&b.body)?;
        if resp.results.len() != req.changes.len()
            || resp
                .results
                .iter()
                .zip(&req.changes)
                .any(|(r, c)| r.change_id != c.change_id)
        {
            return Err(EngineError::Protocol(
                "push results do not match the batch".into(),
            ));
        }
        let manifest = self.manifest.clone();
        let mut store = self.store();
        capture_off(&mut store, |tx| {
            let gen = state::scope_state(tx, &b.scope)?
                .map(|s| s.resync_gen)
                .unwrap_or(0);
            for ((change, result), (_, seqs)) in req.changes.iter().zip(&resp.results).zip(&b.map) {
                state::delete_entries(tx, seqs)?;
                match result.status {
                    ResultStatus::Applied => {
                        report.pushed += 1;
                        if let Some(v) = result.applied_version() {
                            state::set_known(tx, &change.table, &change.id, &b.scope, v)?;
                            state::rebase_row(tx, &change.table, &change.id, v)?;
                        }
                    }
                    ResultStatus::Conflict | ResultStatus::Rejected => {
                        // Later local edits of this row were made on top of
                        // a change the server refused; they go too.
                        state::delete_pending_for_row(tx, &change.table, &change.id)?;
                        if let Some(spec) = manifest.table(&change.table) {
                            overwrite_local(
                                tx,
                                spec,
                                &b.scope,
                                &change.id,
                                result.current_record(),
                                gen,
                            )?;
                        }
                        if result.status == ResultStatus::Conflict {
                            report.conflicts += 1;
                            events.push(SyncEvent::ChangeConflicted {
                                table: change.table.clone(),
                                id: change.id.clone(),
                                reason: result.reason.map(|r| r.as_str()).unwrap_or("stale").into(),
                            });
                        } else {
                            report.rejected += 1;
                            events.push(SyncEvent::ChangeRejected {
                                table: change.table.clone(),
                                id: change.id.clone(),
                                code: result.code.clone().unwrap_or_default(),
                                message: result.message.clone().unwrap_or_default(),
                            });
                        }
                    }
                }
            }
            state::finish_batch(tx, b.batch_id)?;
            Ok(())
        })?;
        Ok(())
    }

    // ------------------------------------------------------------------ pull

    fn pull_all(
        &self,
        session: &Session,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<()> {
        for grant in &session.scopes {
            self.pull_scope(&grant.id, events, report)?;
        }
        Ok(())
    }

    fn pull_scope(
        &self,
        scope: &str,
        events: &mut Vec<SyncEvent>,
        report: &mut SyncReport,
    ) -> Result<()> {
        let mut expired_once = false;
        let mut last_cursor: Option<String> = None;
        loop {
            let Some(mut st) = state::scope_state(self.store().conn(), scope)? else {
                return Ok(());
            };
            // A download from the beginning may leave out tombstones, yet the
            // device can already hold rows of the scope (its own uploads, or
            // an earlier interrupted download). So every download from the
            // beginning ends like a resync: rows the server did not return
            // are removed.
            if st.cursor.is_none() && st.state != RESYNCING {
                let mut store = self.store();
                st = capture_off(&mut store, |tx| {
                    mark_resync(tx, scope)?;
                    Ok(state::scope_state(tx, scope)?)
                })?
                .expect("scope exists");
            }
            let resp = match self
                .client
                .pull(scope, st.cursor.as_deref(), self.pull_limit)
            {
                Ok(r) => r,
                Err(ClientError::CursorExpired) if !expired_once => {
                    expired_once = true;
                    let mut store = self.store();
                    capture_off(&mut store, |tx| start_resync(tx, scope, events))?;
                    continue;
                }
                // Access was withdrawn during the cycle; the next session
                // purges the scope.
                Err(ClientError::ScopeForbidden) => return Ok(()),
                Err(e) => return Err(EngineError::from_client(e)),
            };
            if resp.has_more && last_cursor.as_deref() == Some(resp.cursor.as_str()) {
                return Err(EngineError::Protocol("pull cursor did not advance".into()));
            }
            last_cursor = Some(resp.cursor.clone());
            self.fault(FaultPoint::PageReceived)?;

            let manifest = self.manifest.clone();
            let mut store = self.store();
            capture_off(&mut store, |tx| {
                for r in &resp.records {
                    if let Some(spec) = manifest.table(&r.table) {
                        if apply_pulled(tx, spec, scope, st.resync_gen, r, events, report)? {
                            report.pulled += 1;
                        }
                    }
                }
                state::set_cursor(tx, scope, Some(&resp.cursor))?;
                if !resp.has_more && st.state == RESYNCING {
                    finish_resync(
                        tx,
                        &manifest,
                        scope,
                        st.resync_gen,
                        store_default_scope(tx)?.as_deref(),
                    )?;
                    let announced = events
                        .iter()
                        .any(|e| matches!(e, SyncEvent::ResyncStarted { scope: s } if s == scope));
                    if announced {
                        events.push(SyncEvent::ResyncFinished {
                            scope: scope.to_string(),
                        });
                    }
                }
                Ok(())
            })?;
            drop(store);
            self.fault(FaultPoint::PageApplied)?;
            if !resp.has_more {
                return Ok(());
            }
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
enum PushOutcome {
    Done,
    AccessChanged,
}

enum Plan {
    Done,
    Again,
    Send(SentBatch),
}

fn store_default_scope(conn: &Connection) -> Result<Option<String>> {
    Ok(state::meta_get(conn, "default_scope")?)
}

/// Replace the local row with the server's state of it.
fn overwrite_local(
    conn: &Connection,
    spec: &TableSpec,
    scope: &str,
    id: &str,
    record: Option<&Record>,
    gen: i64,
) -> Result<()> {
    match record {
        Some(r) if !r.deleted => {
            write_record(
                conn,
                spec,
                scope,
                id,
                r.data.as_ref().unwrap_or(&Fields::new()),
            )?;
            state::set_pulled(conn, &spec.name, id, scope, r.version, gen)?;
        }
        Some(r) => {
            delete_row(conn, spec, id)?;
            state::set_pulled(conn, &spec.name, id, scope, r.version, gen)?;
        }
        None => {
            delete_row(conn, spec, id)?;
            state::delete_row_state(conn, &spec.name, id)?;
        }
    }
    Ok(())
}

/// Apply one pulled record (section 4.7). Returns whether the local row was
/// written.
fn apply_pulled(
    conn: &Connection,
    spec: &TableSpec,
    scope: &str,
    gen: i64,
    r: &Record,
    events: &mut Vec<SyncEvent>,
    report: &mut SyncReport,
) -> Result<bool> {
    let tbl = spec.name.as_str();
    if let Some(rs) = state::row_state(conn, tbl, &r.id)? {
        if r.version <= rs.pulled_version {
            // Already reflected; during a resync it still counts as seen.
            if rs.mark != gen {
                conn.execute(
                    "UPDATE _sync_rows SET mark = ?3 WHERE tbl = ?1 AND row_id = ?2",
                    params![tbl, r.id, gen],
                )?;
            }
            return Ok(false);
        }
    }
    let pending = state::entries_for_row(conn, tbl, &r.id)?;

    if r.deleted {
        delete_row(conn, spec, &r.id)?;
        if state::delete_pending_for_row(conn, tbl, &r.id)? > 0 {
            report.conflicts += 1;
            events.push(SyncEvent::ChangeConflicted {
                table: tbl.to_string(),
                id: r.id.clone(),
                reason: ConflictReason::Deleted.as_str().into(),
            });
        }
        state::set_pulled(conn, tbl, &r.id, scope, r.version, gen)?;
        return Ok(true);
    }

    let data = r.data.clone().unwrap_or_default();
    let base = pending.first().and_then(|e| e.base_version).unwrap_or(0);
    if pending.is_empty() {
        write_record(conn, spec, scope, &r.id, &data)?;
    } else if spec.conflict == Policy::ServerWins && r.version > base {
        // Someone changed the record after our pending edits were made; the
        // server will refuse them, so the server's state wins now.
        state::delete_pending_for_row(conn, tbl, &r.id)?;
        write_record(conn, spec, scope, &r.id, &data)?;
        report.conflicts += 1;
        events.push(SyncEvent::ChangeConflicted {
            table: tbl.to_string(),
            id: r.id.clone(),
            reason: ConflictReason::Stale.as_str().into(),
        });
    } else {
        // Take the server's record, then put the unsent local edits back on
        // top. They stay in the outbox and win on the server too.
        write_record(conn, spec, scope, &r.id, &data)?;
        for e in &pending {
            match e.op {
                Op::Upsert => write_record(conn, spec, scope, &r.id, &e.fields)?,
                Op::Delete => delete_row(conn, spec, &r.id)?,
            }
        }
    }
    state::set_pulled(conn, tbl, &r.id, scope, r.version, gen)?;
    Ok(true)
}

/// Begin downloading a scope again from the beginning (expired cursor, or a
/// read-only scope the app wrote to).
fn start_resync(conn: &Connection, scope: &str, events: &mut Vec<SyncEvent>) -> Result<()> {
    if mark_resync(conn, scope)? {
        log::info!("resyncing scope {scope}");
        events.push(SyncEvent::ResyncStarted {
            scope: scope.to_string(),
        });
    }
    Ok(())
}

/// Put a scope into the resyncing state with a new generation and no
/// cursor. Returns false if the scope does not exist.
fn mark_resync(conn: &Connection, scope: &str) -> Result<bool> {
    let Some(st) = state::scope_state(conn, scope)? else {
        return Ok(false);
    };
    state::set_scope_state(conn, scope, RESYNCING, st.resync_gen + 1)?;
    state::set_cursor(conn, scope, None)?;
    Ok(true)
}

/// After a full download: remove local rows of the scope the server did not
/// return, unless they have unsent changes.
fn finish_resync(
    conn: &Connection,
    manifest: &sync_store::Manifest,
    scope: &str,
    gen: i64,
    default_scope: Option<&str>,
) -> Result<()> {
    let mut removed: HashMap<String, usize> = HashMap::new();
    for t in &manifest.tables {
        let filter = match t.scope.column() {
            Some(c) => format!("\"{c}\" = ?1"),
            None if default_scope == Some(scope) => "?1 = ?1".to_string(),
            None => continue,
        };
        let sql = format!(
            "SELECT id FROM \"{name}\" WHERE {filter}
               AND id NOT IN (SELECT row_id FROM _sync_rows WHERE tbl = ?2 AND mark = ?3)
               AND id NOT IN (SELECT row_id FROM _sync_outbox WHERE tbl = ?2)",
            name = t.name
        );
        let ids: Vec<String> = conn
            .prepare(&sql)?
            .query_map(params![scope, t.name, gen], |r| r.get(0))?
            .collect::<std::result::Result<_, _>>()?;
        for id in &ids {
            delete_row(conn, t, id)?;
            state::delete_row_state(conn, &t.name, id)?;
        }
        removed.insert(t.name.clone(), ids.len());
    }
    conn.execute(
        "DELETE FROM _sync_rows WHERE scope = ?1 AND mark != ?2
           AND NOT EXISTS (SELECT 1 FROM _sync_outbox o WHERE o.tbl = _sync_rows.tbl AND o.row_id = _sync_rows.row_id)",
        params![scope, gen],
    )?;
    state::set_scope_state(conn, scope, ACTIVE, gen)?;
    log::info!("resync of {scope} finished; removed {removed:?}");
    Ok(())
}
