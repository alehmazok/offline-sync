# offline-sync

An offline-first data sync system: one client engine written in Rust, one
HTTP/JSON protocol, and a thin server layer that any backend can implement.

Apps read and write a local SQLite database and keep working with no network.
The engine records every local change and exchanges changes with the server
when a connection is available.

**Status:** design phase. This repository currently contains the design, the
protocol specification and the implementation plan. No engine code yet.

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

## Planned repository layout

```
docs/                 design and specification (this is what exists today)
spec/                 OpenAPI description and example files
crates/
  sync-core/          pure logic, no I/O: protocol types, merge, outbox rules
  sync-store/         SQLite storage, trigger generation, migrations
  sync-net/           HTTP transport, retry and backoff
  sync-engine/        orchestration and the public Rust API
  sync-refserver/     small reference server used in tests
  sync-conformance/   test suite that runs against any server URL
bindings/
  flutter/            Dart package built with flutter_rust_bridge
  uniffi/             Swift and Kotlin bindings (later)
```
