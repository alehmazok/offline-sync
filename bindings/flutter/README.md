# offline_sync for Flutter

Dart package around the Rust engine, built with
[flutter_rust_bridge](https://github.com/fzyzcjy/flutter_rust_bridge) 2.13.
It is a Flutter FFI plugin for Android and iOS: an app adds it as a
dependency and the native library is built by cargokit as part of the
app's build.

Needs Flutter 3.35 or newer (Dart 3.9.2, required by flutter_rust_bridge
2.13) and a Rust toolchain from rustup.

> **Status.** The bindings are generated and committed. CI builds the
> example app for Android and the iOS simulator and runs the end-to-end
> test (`example/integration_test`) on an Android emulator and an iOS
> simulator against `sync-refserver`. It has not yet been run on physical
> phones. See `docs/06-implementation-plan.md`, section 6.6.

## Layout

```
rust/                   adapter crate: Dart callbacks and streams on top of sync-ffi
  src/api/engine.rs     the Dart-facing API (input to the code generator)
  src/adapter.rs        token callback, stream sinks, stream registrations
  src/frb_generated.rs  generated
  tests/                runs the example's sync setup over HTTP, no device needed
lib/offline_sync.dart   public entry point
lib/src/scheduler.dart  when to sync: foreground, after writes, on connectivity
lib/src/streams.dart    watch() and the other streams, with prompt cancel
lib/src/rust/           generated Dart (do not edit)
android/ ios/ cargokit/ native build setup (from the bridge's plugin template)
example/                two-device demo app and the end-to-end test
```

All logic lives in `crates/sync-engine`. `crates/sync-ffi` is the
binding-friendly surface that both this package and a future Swift/Kotlin
binding wrap.

## Use from an app

```yaml
dependencies:
  offline_sync:
    git:
      url: https://github.com/alehmazok/offline-sync
      path: bindings/flutter
```

The first build compiles the Rust engine for each target and takes a few
minutes; later builds are incremental. rustup must be installed; cargokit
adds the Android and iOS targets it needs.

```dart
import 'package:flutter/services.dart';
import 'package:offline_sync/offline_sync.dart';
import 'package:path_provider/path_provider.dart';

Future<SyncEngine> openSync(Auth auth, String userId) async {
  await RustLib.init(); // once per process
  final dir = await getApplicationSupportDirectory();
  return SyncEngine.open(
    config: FfiConfig(
      dbPath: '${dir.path}/app.sqlite',
      manifestJson: await rootBundle.loadString('assets/sync_manifest.json'),
      migrationsJson: await rootBundle.loadString('assets/migrations.json'),
      defaultScope: 'user:$userId',
      baseUrl: 'https://api.example.com/sync/v1',
    ),
    // Called before requests; `forceRefresh` is true after a 401.
    // Return '' when signed out: sync then fails with code 'unauthorized'
    // and local changes stay queued.
    tokenProvider: (forceRefresh) => auth.accessToken(refresh: forceRefresh),
  );
}

final scheduler = SyncScheduler(engine)..start();

// Writes go through the engine so they are captured for sync.
await engine.execute(
  sql: 'INSERT INTO visits (id, scope_id, note) VALUES (?1, ?2, ?3)',
  params: [FfiValue.text(engine.newId()), FfiValue.text('team:17'), FfiValue.text('Shelf restocked')],
);
scheduler.notifyLocalWrite();

// Reactive queries: emits now, then whenever a table the query reads changes.
final sub = engine.watch(sql: 'SELECT id, note FROM visits ORDER BY id', params: []).listen((rows) {
  // rows.columns, rows.rows (List<List<FfiValue>>)
});
await sub.cancel(); // unregisters the watch in the engine

// Things to show the user.
engine.eventStream().listen((e) {
  if (e.kind == 'change_rejected') { /* tell the user */ }
  if (e.kind == 'upgrade_required') { /* prompt for an app update */ }
});
```

Errors from the engine are thrown as `FfiError` with a stable `code`
(`unauthorized`, `network`, `database`, ...) and a `message`.

## Rules for app code

- Open one engine per database file and keep it for the app's lifetime.
- Do not open the same SQLite file with another package (sqflite, drift).
  Writes that bypass the engine are not synced.
- Synced tables need a text primary key named `id`; use `engine.newId()`.
- Local-only columns (not in the manifest) must be nullable or have a
  default.
- Call `syncNow()` from a background task (the `workmanager` package, or
  BGTaskScheduler on iOS) if data should sync while the app is closed.
- Cancel stream subscriptions you no longer need; cancelling unregisters
  them in the engine. Use `watch`, `statusStream`, `eventStream` and
  `tablesChangedStream`, not the lower-level `subscribe*` methods, which
  only stop when `unsubscribe` is called.

## Changing the Rust API

After editing anything under `rust/src/api/`, regenerate and commit the
result:

```sh
cargo install flutter_rust_bridge_codegen --version 2.13.0 --locked   # once
cd bindings/flutter
flutter pub get
flutter_rust_bridge_codegen generate   # also runs build_runner for the freezed classes
```

The Rust crate and the Dart package versions of flutter_rust_bridge must
match exactly (`rust/Cargo.toml` and `pubspec.yaml`). CI regenerates the
bindings on every push. On a branch other than `main` it commits any
difference itself; on `main` and in pull requests a difference fails the
build.

## Example app

See [example/README.md](example/README.md).
