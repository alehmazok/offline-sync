# offline-sync

An offline-first sync system for three Flutter apps that may later move to
native iOS and Android. One Rust engine owns each app's local SQLite
database; servers in any stack (PostgreSQL, MySQL or MongoDB) implement
one small HTTP/JSON protocol and prove it with a shared conformance suite.

The owner, Aleh, controls every backend. The apps and backends are not in
this repository.

## Read first

| Need | Document |
| --- | --- |
| How the parts fit, key decisions, what the owner decided | `docs/01-architecture.md` (sections 1.4, 1.7, 1.8) |
| The protocol contract | `docs/02-protocol.md`; `spec/openapi.yaml` mirrors it, and the Markdown wins on conflict |
| Server side for any stack | `docs/03-server-guide.md` |
| Engine design, and where the code differs | `docs/04-client-engine.md`, section 4.14 |
| Conformance tests | `docs/05-conformance-tests.md` |
| Progress, test inventory, bugs found so far | `docs/06-implementation-plan.md`, section 6.6 |

## Where things stand

Work is on branch `engine-implementation`, pull request #1. Not merged.

- Done and tested: phases 0, 1, 2 and 5 (`crates/`).
- Phase 3, the Flutter binding (`bindings/flutter`): written, never
  generated, compiled or run. The Rust adapter type-checks against
  flutter_rust_bridge 2.13.0. **This is the next task.**
- Phases 4 and 6 (real backends and apps): not started. The first app is
  not chosen yet; ask before assuming one.
- CI (`.github/workflows/ci.yml`) has not run yet.

## Next task: phase 3 on a machine with Flutter

Follow `bindings/flutter/README.md`:

1. Install `flutter_rust_bridge_codegen` 2.13.0. The Rust crate and the
   Dart package versions must match exactly.
2. Run `flutter_rust_bridge_codegen integrate` in `bindings/flutter`. Keep
   the existing `rust/` folder; take only `rust_builder/` and the pubspec
   changes it makes.
3. Run `flutter_rust_bridge_codegen generate`, `flutter pub get`, then
   `dart run build_runner build`.
4. Fix what the generated names break, starting with
   `lib/src/scheduler.dart` and `lib/offline_sync.dart`.
5. Build for Android and iOS. Write a small example app that edits data
   offline on two devices against `sync-refserver` and converges.
6. Record the result in `docs/06-implementation-plan.md`, section 6.6.

## Commands

```sh
cargo test --workspace                     # all fast tests (72)
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test -p sync-engine --release --test convergence -- --ignored   # long run, ~2 min

cargo run -p sync-refserver -- --listen 127.0.0.1:8080   # conformance fixture, test hooks on
cargo run -p sync-conformance -- --base-url http://127.0.0.1:8080/sync/v1 \
  --token-a token-a --token-b token-b --scope-1 team:conf1 --scope-2 team:conf2 \
  --test-url http://127.0.0.1:8080/_test
```

The reference server listens on 127.0.0.1 by default. For a phone or an
emulator, listen on `0.0.0.0` (the Android emulator reaches the host as
`10.0.2.2`). Never expose it beyond a local network: its test hooks have no
authentication.

## Rules

- **Run the tests before every commit**, together with `cargo fmt` and
  `cargo clippy -D warnings`. Any change to `crates/sync-engine/src/cycle.rs`
  or `crates/sync-store` also needs the long convergence run.
- **A protocol change touches four places:** `docs/02-protocol.md`,
  `spec/openapi.yaml`, `crates/sync-core/src/protocol.rs`, and a
  conformance test.
- **Keep the engine ignorant of apps.** No table, column or endpoint of any
  project in `crates/`. Nothing under `crates/` may know Flutter exists;
  binding code lives in `crates/sync-ffi` and `bindings/`.
- **Smart client, simple server.** Logic that can live in the engine goes
  there; servers are reimplemented per stack.
- **Every step is safe to repeat.** A sync step either commits one local
  transaction or changes nothing. New code must keep that true at every
  `FaultPoint` in `crates/sync-engine/src/types.rs`.
- **Bugs get a regression test.** When a test finds a bug, add a targeted
  test and a row to "Problems the tests found" in
  `docs/06-implementation-plan.md`.
- **Say what was not verified.** In commits, PRs and replies, separate
  what was run from what was only written.
- Work on a branch, not `main`. Commit messages say what changed and why.

## Known limits

- The reference server cannot move records between scopes; the engine
  handles moves (tested with scripted responses).
- Blob values cannot sync; use `json_columns` for structured data and keep
  files out of sync.
- One SQLite connection behind a mutex; no read pool yet.
- First-download speed has been measured only on a desktop: about 1.2 s
  for 20,000 records with an in-process server.
