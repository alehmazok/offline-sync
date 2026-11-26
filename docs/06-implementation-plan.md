# 6. Implementation plan

The engine is built in seven phases. Each phase ends with something that
runs and a test that proves it. The first app and its backend are brought in
at phase 4, early enough to correct the design before the other two apps
depend on it.

## 6.1 Order of work

| Phase | Deliverable | Done when | Rough effort |
| --- | --- | --- | --- |
| 0. Foundations | Cargo workspace, CI, protocol types, reference server, first conformance tests | The reference server passes the session, push and pull groups | 1.5 weeks |
| 1. Store | SQLite storage, manifest loading, trigger generation, outbox, migrations runner | A write to a manifest table produces the right outbox entry in every case of section 4.4 | 2 weeks |
| 2. Sync loop | Transport, push, pull, result handling, conflict rules, retry | Two engine instances converge through the reference server; the convergence property test passes with failure injection | 3 weeks |
| 3. Flutter binding | Dart package, streams for `watch`, `status`, `events`; iOS and Android builds | A sample Flutter app edits data offline on two phones and converges | 1.5 weeks |
| 4. First real project | Sync layer in the first backend; first app moved onto the engine | The backend passes the full conformance suite; the app runs on the engine in a test build | 3 weeks |
| 5. Hardening | Forced upgrade, expired cursor resync, scope removal, sign-out, outbox-aware migrations, logging | Every row of the failure table in section 4.8 has an automated test | 2 weeks |
| 6. Remaining projects | Sync layers in the other two backends; the other two apps | Each backend passes the suite | 1.5 weeks per project |

Total to the first app in production: about 13 weeks of focused work.
All three apps: about 16 weeks.

These figures are estimates for one developer with working Rust
experience. Learning Rust alongside would add to phases 0 to 2. The largest
uncertainty is phase 4, because moving an existing app's data layer onto
the engine depends on how that app is built.

## 6.2 Phase details

### Phase 0: Foundations

- Workspace with the crates listed in section 6.3, formatting, lints and
  tests in CI.
- `sync-core`: Rust types for every protocol message with JSON
  serialisation, checked against `spec/openapi.yaml`.
- `sync-refserver`: the server guide implemented in the simplest possible
  way (in-process, SQLite or in-memory). It doubles as an executable
  version of the spec and as the test server for everything after.
- `sync-conformance`: the runner, plus the session, push and pull groups.

Building the server side first is deliberate. It forces the protocol to be
precise before any client code depends on it.

### Phase 1: Store

- Open the database in WAL mode; single writer, read pool.
- Parse and validate the manifest against the actual tables.
- Generate insert, update and delete triggers; capture switch for engine
  writes.
- Engine tables and their access layer.
- Migration runner with the outbox instructions.
- `execute`, `write_batch`, `query`.

### Phase 2: Sync loop

- Transport interface with two implementations: HTTP, and in-process for
  tests.
- Batch building with merging (section 4.6).
- Session handling and scope bookkeeping.
- Push with result handling; pull with page application (section 4.7).
- Retry policy, token refresh on 401.
- The simulation harness and the convergence property test.

This phase carries the most risk and gets the most tests. No binding work
starts until the property test is stable.

### Phase 3: Flutter binding

- Wrapper crate exposing the public API through flutter_rust_bridge.
- Dart package: typed configuration, streams, error mapping.
- Scheduling helper: sync on foreground, on connectivity, debounced after
  writes.
- `watch`: re-run queries when the tables they read change.
- Build setup for iOS and Android, including CI builds of the native
  libraries.

### Phase 4: First real project

- Choose the app with the simplest data and the fewest users.
- Implement the sync layer in its backend following the server guide; run
  the conformance suite in that backend's CI.
- Backfill existing server data through the write path.
- Move the app's data access onto the engine, table by table.
- Plan the on-device transition: existing local data is either re-downloaded
  after update, or imported once by a migration step.
- Collect every place where the design was awkward and fix the design
  before phase 6.

### Phase 5: Hardening

- 426 handling end to end, with the app showing an update prompt.
- 410 resync.
- Scope added and removed while the app is in use.
- Sign-out with and without unsent changes.
- Structured logs and a debug dump of sync state for support.
- Size and timing check: a first sync of a realistic dataset on a low-end
  phone.

### Phase 6: Remaining projects

- One backend sync layer per project, each verified by the suite.
- Write down, per database, anything the server guide got wrong or left
  out.

### Later, when needed

- Swift and Kotlin bindings with UniFFI. Nothing in the engine changes.
- A "something changed" signal for near-real-time updates.
- File sync, atomic change groups, Flutter web.

## 6.3 Crates

| Crate | Responsibility | Main dependencies |
| --- | --- | --- |
| `sync-core` | Protocol types, merge and conflict rules, batch building. No I/O, so it is trivially testable | serde, uuid |
| `sync-store` | SQLite access, triggers, engine tables, migrations | rusqlite with bundled SQLite |
| `sync-net` | HTTP transport, gzip, retry and backoff | an HTTP client with rustls |
| `sync-engine` | The sync cycle, public Rust API, status and events | the three above, an async runtime |
| `sync-refserver` | Reference server | a small HTTP framework |
| `sync-conformance` | Conformance runner | an HTTP client |
| `bindings/flutter` | Dart package | flutter_rust_bridge |
| `bindings/uniffi` | Swift and Kotlin, later | uniffi |

Dependency rule: `sync-core` depends on nothing in this repository, and no
crate under `crates/` knows that Flutter exists.

## 6.4 Risks

| Risk | Likelihood | Effect | Mitigation |
| --- | --- | --- | --- |
| The design does not fit a real app's data (for example, partial access inside a team) | Medium | Protocol change after code exists | Confirm the assumptions in architecture section 1.7 before phase 1; bring in the first app at phase 4, not at the end |
| Subtle data loss in the sync loop | Medium | Lost user work, hard to diagnose | Property test with failure injection from the start of phase 2; no shortcut around it |
| A backend numbers changes incorrectly | Medium | Devices silently miss changes | Conformance test C-PULL-06 in every backend's CI |
| Server writes that bypass the write path | High | Changes never reach devices | One write function per backend; code review rule; a periodic consistency check comparing data and index |
| Mobile build and packaging of the native library takes longer than expected | Medium | Delay in phase 3 | Build a "hello world" binding for both platforms during phase 0, before it is needed |
| Moving an existing app onto engine-owned SQLite is larger than estimated | Medium | Delay in phase 4 | Start with the smallest app; migrate table by table behind the existing repository layer |
| Over-generalising for three apps at once | Medium | A slow, awkward engine | Build only what the first app needs; add features when the second app asks for them |

## 6.5 Decisions needed before coding starts

1. ~~Confirm or correct the four assumptions in
   [architecture section 1.7](01-architecture.md#17-assumptions).~~
   Confirmed.
2. Which app goes first. Open.
3. For that app: its scopes, its synced tables, and which tables need
   `server_wins`.
4. ~~Whether existing local data on users' devices must be preserved
   through the switch, or can be re-downloaded.~~ It can be re-downloaded.

## 6.6 Progress

| Phase | Status | Where |
| --- | --- | --- |
| 0. Foundations | Done | `crates/sync-core`, `crates/sync-refserver`, `crates/sync-conformance`, `.github/workflows/ci.yml` |
| 1. Store | Done | `crates/sync-store` |
| 2. Sync loop | Done | `crates/sync-net`, `crates/sync-engine` |
| 3. Flutter binding | Done on emulators and simulators; not yet run on physical phones | `crates/sync-ffi`, `bindings/flutter`, `bindings/flutter/example` |
| 4. First real project | Not started; needs the choice of app | |
| 5. Hardening | Done | `crates/sync-engine/tests/hardening.rs` |
| 6. Remaining projects | Not started | |

Phase 3 in CI (`.github/workflows/ci.yml`): the bindings are regenerated
on every push; the Dart package and the example app pass `flutter analyze`;
the example builds for Android (debug APK) and the iOS simulator. On an
Android emulator (API 34) the end-to-end test
(`bindings/flutter/example/integration_test/sync_test.dart`) passes:
two engines in one app sync through `sync-refserver` over HTTP, merge
offline edits to different fields, propagate deletes, call the Dart token
callback (and fail with `unauthorized` when it returns no token), deliver
watch, status and event streams, and cancel streams promptly. On the iOS
simulator, CI launches the example, which writes a note and syncs it, and
finds the note on the server. `flutter test` itself cannot attach to the
app on hosted macOS runners, so the full test has not passed on iOS yet.

Not done yet: running the example on two physical phones, and the
first-sync timing on a low-end phone (both need hardware).

### Tests

| Suite | What it covers | Command |
| --- | --- | --- |
| Flutter adapter (3) | Stream registration bookkeeping; the example's sync setup through two engines over HTTP | `cargo test --manifest-path bindings/flutter/rust/Cargo.toml` |
| Flutter end-to-end (8) | The binding on a device against `sync-refserver` | `flutter test integration_test` in `bindings/flutter/example` (see its README) |
| Unit tests | Protocol types, batch merging, client retry, store triggers and migrations | `cargo test --workspace` |
| Conformance (44 tests) | The reference server against the protocol | `cargo test -p sync-conformance` |
| Engine scenarios (19) | Two-device sync, conflicts, crashes at every point, watches | `cargo test -p sync-engine --test engine` |
| Hardening (15) | Every failure case of section 4.8 of the engine design, scopes, sign-out, migrations, moves | `cargo test -p sync-engine --test hardening` |
| Convergence | 25 random schedules with lost requests, lost responses and crashes | `cargo test -p sync-engine --test convergence` |
| Convergence, long | 300 schedules, 4 devices, 400 steps each, release build, about 2 minutes | `cargo test -p sync-engine --release --test convergence -- --ignored` |

A first download of 20,000 records from an in-process server takes about
1.2 seconds on the development machine
(`cargo test --release -p sync-engine --test hardening first_sync_timing -- --ignored --nocapture`).
It has not been measured on a phone.

### Problems the tests found

| Found by | Problem | Fix |
| --- | --- | --- |
| C-OPT-01 | After tombstones were pruned, the cursor ending a full download could itself be expired, so a device would download forever | Caught-up cursors are the scope counter; download cursors stay valid until the next pruning (protocol 5.2, rules 9 and 10) |
| Long convergence run, 9 of 300 seeds | A download from the beginning carries no tombstones, so a device that already held rows (its own uploads, an interrupted download) never learnt of their deletion | Every download from the beginning ends with resync cleanup (protocol 5.2, client rule 5) |
| Hardening test | A write to a read-only scope was dropped but stayed on the device, because the re-download skipped the unchanged row | The row's version is forgotten when its change is dropped |
| Review | A record moved between scopes could be skipped, since version numbers were compared across scopes, and its local scope column was never updated | Row versions are compared only within one scope; the engine may change the scope column |
| Mutation check | Removing the step that rebases later edits after an upload went unnoticed by the random test | Added a test that edits a record while its previous edit is uploading |
| Generating the binding | The adapter's private helper types sat in the generator's input and were exported to Dart, where the generated code did not compile | Helpers moved to `bindings/flutter/rust/src/adapter.rs`, outside `crate::api` |
| Review of the generated API | Stream functions return a Dart `Stream`, so the registration ID never reached Dart and a cancelled stream stayed registered forever | Superseded by the next row |
| Android integration test | `await subscription.cancel()` on a watch or status stream never completed: a bridge stream only finishes cancelling when Rust sends to it again or drops its sink | Streams get an ID from Dart; cancel calls `unsubscribe`, which drops the sink and ends the stream (`lib/src/streams.dart`). Tests require cancel within 5 s |
