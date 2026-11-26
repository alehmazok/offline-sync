/// Offline-first data sync for Flutter.
///
/// ```dart
/// await RustLib.init();
/// final engine = await SyncEngine.open(
///   config: FfiConfig(
///     dbPath: '${dir.path}/app.sqlite',
///     manifestJson: manifest,
///     migrationsJson: migrations,
///     defaultScope: 'user:$userId',
///     baseUrl: 'https://api.example.com/sync/v1',
///   ),
///   tokenProvider: (forceRefresh) async => auth.token(refresh: forceRefresh),
/// );
/// final scheduler = SyncScheduler(engine)..start();
/// ```
library offline_sync;

export 'src/rust/api/engine.dart';
export 'src/rust/frb_generated.dart' show RustLib;
export 'src/scheduler.dart';
export 'src/streams.dart';
