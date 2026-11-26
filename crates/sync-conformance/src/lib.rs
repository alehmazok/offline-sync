//! Conformance suite for sync protocol v1 (`docs/05-conformance-tests.md`).
//!
//! The suite talks to a running server over HTTP and knows nothing about
//! its language or database. Every run creates records with fresh IDs, so
//! the test scopes never need to be emptied.

pub mod client;

use client::{hook_post, Api, Body};
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

#[derive(Debug, Clone)]
pub struct Config {
    pub base_url: String,
    pub token_a: String,
    pub token_b: String,
    pub scope_1: String,
    pub scope_2: String,
    pub schema: i64,
    /// Base URL of the server's test hooks; enables the optional tests.
    pub test_url: Option<String>,
    /// How many times the concurrency test C-PULL-06 is repeated.
    pub concurrency_runs: usize,
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: &'static str,
    pub title: &'static str,
    /// `None` when the test was skipped.
    pub passed: Option<bool>,
    pub detail: String,
}

type TestFn = fn(&Ctx) -> Result<(), String>;

struct Test {
    id: &'static str,
    title: &'static str,
    run: TestFn,
    needs_hooks: bool,
}

macro_rules! ensure {
    ($cond:expr, $($fmt:tt)+) => {
        if !$cond {
            return Err(format!($($fmt)+));
        }
    };
}

pub struct Ctx {
    pub cfg: Config,
    pub api: Api,
    cursors: Mutex<HashMap<String, String>>,
}

impl Ctx {
    fn a(&self) -> &str {
        &self.cfg.token_a
    }
    fn b(&self) -> &str {
        &self.cfg.token_b
    }
    fn s1(&self) -> &str {
        &self.cfg.scope_1
    }
    fn s2(&self) -> &str {
        &self.cfg.scope_2
    }

    /// The cursor after everything currently in `scope`.
    fn tail(&self, scope: &str) -> Result<String, String> {
        let cached = self.cursors.lock().unwrap().get(scope).cloned();
        let (_, cursor) = self
            .api
            .pull_all(self.a(), scope, cached.as_deref(), Some(1000))?;
        self.cursors
            .lock()
            .unwrap()
            .insert(scope.to_string(), cursor.clone());
        Ok(cursor)
    }

    fn since(&self, scope: &str, cursor: &str) -> Result<Vec<Value>, String> {
        Ok(self
            .api
            .pull_all(self.a(), scope, Some(cursor), Some(1000))?
            .0)
    }

    /// Create a record with user A and return its ID and version.
    fn create(&self, scope: &str, table: &str, fields: Value) -> Result<(String, i64), String> {
        let id = sync_core::new_id();
        let r = self
            .api
            .push_one(self.a(), scope, upsert(table, &id, fields, None))?;
        ensure!(
            status(&r) == "applied",
            "creating a record: expected applied, got {r}"
        );
        Ok((id, version(&r).ok_or("creating a record: no version")?))
    }

    /// The last state of record `id` in a pull after `cursor`.
    fn latest(&self, scope: &str, cursor: &str, id: &str) -> Result<Value, String> {
        let records = self.since(scope, cursor)?;
        last_of(&records, id)
            .cloned()
            .ok_or_else(|| format!("record {id} not returned by pull"))
    }
}

pub fn upsert(table: &str, id: &str, fields: Value, base: Option<i64>) -> Value {
    json!({
        "change_id": sync_core::new_id(),
        "table": table,
        "id": id,
        "op": "upsert",
        "base_version": base,
        "fields": fields,
    })
}

pub fn delete(table: &str, id: &str, base: Option<i64>) -> Value {
    json!({
        "change_id": sync_core::new_id(),
        "table": table,
        "id": id,
        "op": "delete",
        "base_version": base,
    })
}

fn status(r: &Value) -> &str {
    r["status"].as_str().unwrap_or("")
}

fn version(r: &Value) -> Option<i64> {
    r["version"].as_i64()
}

fn last_of<'a>(records: &'a [Value], id: &str) -> Option<&'a Value> {
    records.iter().rev().find(|r| r["id"] == id)
}

fn tests() -> Vec<Test> {
    macro_rules! t {
        ($id:literal, $title:literal, $f:ident) => {
            Test {
                id: $id,
                title: $title,
                run: $f,
                needs_hooks: false,
            }
        };
        ($id:literal, $title:literal, $f:ident, hooks) => {
            Test {
                id: $id,
                title: $title,
                run: $f,
                needs_hooks: true,
            }
        };
    }
    vec![
        t!(
            "C-SES-01",
            "session of user A lists both scopes read_write",
            ses_01
        ),
        t!("C-SES-02", "session of user B lists S1 read only", ses_02),
        t!("C-SES-03", "request without a token is refused", ses_03),
        t!("C-PUSH-01", "create one record", push_01),
        t!("C-PUSH-02", "update changes only the named field", push_02),
        t!("C-PUSH-03", "same batch twice is applied once", push_03),
        t!(
            "C-PUSH-04",
            "resent change keeps its result and harms nothing",
            push_04
        ),
        t!(
            "C-PUSH-05",
            "changes in a batch are decided independently",
            push_05
        ),
        t!(
            "C-PUSH-06",
            "rejected change carries the current record",
            push_06
        ),
        t!("C-PUSH-07", "a field can be set to null", push_07),
        t!("C-PUSH-08", "read-only table rejects changes", push_08),
        t!(
            "C-PUSH-09",
            "unknown table and unknown field are rejected",
            push_09
        ),
        t!(
            "C-PUSH-10",
            "unknown top-level property is ignored",
            push_10
        ),
        t!("C-PUSH-11", "push to a read-only scope is refused", push_11),
        t!(
            "C-PUSH-12",
            "push to an inaccessible scope is refused",
            push_12
        ),
        t!(
            "C-PUSH-13",
            "record of another scope cannot be written",
            push_13
        ),
        t!(
            "C-PUSH-14",
            "batch over the limit is refused whole",
            push_14
        ),
        t!("C-PUSH-15", "values of every type round-trip", push_15),
        t!(
            "C-PULL-01",
            "pull from the beginning is ordered and has a cursor",
            pull_01
        ),
        t!("C-PULL-02", "pull at the tail is empty", pull_02),
        t!(
            "C-PULL-03",
            "paging delivers the same set as one pull",
            pull_03
        ),
        t!("C-PULL-04", "deletions arrive as tombstones", pull_04),
        t!(
            "C-PULL-05",
            "last appearance of a record has its final state",
            pull_05
        ),
        t!(
            "C-PULL-06",
            "no change is skipped under concurrent writes",
            pull_06
        ),
        t!("C-PULL-07", "a device receives its own changes", pull_07),
        t!(
            "C-PULL-08",
            "pull of an inaccessible scope is refused",
            pull_08
        ),
        t!("C-PULL-09", "pulled records carry every field", pull_09),
        t!("C-PULL-10", "scopes are independent", pull_10),
        t!(
            "C-CONF-01",
            "field_merge: different fields both survive",
            conf_01
        ),
        t!(
            "C-CONF-02",
            "field_merge: same field, later arrival wins",
            conf_02
        ),
        t!(
            "C-CONF-03",
            "server_wins: stale change is a conflict",
            conf_03
        ),
        t!("C-CONF-04", "server_wins: current base is applied", conf_04),
        t!(
            "C-CONF-05",
            "upsert of a deleted record is a conflict",
            conf_05
        ),
        t!("C-CONF-06", "delete twice is applied twice", conf_06),
        t!(
            "C-CONF-07",
            "delete of an unknown record is applied",
            conf_07
        ),
        t!("C-CONF-08", "field_merge: stale delete is applied", conf_08),
        t!(
            "C-CONF-09",
            "server_wins: stale delete is a conflict",
            conf_09
        ),
        t!(
            "C-CONF-10",
            "new record with a base version is created",
            conf_10
        ),
        t!("C-ERR-01", "old schema gets 426 on every endpoint", err_01),
        t!("C-ERR-02", "newer schema gets 409", err_02),
        t!("C-ERR-03", "malformed JSON gets 400", err_03),
        t!("C-ERR-04", "garbage cursor gets 400 or 410", err_04),
        t!("C-OPT-02", "revoked access takes effect", opt_02, hooks),
        // Pruning tombstones invalidates old cursors, so it runs last.
        t!(
            "C-OPT-01",
            "expired cursor and pull from the beginning",
            opt_01,
            hooks
        ),
    ]
}

/// Run the suite. `filter` keeps only tests whose ID starts with it.
pub fn run(cfg: &Config, filter: Option<&str>, mut report: impl FnMut(&Outcome)) -> Vec<Outcome> {
    let ctx = Ctx {
        api: Api::new(&cfg.base_url, cfg.schema),
        cfg: cfg.clone(),
        cursors: Mutex::new(HashMap::new()),
    };
    let mut outcomes = Vec::new();
    for t in tests() {
        if filter.is_some_and(|f| !t.id.starts_with(f)) {
            continue;
        }
        let outcome = if t.needs_hooks && cfg.test_url.is_none() {
            Outcome {
                id: t.id,
                title: t.title,
                passed: None,
                detail: "needs --test-url".into(),
            }
        } else {
            let res = std::panic::catch_unwind(AssertUnwindSafe(|| (t.run)(&ctx)));
            let (passed, detail) = match res {
                Ok(Ok(())) => (true, String::new()),
                Ok(Err(e)) => (false, e),
                Err(p) => {
                    let msg = p
                        .downcast_ref::<String>()
                        .cloned()
                        .or_else(|| p.downcast_ref::<&str>().map(|s| s.to_string()))
                        .unwrap_or_default();
                    (false, format!("panicked: {msg}"))
                }
            };
            Outcome {
                id: t.id,
                title: t.title,
                passed: Some(passed),
                detail,
            }
        };
        report(&outcome);
        outcomes.push(outcome);
    }
    outcomes
}

// ===================================================================== session

fn grants(r: &client::Resp) -> HashMap<String, String> {
    r.body["scopes"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|s| {
                    (
                        s["id"].as_str().unwrap_or("").to_string(),
                        s["access"].as_str().unwrap_or("").to_string(),
                    )
                })
                .collect()
        })
        .unwrap_or_default()
}

fn ses_01(c: &Ctx) -> Result<(), String> {
    let r = c.api.session(Some(c.a()))?;
    ensure!(r.status == 200, "expected 200, got {}", r.status);
    ensure!(r.body["protocol"] == 1, "protocol is not 1: {}", r.body);
    for k in ["schema_version", "min_schema_version"] {
        ensure!(r.body[k].is_i64(), "{k} missing");
    }
    for k in ["max_push_changes", "max_push_bytes", "max_pull_limit"] {
        ensure!(r.body["limits"][k].is_u64(), "limits.{k} missing");
    }
    let g = grants(&r);
    ensure!(
        g.get(c.s1()).map(String::as_str) == Some("read_write"),
        "S1 not read_write: {g:?}"
    );
    ensure!(
        g.get(c.s2()).map(String::as_str) == Some("read_write"),
        "S2 not read_write: {g:?}"
    );
    Ok(())
}

fn ses_02(c: &Ctx) -> Result<(), String> {
    let r = c.api.session(Some(c.b()))?;
    ensure!(r.status == 200, "expected 200, got {}", r.status);
    let g = grants(&r);
    ensure!(
        g.get(c.s1()).map(String::as_str) == Some("read"),
        "S1 not read: {g:?}"
    );
    ensure!(!g.contains_key(c.s2()), "S2 should not be listed: {g:?}");
    Ok(())
}

fn ses_03(c: &Ctx) -> Result<(), String> {
    let r = c.api.session(None)?;
    ensure!(r.status == 401, "expected 401, got {}", r.status);
    ensure!(
        r.error_code() == Some("unauthorized"),
        "expected unauthorized, got {}",
        r.body
    );
    Ok(())
}

// ===================================================================== push

fn push_01(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let (id, _) = c.create(
        c.s1(),
        "conf_items",
        json!({"title": "one", "qty": 1, "note": "n"}),
    )?;
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"] == json!({"title": "one", "qty": 1, "note": "n"}),
        "unexpected data: {rec}"
    );
    ensure!(
        rec["deleted"] == false && rec["table"] == "conf_items",
        "unexpected record: {rec}"
    );
    Ok(())
}

fn push_02(c: &Ctx) -> Result<(), String> {
    let (id, v1) = c.create(
        c.s1(),
        "conf_items",
        json!({"title": "two", "qty": 1, "note": "keep"}),
    )?;
    let cur = c.tail(c.s1())?;
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"qty": 2}), Some(v1)),
    )?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    let v2 = version(&r).ok_or("no version")?;
    ensure!(v2 > v1, "version did not increase: {v1} then {v2}");
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"] == json!({"title": "two", "qty": 2, "note": "keep"}),
        "unexpected data: {rec}"
    );
    ensure!(
        rec["version"] == v2,
        "pulled version {} != {v2}",
        rec["version"]
    );
    Ok(())
}

fn push_03(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let id = sync_core::new_id();
    let batch = vec![
        upsert("conf_items", &id, json!({"title": "dup", "qty": 1}), None),
        upsert("conf_items", &id, json!({"qty": 5}), None),
    ];
    let first = c.api.push(c.a(), c.s1(), batch.clone())?;
    let second = c.api.push(c.a(), c.s1(), batch)?;
    for (a, b) in first.iter().zip(&second) {
        ensure!(
            a["status"] == b["status"] && a["version"] == b["version"],
            "results differ: {a} vs {b}"
        );
    }
    let records = c.since(c.s1(), &cur)?;
    let mine: Vec<_> = records.iter().filter(|r| r["id"] == id).collect();
    ensure!(
        mine.len() == 1,
        "expected the record once, got {}",
        mine.len()
    );
    ensure!(
        mine[0]["version"] == first[1]["version"],
        "record changed by the duplicate batch"
    );
    Ok(())
}

fn push_04(c: &Ctx) -> Result<(), String> {
    let id = sync_core::new_id();
    let create = upsert("conf_items", &id, json!({"title": "d1", "qty": 1}), None);
    let r1 = c.api.push_one(c.a(), c.s1(), create.clone())?;
    let v1 = version(&r1).ok_or("no version")?;
    let other = c.api.device();
    let r2 = other.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"qty": 7}), Some(v1)),
    )?;
    ensure!(
        status(&r2) == "applied",
        "second device's change not applied: {r2}"
    );
    let cur = c.tail(c.s1())?;
    let again = c.api.push_one(c.a(), c.s1(), create)?;
    ensure!(
        status(&again) == "applied" && version(&again) == Some(v1),
        "resend changed its result: {again}"
    );
    ensure!(
        c.since(c.s1(), &cur)?.iter().all(|r| r["id"] != id),
        "resend produced a new version"
    );
    let (all, _) = c.api.pull_all(c.a(), c.s1(), None, Some(1000))?;
    let rec = last_of(&all, &id).ok_or("record missing")?;
    ensure!(
        rec["data"]["qty"] == 7,
        "the other device's change was undone: {rec}"
    );
    Ok(())
}

fn push_05(c: &Ctx) -> Result<(), String> {
    let r = c.api.push(
        c.a(),
        c.s1(),
        vec![
            upsert(
                "conf_items",
                &sync_core::new_id(),
                json!({"title": "ok1"}),
                None,
            ),
            upsert(
                "conf_items",
                &sync_core::new_id(),
                json!({"title": ""}),
                None,
            ),
            upsert(
                "conf_items",
                &sync_core::new_id(),
                json!({"title": "ok2"}),
                None,
            ),
        ],
    )?;
    let s: Vec<_> = r.iter().map(status).collect();
    ensure!(s == ["applied", "rejected", "applied"], "statuses: {s:?}");
    ensure!(
        r[1]["code"] == "validation_failed",
        "code: {}",
        r[1]["code"]
    );
    Ok(())
}

fn push_06(c: &Ctx) -> Result<(), String> {
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert(
            "conf_items",
            &sync_core::new_id(),
            json!({"title": ""}),
            None,
        ),
    )?;
    ensure!(status(&r) == "rejected", "expected rejected: {r}");
    ensure!(
        r.get("record") == Some(&Value::Null),
        "expected record null for a new record: {r}"
    );
    let (id, v) = c.create(c.s1(), "conf_items", json!({"title": "keep me"}))?;
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": ""}), Some(v)),
    )?;
    ensure!(status(&r) == "rejected", "expected rejected: {r}");
    ensure!(
        r["record"]["data"]["title"] == "keep me" && r["record"]["version"] == v,
        "record not current: {r}"
    );
    Ok(())
}

fn push_07(c: &Ctx) -> Result<(), String> {
    let (id, v) = c.create(c.s1(), "conf_items", json!({"title": "n", "note": "x"}))?;
    let cur = c.tail(c.s1())?;
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"note": null}), Some(v)),
    )?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"].get("note") == Some(&Value::Null),
        "note not null: {rec}"
    );
    Ok(())
}

fn push_08(c: &Ctx) -> Result<(), String> {
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert(
            "conf_readonly",
            &sync_core::new_id(),
            json!({"title": "x"}),
            None,
        ),
    )?;
    ensure!(
        status(&r) == "rejected" && r["code"] == "read_only",
        "expected rejected read_only: {r}"
    );
    Ok(())
}

fn push_09(c: &Ctx) -> Result<(), String> {
    let r = c.api.push(
        c.a(),
        c.s1(),
        vec![
            upsert(
                "conf_nothing",
                &sync_core::new_id(),
                json!({"title": "x"}),
                None,
            ),
            upsert(
                "conf_items",
                &sync_core::new_id(),
                json!({"title": "x", "colour": "red"}),
                None,
            ),
        ],
    )?;
    ensure!(
        r[0]["status"] == "rejected" && r[0]["code"] == "unknown_table",
        "unknown table: {}",
        r[0]
    );
    ensure!(
        r[1]["status"] == "rejected" && r[1]["code"] == "unknown_field",
        "unknown field: {}",
        r[1]
    );
    Ok(())
}

fn push_10(c: &Ctx) -> Result<(), String> {
    let body = json!({
        "changes": [upsert("conf_items", &sync_core::new_id(), json!({"title": "x"}), None)],
        "future_extension": {"anything": true}
    });
    let r = c.api.push_raw(c.a(), c.s1(), &body)?;
    ensure!(r.status == 200, "expected 200, got {} {}", r.status, r.body);
    ensure!(
        r.body["results"][0]["status"] == "applied",
        "not applied: {}",
        r.body
    );
    Ok(())
}

fn push_11(c: &Ctx) -> Result<(), String> {
    let body = json!({"changes": [upsert("conf_items", &sync_core::new_id(), json!({"title": "x"}), None)]});
    let r = c.api.push_raw(c.b(), c.s1(), &body)?;
    ensure!(
        r.status == 403 && r.error_code() == Some("scope_forbidden"),
        "expected 403 scope_forbidden, got {} {}",
        r.status,
        r.body
    );
    Ok(())
}

fn push_12(c: &Ctx) -> Result<(), String> {
    let body = json!({"changes": [upsert("conf_items", &sync_core::new_id(), json!({"title": "x"}), None)]});
    let r = c.api.push_raw(c.b(), c.s2(), &body)?;
    ensure!(
        r.status == 403 || r.status == 404,
        "expected 403 or 404, got {}",
        r.status
    );
    Ok(())
}

fn push_13(c: &Ctx) -> Result<(), String> {
    let (id, v) = c.create(c.s1(), "conf_items", json!({"title": "mine"}))?;
    let r = c.api.push_one(
        c.a(),
        c.s2(),
        upsert("conf_items", &id, json!({"title": "stolen"}), Some(v)),
    )?;
    ensure!(
        status(&r) == "rejected" && r["code"] == "forbidden",
        "expected rejected forbidden: {r}"
    );
    let (all, _) = c.api.pull_all(c.a(), c.s1(), None, Some(1000))?;
    let rec = last_of(&all, &id).ok_or("record missing")?;
    ensure!(
        rec["data"]["title"] == "mine" && rec["version"] == v,
        "S1 record changed: {rec}"
    );
    Ok(())
}

fn push_14(c: &Ctx) -> Result<(), String> {
    let s = c.api.session(Some(c.a()))?;
    let max = s.body["limits"]["max_push_changes"]
        .as_u64()
        .ok_or("no limit")? as usize;
    let cur = c.tail(c.s1())?;
    let changes: Vec<_> = (0..=max)
        .map(|_| {
            upsert(
                "conf_items",
                &sync_core::new_id(),
                json!({"title": "big"}),
                None,
            )
        })
        .collect();
    let r = c
        .api
        .push_raw(c.a(), c.s1(), &json!({ "changes": changes }))?;
    ensure!(
        r.status == 413 && r.error_code() == Some("too_large"),
        "expected 413 too_large, got {} {}",
        r.status,
        r.body
    );
    ensure!(
        c.since(c.s1(), &cur)?.is_empty(),
        "part of the batch was applied"
    );
    Ok(())
}

fn push_15(c: &Ctx) -> Result<(), String> {
    let values = [
        json!("plain text"),
        json!(42),
        json!("12345678901234567890"),
        json!("10.25"),
        json!(1.5),
        json!(true),
        json!(null),
        json!("2026-11-26T08:15:00Z"),
        json!({"nested": [1, "two", {"three": 3}]}),
        json!(["a", 1, null]),
        json!("Шчасце, ünïcødé ✓ 🌴"),
        json!(-9007199254740991i64),
    ];
    let cur = c.tail(c.s1())?;
    let mut ids = Vec::new();
    for v in &values {
        let (id, _) = c.create(c.s1(), "conf_items", json!({"title": "types", "note": v}))?;
        ids.push(id);
    }
    let records = c.since(c.s1(), &cur)?;
    for (id, v) in ids.iter().zip(&values) {
        let rec = last_of(&records, id).ok_or("record missing")?;
        ensure!(
            &rec["data"]["note"] == v,
            "value changed: sent {v}, got {}",
            rec["data"]["note"]
        );
    }
    Ok(())
}

// ===================================================================== pull

fn check_ordered(records: &[Value]) -> Result<(), String> {
    let versions: Vec<i64> = records
        .iter()
        .map(|r| r["version"].as_i64().unwrap_or(-1))
        .collect();
    ensure!(
        versions.windows(2).all(|w| w[0] < w[1]),
        "versions not ascending: {versions:?}"
    );
    Ok(())
}

fn pull_01(c: &Ctx) -> Result<(), String> {
    c.create(c.s1(), "conf_items", json!({"title": "p1"}))?;
    let page = c.api.pull(c.a(), c.s1(), None, Some(1000))?;
    ensure!(!page.records.is_empty(), "no records from the beginning");
    ensure!(
        !page.cursor.is_empty() && page.cursor.len() <= 255,
        "bad cursor"
    );
    check_ordered(&page.records)
}

fn pull_02(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let page = c.api.pull(c.a(), c.s1(), Some(&cur), None)?;
    ensure!(
        page.records.is_empty(),
        "records at the tail: {}",
        page.records.len()
    );
    ensure!(!page.has_more, "has_more at the tail");
    ensure!(!page.cursor.is_empty(), "no cursor at the tail");
    Ok(())
}

fn pull_03(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    for i in 0..25 {
        c.create(c.s1(), "conf_items", json!({"title": format!("page {i}")}))?;
    }
    let one: HashSet<String> = c
        .since(c.s1(), &cur)?
        .iter()
        .map(|r| r["id"].as_str().unwrap_or("").to_string())
        .collect();
    ensure!(
        one.len() >= 25,
        "single pull returned {} records",
        one.len()
    );
    let mut paged = HashSet::new();
    let mut cursor = cur;
    for _ in 0..100 {
        let page = c.api.pull(c.a(), c.s1(), Some(&cursor), Some(10))?;
        ensure!(
            page.records.len() <= 10,
            "page larger than limit: {}",
            page.records.len()
        );
        let ids: HashSet<_> = page
            .records
            .iter()
            .map(|r| r["id"].as_str().unwrap_or("").to_string())
            .collect();
        ensure!(
            ids.len() == page.records.len(),
            "duplicate record within a page"
        );
        check_ordered(&page.records)?;
        paged.extend(ids);
        cursor = page.cursor;
        if !page.has_more {
            ensure!(
                paged == one,
                "paged set differs from single pull ({} vs {})",
                paged.len(),
                one.len()
            );
            return Ok(());
        }
    }
    Err("has_more never became false".into())
}

fn pull_04(c: &Ctx) -> Result<(), String> {
    let (id, v) = c.create(c.s1(), "conf_items", json!({"title": "to delete"}))?;
    let cur = c.tail(c.s1())?;
    let r = c
        .api
        .push_one(c.a(), c.s1(), delete("conf_items", &id, Some(v)))?;
    ensure!(status(&r) == "applied", "delete not applied: {r}");
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(rec["deleted"] == true, "not a tombstone: {rec}");
    ensure!(
        rec.get("data").is_none_or(Value::is_null),
        "tombstone has data: {rec}"
    );
    Ok(())
}

fn pull_05(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let (id, mut v) = c.create(c.s1(), "conf_items", json!({"title": "v0", "qty": 0}))?;
    for i in 1..=3 {
        let r = c.api.push_one(
            c.a(),
            c.s1(),
            upsert("conf_items", &id, json!({"qty": i}), Some(v)),
        )?;
        v = version(&r).ok_or("no version")?;
    }
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"]["qty"] == 3 && rec["version"] == v,
        "last appearance is not final: {rec}"
    );
    Ok(())
}

fn pull_06(c: &Ctx) -> Result<(), String> {
    const WRITERS: usize = 8;
    const CHANGES: usize = 50;
    for run in 0..c.cfg.concurrency_runs.max(1) {
        let start = c.tail(c.s1())?;
        let done = AtomicBool::new(false);
        let expected: Mutex<HashMap<String, i64>> = Mutex::new(HashMap::new());
        let seen: Mutex<HashMap<String, i64>> = Mutex::new(HashMap::new());
        let failure: Mutex<Option<String>> = Mutex::new(None);

        std::thread::scope(|s| {
            let reader = s.spawn(|| -> Result<(), String> {
                let api = c.api.device();
                let mut cursor = start.clone();
                let record = |records: Vec<Value>| {
                    let mut seen = seen.lock().unwrap();
                    for r in records {
                        let id = r["id"].as_str().unwrap_or("").to_string();
                        let v = r["version"].as_i64().unwrap_or(0);
                        let e = seen.entry(id).or_insert(0);
                        *e = (*e).max(v);
                    }
                };
                loop {
                    let finished = done.load(Ordering::SeqCst);
                    let page = api.pull(c.a(), c.s1(), Some(&cursor), Some(50))?;
                    let more = page.has_more;
                    record(page.records);
                    cursor = page.cursor;
                    if finished && !more {
                        return Ok(());
                    }
                }
            });
            let writers: Vec<_> = (0..WRITERS)
                .map(|w| {
                    let expected = &expected;
                    let failure = &failure;
                    s.spawn(move || {
                        let api = c.api.device();
                        let mut mine: Vec<(String, i64)> = Vec::new();
                        for i in 0..CHANGES {
                            let change = if i % 3 == 2 && !mine.is_empty() {
                                let (id, v) = mine[i % mine.len()].clone();
                                upsert("conf_items", &id, json!({"qty": i}), Some(v))
                            } else {
                                upsert(
                                    "conf_items",
                                    &sync_core::new_id(),
                                    json!({"title": format!("w{w} c{i}"), "qty": i}),
                                    None,
                                )
                            };
                            match api.push_one(c.a(), c.s1(), change.clone()) {
                                Ok(r) if status(&r) == "applied" => {
                                    let id = change["id"].as_str().unwrap().to_string();
                                    let v = version(&r).unwrap_or(0);
                                    match mine.iter_mut().find(|(x, _)| *x == id) {
                                        Some(e) => e.1 = v,
                                        None => mine.push((id.clone(), v)),
                                    }
                                    let mut ex = expected.lock().unwrap();
                                    let e = ex.entry(id).or_insert(0);
                                    *e = (*e).max(v);
                                }
                                Ok(r) => {
                                    *failure.lock().unwrap() =
                                        Some(format!("writer {w}: not applied: {r}"))
                                }
                                Err(e) => {
                                    *failure.lock().unwrap() = Some(format!("writer {w}: {e}"))
                                }
                            }
                        }
                    })
                })
                .collect();
            for w in writers {
                let _ = w.join();
            }
            done.store(true, Ordering::SeqCst);
            if let Ok(Err(e)) = reader.join() {
                *failure.lock().unwrap() = Some(format!("reader: {e}"));
            }
        });

        if let Some(f) = failure.into_inner().unwrap() {
            return Err(format!("run {}: {f}", run + 1));
        }
        let expected = expected.into_inner().unwrap();
        let seen = seen.into_inner().unwrap();
        let missing: Vec<_> = expected
            .iter()
            .filter(|(id, v)| seen.get(*id).copied().unwrap_or(0) < **v)
            .collect();
        ensure!(
            missing.is_empty(),
            "run {}: the reader missed {} of {} records at their final version, for example {:?}",
            run + 1,
            missing.len(),
            expected.len(),
            missing.first()
        );
    }
    Ok(())
}

fn pull_07(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let (id, v) = c.create(c.s1(), "conf_items", json!({"title": "own"}))?;
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["version"] == v,
        "own change returned with a different version: {rec}"
    );
    Ok(())
}

fn pull_08(c: &Ctx) -> Result<(), String> {
    let r = c.api.pull_raw(c.b(), c.s2(), None, None)?;
    ensure!(
        r.status == 403 || r.status == 404,
        "expected 403 or 404, got {}",
        r.status
    );
    Ok(())
}

fn pull_09(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let (id, _) = c.create(c.s1(), "conf_items", json!({"title": "partial"}))?;
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"] == json!({"title": "partial", "qty": null, "note": null}),
        "not every field present: {rec}"
    );
    Ok(())
}

fn pull_10(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    c.create(c.s2(), "conf_items", json!({"title": "elsewhere"}))?;
    let records = c.since(c.s1(), &cur)?;
    ensure!(
        records.is_empty(),
        "S1 returned {} records after a change in S2",
        records.len()
    );
    Ok(())
}

// ===================================================================== conflicts

fn conf_01(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_items", json!({"title": "base", "qty": 0}))?;
    let cur = c.tail(c.s1())?;
    let d2 = c.api.device();
    let r1 = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": "from 1"}), Some(n)),
    )?;
    let r2 = d2.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"qty": 2}), Some(n)),
    )?;
    ensure!(
        status(&r1) == "applied" && status(&r2) == "applied",
        "not both applied: {r1} {r2}"
    );
    let rec = &c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"]["title"] == "from 1" && rec["data"]["qty"] == 2,
        "merge lost a field: {rec}"
    );
    Ok(())
}

fn conf_02(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_items", json!({"title": "base"}))?;
    let cur = c.tail(c.s1())?;
    let r1 = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": "first"}), Some(n)),
    )?;
    let r2 = c.api.device().push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": "second"}), Some(n)),
    )?;
    ensure!(
        status(&r1) == "applied" && status(&r2) == "applied",
        "not both applied: {r1} {r2}"
    );
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"]["title"] == "second",
        "later arrival did not win: {rec}"
    );
    Ok(())
}

fn conf_03(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_locked", json!({"title": "base", "qty": 0}))?;
    let cur = c.tail(c.s1())?;
    let r1 = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_locked", &id, json!({"title": "first"}), Some(n)),
    )?;
    ensure!(status(&r1) == "applied", "first not applied: {r1}");
    let r2 = c.api.device().push_one(
        c.a(),
        c.s1(),
        upsert("conf_locked", &id, json!({"title": "second"}), Some(n)),
    )?;
    ensure!(
        status(&r2) == "conflict" && r2["reason"] == "stale",
        "expected conflict stale: {r2}"
    );
    ensure!(
        r2["record"]["data"]["title"] == "first",
        "conflict without current record: {r2}"
    );
    let rec = c.latest(c.s1(), &cur, &id)?;
    ensure!(
        rec["data"]["title"] == "first",
        "stale change was stored: {rec}"
    );
    Ok(())
}

fn conf_04(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_locked", json!({"title": "base"}))?;
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_locked", &id, json!({"qty": 1}), Some(n)),
    )?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    Ok(())
}

fn conf_05(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_items", json!({"title": "doomed"}))?;
    let d = c
        .api
        .push_one(c.a(), c.s1(), delete("conf_items", &id, Some(n)))?;
    ensure!(status(&d) == "applied", "delete not applied: {d}");
    let cur = c.tail(c.s1())?;
    let r = c.api.device().push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": "revive"}), Some(n)),
    )?;
    ensure!(
        status(&r) == "conflict" && r["reason"] == "deleted",
        "expected conflict deleted: {r}"
    );
    ensure!(
        r["record"]["deleted"] == true,
        "conflict should carry the tombstone: {r}"
    );
    ensure!(
        c.since(c.s1(), &cur)?.iter().all(|x| x["id"] != id),
        "the deleted record changed"
    );
    Ok(())
}

fn conf_06(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_items", json!({"title": "twice"}))?;
    let r1 = c
        .api
        .push_one(c.a(), c.s1(), delete("conf_items", &id, Some(n)))?;
    let r2 = c
        .api
        .push_one(c.a(), c.s1(), delete("conf_items", &id, Some(n)))?;
    ensure!(
        status(&r1) == "applied" && status(&r2) == "applied",
        "not both applied: {r1} {r2}"
    );
    ensure!(
        r1["version"] == r2["version"],
        "second delete should return the tombstone's version: {r1} {r2}"
    );
    Ok(())
}

fn conf_07(c: &Ctx) -> Result<(), String> {
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        delete("conf_items", &sync_core::new_id(), None),
    )?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    ensure!(
        r.get("version") == Some(&Value::Null),
        "expected version null: {r}"
    );
    Ok(())
}

fn conf_08(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_items", json!({"title": "x"}))?;
    c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"qty": 1}), Some(n)),
    )?;
    let cur = c.tail(c.s1())?;
    let r = c
        .api
        .device()
        .push_one(c.a(), c.s1(), delete("conf_items", &id, Some(n)))?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    ensure!(
        c.latest(c.s1(), &cur, &id)?["deleted"] == true,
        "record not deleted"
    );
    Ok(())
}

fn conf_09(c: &Ctx) -> Result<(), String> {
    let (id, n) = c.create(c.s1(), "conf_locked", json!({"title": "x"}))?;
    let u = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_locked", &id, json!({"qty": 1}), Some(n)),
    )?;
    ensure!(status(&u) == "applied", "update not applied: {u}");
    let r = c
        .api
        .device()
        .push_one(c.a(), c.s1(), delete("conf_locked", &id, Some(n)))?;
    ensure!(
        status(&r) == "conflict" && r["reason"] == "stale",
        "expected conflict stale: {r}"
    );
    ensure!(
        r["record"]["deleted"] == false,
        "record should still exist: {r}"
    );
    Ok(())
}

fn conf_10(c: &Ctx) -> Result<(), String> {
    let cur = c.tail(c.s1())?;
    let id = sync_core::new_id();
    let r = c.api.push_one(
        c.a(),
        c.s1(),
        upsert("conf_items", &id, json!({"title": "late base"}), Some(7)),
    )?;
    ensure!(status(&r) == "applied", "expected applied: {r}");
    c.latest(c.s1(), &cur, &id)?;
    Ok(())
}

// ===================================================================== errors

fn err_01(c: &Ctx) -> Result<(), String> {
    let s = c.api.session(Some(c.a()))?;
    let old = s.body["min_schema_version"]
        .as_i64()
        .ok_or("no min_schema_version")?
        - 1;
    let path = client::changes_path(c.s1());
    let body = json!({"changes": []});
    let checks = [
        (
            "session",
            c.api
                .request("GET", "/session", Some(c.a()), old, Body::None)?,
        ),
        (
            "pull",
            c.api.request("GET", &path, Some(c.a()), old, Body::None)?,
        ),
        (
            "push",
            c.api
                .request("POST", &path, Some(c.a()), old, Body::Json(&body))?,
        ),
    ];
    for (name, r) in checks {
        ensure!(
            r.status == 426 && r.error_code() == Some("upgrade_required"),
            "{name}: expected 426, got {} {}",
            r.status,
            r.body
        );
    }
    Ok(())
}

fn err_02(c: &Ctx) -> Result<(), String> {
    let s = c.api.session(Some(c.a()))?;
    let newer = s.body["schema_version"]
        .as_i64()
        .ok_or("no schema_version")?
        + 1;
    let r = c
        .api
        .request("GET", "/session", Some(c.a()), newer, Body::None)?;
    ensure!(
        r.status == 409 && r.error_code() == Some("schema_ahead"),
        "expected 409 schema_ahead, got {} {}",
        r.status,
        r.body
    );
    Ok(())
}

fn err_03(c: &Ctx) -> Result<(), String> {
    let r = c.api.request(
        "POST",
        &client::changes_path(c.s1()),
        Some(c.a()),
        c.cfg.schema,
        Body::Raw(b"{\"changes\": [}"),
    )?;
    ensure!(
        r.status == 400 && r.error_code() == Some("bad_request"),
        "expected 400 bad_request, got {} {}",
        r.status,
        r.body
    );
    Ok(())
}

fn err_04(c: &Ctx) -> Result<(), String> {
    let r = c.api.pull_raw(c.a(), c.s1(), Some("garbage!!"), None)?;
    ensure!(
        r.status == 400 || r.status == 410,
        "expected 400 or 410, got {} {}",
        r.status,
        r.body
    );
    ensure!(
        r.error_code().is_some(),
        "no standard error body: {}",
        r.body
    );
    Ok(())
}

// ===================================================================== optional

fn opt_01(c: &Ctx) -> Result<(), String> {
    let hooks = c.cfg.test_url.as_deref().ok_or("no test url")?;
    let (live, _) = c.create(c.s1(), "conf_items", json!({"title": "survivor"}))?;
    let (gone, v) = c.create(c.s1(), "conf_items", json!({"title": "goner"}))?;
    let old = c.tail(c.s1())?;
    c.api
        .push_one(c.a(), c.s1(), delete("conf_items", &gone, Some(v)))?;
    hook_post(hooks, "prune", &json!({}))?;
    let r = c.api.pull_raw(c.a(), c.s1(), Some(&old), None)?;
    ensure!(
        r.status == 410 && r.error_code() == Some("cursor_expired"),
        "expected 410 cursor_expired, got {} {}",
        r.status,
        r.body
    );
    // Small pages, so the download from the beginning spans several requests.
    let (all, cursor) = c.api.pull_all(c.a(), c.s1(), None, Some(7))?;
    ensure!(
        last_of(&all, &live).is_some_and(|r| r["deleted"] == false),
        "live record missing from a full pull"
    );
    ensure!(
        last_of(&all, &gone).is_none_or(|r| r["deleted"] == true),
        "deleted record returned as live"
    );
    let after = c.api.pull_raw(c.a(), c.s1(), Some(&cursor), None)?;
    ensure!(
        after.status == 200,
        "the cursor that ends a full pull is not accepted: {} {}",
        after.status,
        after.body
    );
    c.cursors.lock().unwrap().insert(c.s1().to_string(), cursor);
    Ok(())
}

fn opt_02(c: &Ctx) -> Result<(), String> {
    let hooks = c.cfg.test_url.as_deref().ok_or("no test url")?;
    hook_post(
        hooks,
        "access",
        &json!({"token": c.b(), "scope": c.s2(), "access": "read"}),
    )?;
    let granted = grants(&c.api.session(Some(c.b()))?);
    hook_post(
        hooks,
        "access",
        &json!({"token": c.b(), "scope": c.s2(), "access": null}),
    )?;
    ensure!(
        granted.get(c.s2()).map(String::as_str) == Some("read"),
        "grant not visible: {granted:?}"
    );
    let revoked = grants(&c.api.session(Some(c.b()))?);
    ensure!(
        !revoked.contains_key(c.s2()),
        "revoked scope still listed: {revoked:?}"
    );
    let r = c.api.pull_raw(c.b(), c.s2(), None, None)?;
    ensure!(
        r.status == 403 || r.status == 404,
        "pull after revoke: expected 403 or 404, got {}",
        r.status
    );
    Ok(())
}
