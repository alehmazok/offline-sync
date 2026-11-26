import 'dart:async';

import 'rust/api/engine.dart';

int _nextSubscription = 1;

/// The engine's streams. Use these rather than the `subscribe*` methods.
///
/// Each stream registers with the engine when it is listened to and
/// unregisters when the subscription is cancelled, so `cancel()` completes
/// promptly. (A bare bridge stream only finishes cancelling when Rust next
/// sends to it or drops it.)
extension SyncEngineStreams on SyncEngine {
  /// Emits the query result now, and again whenever a table the query
  /// reads changes.
  Stream<FfiRows> watch({required String sql, required List<FfiValue> params}) =>
      _managed(this, (id) => subscribeWatch(subscription: id, sql: sql, params: params));

  /// Sync status: phase, unsent changes, last sync, last error.
  Stream<FfiStatus> statusStream() => _managed(this, (id) => subscribeStatus(subscription: id));

  /// Things to show the user, such as `change_rejected` and
  /// `upgrade_required`.
  Stream<FfiEvent> eventStream() => _managed(this, (id) => subscribeEvents(subscription: id));

  /// Names of tables changed by local writes or by sync.
  Stream<List<String>> tablesChangedStream() =>
      _managed(this, (id) => subscribeTables(subscription: id));
}

Stream<T> _managed<T>(SyncEngine engine, Stream<T> Function(int id) open) {
  final id = _nextSubscription++;
  StreamSubscription<T>? inner;
  late final StreamController<T> controller;
  controller = StreamController<T>(
    onListen: () {
      inner = open(id).listen(
        controller.add,
        onError: controller.addError,
        onDone: controller.close,
      );
    },
    onPause: () => inner?.pause(),
    onResume: () => inner?.resume(),
    onCancel: () async {
      // Unregistering drops the Rust sink, which ends the bridge stream, so
      // the inner cancel below completes.
      await engine.unsubscribe(subscription: id);
      await inner?.cancel();
    },
  );
  return controller.stream;
}
