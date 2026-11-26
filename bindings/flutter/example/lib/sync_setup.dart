import 'dart:io';

import 'package:flutter/services.dart';
import 'package:offline_sync/offline_sync.dart';
import 'package:path_provider/path_provider.dart';

/// The demo user in `example/server.json`.
const demoToken = 'demo';
const demoScope = 'team:demo';

/// Where the reference server is. Pass
/// `--dart-define=SYNC_URL=http://<laptop-ip>:8080/sync/v1` for a real phone.
/// The Android emulator reaches the host as 10.0.2.2; the iOS simulator
/// shares the host's network.
String defaultSyncUrl() {
  const fromEnv = String.fromEnvironment('SYNC_URL');
  if (fromEnv.isNotEmpty) return fromEnv;
  return Platform.isAndroid
      ? 'http://10.0.2.2:8080/sync/v1'
      : 'http://127.0.0.1:8080/sync/v1';
}

bool _rustReady = false;

/// Load the native library once per process.
Future<void> initRust() async {
  if (_rustReady) return;
  await RustLib.init();
  _rustReady = true;
}

/// Open an engine on its own database file. Two engines with different
/// [dbName]s in one process behave like two devices.
Future<SyncEngine> openDemoEngine({
  required String baseUrl,
  String dbName = 'notes.sqlite',
  String Function()? token,
}) async {
  await initRust();
  final dir = await getApplicationSupportDirectory();
  return SyncEngine.open(
    config: FfiConfig(
      dbPath: '${dir.path}/$dbName',
      manifestJson: await rootBundle.loadString('assets/sync_manifest.json'),
      migrationsJson: await rootBundle.loadString('assets/migrations.json'),
      defaultScope: demoScope,
      baseUrl: baseUrl,
    ),
    tokenProvider: (forceRefresh) => token?.call() ?? demoToken,
  );
}

/// Read a column value from a query result.
Object? valueOf(FfiValue v) => switch (v) {
      FfiValue_Null() => null,
      FfiValue_Integer(:final field0) => field0,
      FfiValue_Real(:final field0) => field0,
      FfiValue_Text(:final field0) => field0,
      FfiValue_Blob(:final field0) => field0,
    };

class Note {
  Note(this.id, this.title, this.done);

  final String id;
  final String title;
  final bool done;

  static List<Note> fromRows(FfiRows r) => [
        for (final row in r.rows)
          Note(
            valueOf(row[0]) as String,
            valueOf(row[1]) as String,
            (valueOf(row[2]) as int) != 0,
          ),
      ];
}

const notesQuery = 'SELECT id, title, done FROM notes ORDER BY title, id';

Future<void> addNote(SyncEngine e, String title) => e.execute(
      sql: 'INSERT INTO notes (id, title) VALUES (?1, ?2)',
      params: [FfiValue.text(e.newId()), FfiValue.text(title)],
    );

Future<void> setDone(SyncEngine e, String id, bool done) => e.execute(
      sql: 'UPDATE notes SET done = ?2 WHERE id = ?1',
      params: [FfiValue.text(id), FfiValue.integer(done ? 1 : 0)],
    );

Future<void> renameNote(SyncEngine e, String id, String title) => e.execute(
      sql: 'UPDATE notes SET title = ?2 WHERE id = ?1',
      params: [FfiValue.text(id), FfiValue.text(title)],
    );

Future<void> deleteNote(SyncEngine e, String id) => e.execute(
      sql: 'DELETE FROM notes WHERE id = ?1',
      params: [FfiValue.text(id)],
    );
