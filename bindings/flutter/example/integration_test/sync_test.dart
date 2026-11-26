// End-to-end test of the Flutter binding: the native library, the Dart
// API, streams and the token callback, against a real sync-refserver.
//
// Start the server first (from the repository root):
//   cargo run -p sync-refserver -- --config bindings/flutter/example/server.json --listen 0.0.0.0:8080
// then, in bindings/flutter/example:
//   flutter test integration_test -d <device> [--dart-define=SYNC_URL=...]
//
// Two engines on two database files in one process stand in for two devices.

import 'dart:async';

import 'package:flutter/foundation.dart';
import 'package:flutter_test/flutter_test.dart';
import 'package:integration_test/integration_test.dart';
import 'package:offline_sync/offline_sync.dart';
import 'package:offline_sync_example/sync_setup.dart';

// Generous, but short enough that a hang fails the test instead of the job.
const _limit = Timeout(Duration(seconds: 60));

/// Progress marker in the test log, to locate a hang.
void step(String s) => debugPrint('[step ${DateTime.now().toIso8601String().substring(11, 23)}] $s');

void main() {
  IntegrationTestWidgetsFlutterBinding.ensureInitialized();

  final url = defaultSyncUrl();
  // Fresh files per run, so reruns against a long-lived server start clean
  // on the device side.
  final run = DateTime.now().microsecondsSinceEpoch;
  var n = 0;
  Future<SyncEngine> device({String Function()? token}) =>
      openDemoEngine(baseUrl: url, dbName: 'it-$run-${n++}.sqlite', token: token);

  Future<List<Note>> notes(SyncEngine e) async =>
      Note.fromRows(await e.query(sql: notesQuery, params: []));

  Future<Note?> noteById(SyncEngine e, String id) async =>
      (await notes(e)).where((x) => x.id == id).firstOrNull;

  testWidgets('a note written on one device reaches the other', (_) async {
    final a = await device();
    final b = await device();

    final title = 'note $run';
    step('t1 add');
    await addNote(a, title);
    expect((await a.status()).pendingChanges, 1);

    step('t1 a.sync');
    final report = await a.syncNow();
    expect(report.pushed, 1);
    expect((await a.status()).pendingChanges, 0);

    // B's watch emits the current result, then again after the download.
    final seen = Completer<void>();
    step('t1 b.watch');
    final sub = b.watch(sql: notesQuery, params: []).listen((rows) {
      step('t1 watch rows ${rows.rows.length}');
      if (Note.fromRows(rows).any((x) => x.title == title) && !seen.isCompleted) {
        seen.complete();
      }
    });
    step('t1 b.sync');
    await b.syncNow();
    step('t1 wait seen');
    await seen.future.timeout(const Duration(seconds: 10));
    step('t1 cancel');
    await sub.cancel().timeout(const Duration(seconds: 5));
    step('t1 done');
  }, timeout: _limit);

  testWidgets('offline edits to different fields merge', (_) async {
    final a = await device();
    final b = await device();

    await addNote(a, 'milk $run');
    await a.syncNow();
    await b.syncNow();
    final id = (await notes(a)).firstWhere((x) => x.title == 'milk $run').id;

    // Neither syncs while editing.
    await renameNote(a, id, 'oat milk $run');
    await setDone(b, id, true);
    for (var i = 0; i < 2; i++) {
      await a.syncNow();
      await b.syncNow();
    }

    for (final e in [a, b]) {
      final note = await noteById(e, id);
      expect(note?.title, 'oat milk $run');
      expect(note?.done, isTrue);
      expect((await e.status()).pendingChanges, 0);
    }

    await deleteNote(b, id);
    await b.syncNow();
    await a.syncNow();
    expect(await noteById(a, id), isNull);
  }, timeout: _limit);

  testWidgets('status streams from Rust during a sync', (_) async {
    final e = await device();
    final phases = <String>[];
    step('t3a listen');
    final sub = e.statusStream().listen((s) {
      step('t3a status ${s.phase}');
      phases.add(s.phase);
    });
    step('t3a sync');
    await e.syncNow();
    step('t3a delay');
    await Future<void>.delayed(const Duration(milliseconds: 200));
    expect(phases, contains('idle'));
    step('t3a cancel');
    await sub.cancel().timeout(const Duration(seconds: 5));
    step('t3a done');
  }, timeout: _limit);

  testWidgets('events stream from Rust during a sync', (_) async {
    final e = await device();
    final events = <String>[];
    step('t3b listen');
    final sub = e.eventStream().listen((ev) {
      step('t3b event ${ev.kind}');
      events.add(ev.kind);
    });
    step('t3b sync');
    await e.syncNow();
    step('t3b delay');
    await Future<void>.delayed(const Duration(milliseconds: 200));
    expect(events, contains('scope_added'));
    step('t3b cancel');
    await sub.cancel().timeout(const Duration(seconds: 5));
    step('t3b done');
  }, timeout: _limit);

  testWidgets('the token callback is called and a signed-out user is not synced', (_) async {
    var signedIn = true;
    var calls = 0;
    final e = await device(token: () {
      calls++;
      return signedIn ? demoToken : '';
    });
    await e.syncNow();
    expect(calls, greaterThan(0));

    signedIn = false;
    await addNote(e, 'while signed out $run');
    await expectLater(
      e.syncNow(),
      throwsA(isA<FfiError>().having((err) => err.code, 'code', 'unauthorized')),
    );
    expect((await e.status()).pendingChanges, 1);

    signedIn = true;
    await e.syncNow();
    expect((await e.status()).pendingChanges, 0);
  }, timeout: _limit);

  testWidgets('cancelling a watch completes and stops its updates', (_) async {
    final e = await device();
    var updates = 0;
    final first = Completer<void>();
    final sub = e.watch(sql: notesQuery, params: []).listen((_) {
      updates++;
      if (!first.isCompleted) first.complete();
    });
    await first.future.timeout(const Duration(seconds: 5));
    await sub.cancel().timeout(const Duration(seconds: 5));
    final before = updates;
    await addNote(e, 'after cancel $run');
    await Future<void>.delayed(const Duration(milliseconds: 300));
    expect(updates, before);
    expect((await notes(e)).any((x) => x.title == 'after cancel $run'), isTrue);
  }, timeout: _limit);

  testWidgets('a stream cancelled before it emits is dropped', (_) async {
    final e = await device();
    // Cancel straight away: unsubscribe can reach Rust before subscribe.
    for (var i = 0; i < 20; i++) {
      final sub = e.statusStream().listen((_) {});
      await sub.cancel().timeout(const Duration(seconds: 5));
    }
    // Still usable afterwards.
    await e.syncNow();
    expect((await e.status()).phase, 'idle');
  }, timeout: _limit);

  testWidgets('SQL errors arrive as FfiError', (_) async {
    final e = await device();
    await expectLater(
      e.execute(sql: 'INSERT INTO no_such_table VALUES (1)', params: []),
      throwsA(isA<FfiError>()),
    );
  }, timeout: _limit);
}
