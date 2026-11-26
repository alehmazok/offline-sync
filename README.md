# offline-sync

An offline-first data sync system: one client engine written in Rust, one
HTTP/JSON protocol, and a thin server layer that any backend can implement.

Apps read and write a local SQLite database and keep working with no network.
The engine records every local change and exchanges changes with the server
when a connection is available.

**Status:** engine implemented and tested. The protocol, reference
server, conformance suite, local store, sync engine and a binding facade
are built; all tests pass, including a long randomised convergence test.
The Flutter package is written but not yet generated or run on a device.
No real backend or app uses it yet. See
[implementation progress](docs/06-implementation-plan.md#66-progress).

## Quick start

```sh
cargo test --workspace                       # everything except the long runs
cargo test -p sync-engine --release --test convergence -- --ignored   # long convergence run

# Run the reference server and test it with the conformance suite.
cargo run -p sync-refserver -- --listen 127.0.0.1:8080
cargo run -p sync-conformance -- --base-url http://127.0.0.1:8080/sync/v1 \
  --token-a token-a --token-b token-b --scope-1 team:conf1 --scope-2 team:conf2 \
  --test-url http://127.0.0.1:8080/_test
```

## Why it is built this way

- **Several apps, one engine.** The engine knows nothing about any app's
  tables. Each app describes its data in a small manifest.
- **Several backends, one protocol.** Servers differ in language and database
  (PostgreSQL, MySQL, MongoDB). They share a protocol, not code, and prove
  they implement it by passing one conformance test suite.
- **Flutter today, native later.** The engine is a plain Rust library. A thin
  binding exposes it to Flutter now; Swift and Kotlin bindings can be added
  later without touching the engine.
- **Smart client, simple server.** Retry, ordering, batching, conflict
  handling and crash recovery live in the client, where they are written
  once. A server only has to do four small things correctly.

## Documentation

Read in this order:

| # | Document | What it covers |
| --- | --- | --- |
| 1 | [Architecture](docs/01-architecture.md) | Components, principles, key decisions, glossary |
| 2 | [Protocol specification](docs/02-protocol.md) | Endpoints, message shapes, rules both sides must follow |
| 3 | [Server implementation guide](docs/03-server-guide.md) | Data model, algorithms, notes per database |
| 4 | [Client engine design](docs/04-client-engine.md) | Manifest, local storage, sync cycle, public API |
| 5 | [Conformance tests](docs/05-conformance-tests.md) | The test cases every server must pass |
| 6 | [Implementation plan](docs/06-implementation-plan.md) | Repository layout, phases, risks |

The machine-readable version of the protocol is
[`spec/openapi.yaml`](spec/openapi.yaml). If it disagrees with
`docs/02-protocol.md`, the Markdown document wins and the YAML has a bug.

## Repository layout

```
docs/                 design and specification
spec/                 OpenAPI description of the protocol
crates/
  sync-core/          protocol types, batch merging; no I/O
  sync-store/         SQLite storage, change-capture triggers, migrations
  sync-net/           HTTP transport, retry, token refresh
  sync-engine/        the sync cycle and the public Rust API
  sync-ffi/           binding-friendly facade (Flutter now, Swift/Kotlin later)
  sync-refserver/     reference server used in tests
  sync-conformance/   protocol test suite that runs against any server URL
bindings/
  flutter/            Dart package (flutter_rust_bridge); not yet generated
```
