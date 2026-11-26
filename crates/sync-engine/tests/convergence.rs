//! Convergence property test (`docs/04-client-engine.md`, section 4.13).
//!
//! Several devices make random edits while requests and responses are lost
//! and the app is killed at random points. Then the faults stop and every
//! device syncs. Required afterwards:
//!
//! 1. every device holds exactly the server's data for its scopes;
//! 2. no device has unsent changes;
//! 3. every record created anywhere and never targeted by a delete exists
//!    on the server: no acknowledged work was lost.

mod common;

use common::*;
use serde_json::json;
use std::collections::BTreeSet;
use sync_engine::*;

struct World {
    rng: Rng,
    server: std::sync::Arc<sync_refserver::RefServer>,
    devices: Vec<Device>,
    created: BTreeSet<(String, String)>,
    deleted: BTreeSet<(String, String)>,
}

fn writable(token: &str) -> &'static [&'static str] {
    match token {
        "alice" => &["team:1", "team:2"],
        _ => &["team:1"],
    }
}

fn scopes_of(d: &Device) -> Vec<String> {
    let mut s: Vec<String> = writable(&d.token).iter().map(|s| s.to_string()).collect();
    s.push(d.default_scope.clone());
    s
}

fn local_ids(d: &Device, table: &str) -> Vec<String> {
    d.e()
        .query(&format!("SELECT id FROM {table} ORDER BY id"), &[])
        .unwrap()
        .rows
        .into_iter()
        .filter_map(|r| match &r[0] {
            SqlValue::Text(s) => Some(s.clone()),
            _ => None,
        })
        .collect()
}

impl World {
    fn new(seed: u64, n_devices: usize) -> World {
        let server = server();
        let mut rng = Rng::new(seed);
        let devices = (0..n_devices)
            .map(|i| {
                let token = if i % 2 == 0 { "alice" } else { "bob" };
                let d = Device::new(&format!("d{i}"), token, &server, rng.next());
                d.transport.set_faults(Faults {
                    drop_request: 0.1,
                    drop_response: 0.1,
                    drop_push_response: 0.1,
                    offline: false,
                });
                *d.crash.rng.lock().unwrap() = Some((Rng::new(rng.next()), 0.04));
                d
            })
            .collect();
        World {
            rng,
            server,
            devices,
            created: BTreeSet::new(),
            deleted: BTreeSet::new(),
        }
    }

    fn step(&mut self) -> Result<(), String> {
        let i = self.rng.below(self.devices.len() as u64) as usize;
        let roll = self.rng.below(100);
        let table = if self.rng.chance(0.3) {
            "locked"
        } else {
            "items"
        };
        let d = &mut self.devices[i];
        match roll {
            0..=27 => {
                let scope = *self.rng.pick(writable(&d.token)).unwrap();
                let id = d.e().new_id();
                let title = format!("{} {}", d.name, self.rng.below(1000));
                d.exec(
                    &format!(
                        "INSERT INTO {table} (id, scope_id, title, qty) VALUES (?1, ?2, ?3, ?4)"
                    ),
                    &[
                        id.clone().into(),
                        scope.into(),
                        title.into(),
                        (self.rng.below(100) as i64).into(),
                    ],
                );
                self.created.insert((table.to_string(), id));
            }
            28..=52 => {
                if let Some(id) = self.rng.pick(&local_ids(d, table)).cloned() {
                    let (col, val): (&str, SqlValue) = match self.rng.below(3) {
                        0 => (
                            "title",
                            format!("{} t{}", d.name, self.rng.below(1000)).into(),
                        ),
                        1 => ("qty", (self.rng.below(1000) as i64).into()),
                        _ if table == "items" => (
                            "note",
                            format!("{} n{}", d.name, self.rng.below(1000)).into(),
                        ),
                        _ => ("qty", SqlValue::Null),
                    };
                    d.exec(
                        &format!("UPDATE {table} SET {col} = ?1 WHERE id = ?2"),
                        &[val, id.into()],
                    );
                }
            }
            53..=60 => {
                if let Some(id) = self.rng.pick(&local_ids(d, table)).cloned() {
                    d.exec(
                        &format!("DELETE FROM {table} WHERE id = ?1"),
                        &[id.clone().into()],
                    );
                    self.deleted.insert((table.to_string(), id));
                }
            }
            61..=63 => {
                let theme = format!("theme {}", self.rng.below(10));
                d.exec(
                    "INSERT INTO prefs (id, theme) VALUES ('me', ?1) ON CONFLICT (id) DO UPDATE SET theme = excluded.theme",
                    &[theme.into()],
                );
                self.created.insert(("prefs".into(), "me".into()));
            }
            64..=84 => match d.e().sync_now() {
                Ok(_) => {}
                Err(EngineError::Crashed(_)) => d.restart(),
                Err(EngineError::Network(_)) | Err(EngineError::Server(_)) => {}
                Err(e) => return Err(format!("{}: unexpected sync error: {e}", d.name)),
            },
            85..=90 => d.restart(),
            _ => {
                // Server-side writes, as an admin panel would make them.
                let scope = *self.rng.pick(&["team:1", "team:2"]).unwrap();
                let records: Vec<(String, String)> = server_dump(&self.server, scope)
                    .into_keys()
                    .filter(|(t, _)| t == table)
                    .collect();
                match (self.rng.below(3), self.rng.pick(&records).cloned()) {
                    (0, Some((t, id))) => {
                        admin_write(&self.server, scope, &t, &id, None);
                        self.deleted.insert((t, id));
                    }
                    (1, Some((t, id))) => {
                        admin_write(
                            &self.server,
                            scope,
                            &t,
                            &id,
                            Some(json!({"qty": self.rng.below(1000)})),
                        );
                    }
                    _ => {
                        let id = sync_core::new_id();
                        admin_write(
                            &self.server,
                            scope,
                            table,
                            &id,
                            Some(json!({"title": "admin", "qty": 1})),
                        );
                        self.created.insert((table.to_string(), id));
                    }
                }
            }
        }
        Ok(())
    }

    fn settle(&mut self) -> Result<(), String> {
        for d in &mut self.devices {
            d.transport.set_faults(Faults::default());
            *d.crash.rng.lock().unwrap() = None;
            // A crash may have been the last thing that happened.
            d.restart();
        }
        for round in 0..3 {
            for d in &self.devices {
                d.e()
                    .sync_now()
                    .map_err(|e| format!("{} settle round {round}: {e}", d.name))?;
            }
        }
        Ok(())
    }

    fn check(&self) -> Result<(), String> {
        for d in &self.devices {
            if d.unsent() != 0 {
                return Err(format!(
                    "{} still has {} unsent changes: {}",
                    d.name,
                    d.unsent(),
                    d.e().debug_dump().unwrap()
                ));
            }
            for scope in scopes_of(d) {
                let local = d.dump(&scope);
                let remote = server_dump(&self.server, &scope);
                if local != remote {
                    let only_local: Vec<_> = local
                        .iter()
                        .filter(|(k, v)| remote.get(*k) != Some(v))
                        .take(3)
                        .collect();
                    let only_remote: Vec<_> = remote
                        .iter()
                        .filter(|(k, v)| local.get(*k) != Some(v))
                        .take(3)
                        .collect();
                    return Err(format!(
                        "{} diverged in {scope} ({} local, {} on server)\n  device: {only_local:?}\n  server: {only_remote:?}",
                        d.name,
                        local.len(),
                        remote.len()
                    ));
                }
            }
        }
        let mut on_server = BTreeSet::new();
        for scope in ["team:1", "team:2", "user:alice", "user:bob"] {
            on_server.extend(server_dump(&self.server, scope).into_keys());
        }
        for key in self.created.difference(&self.deleted) {
            if !on_server.contains(key) {
                return Err(format!("created record {key:?} was lost"));
            }
        }
        Ok(())
    }
}

fn run(seed: u64, steps: usize, devices: usize) -> Result<(), String> {
    let mut w = World::new(seed, devices);
    for s in 0..steps {
        w.step().map_err(|e| format!("step {s}: {e}"))?;
    }
    w.settle()?;
    w.check()
}

fn run_many(seeds: std::ops::Range<u64>, steps: usize, devices: usize) {
    let failures: Vec<String> = seeds
        .filter_map(|seed| {
            run(seed, steps, devices)
                .err()
                .map(|e| format!("seed {seed}: {e}"))
        })
        .collect();
    assert!(
        failures.is_empty(),
        "{} failing seeds:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

#[test]
fn devices_converge_under_failures() {
    run_many(0..25, 150, 3);
}

/// The long run used in CI: `cargo test --release --test convergence -- --ignored`.
#[test]
#[ignore]
fn devices_converge_under_failures_long() {
    run_many(1000..1300, 400, 4);
}

/// Reproduce one seed: `SEED=1027 STEPS=400 DEVICES=4 cargo test --test convergence one_seed -- --ignored --nocapture`.
#[test]
#[ignore]
fn one_seed() {
    let env = |k: &str, d: u64| {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    };
    if let Err(e) = run(
        env("SEED", 0),
        env("STEPS", 400) as usize,
        env("DEVICES", 4) as usize,
    ) {
        panic!("{e}");
    }
}
