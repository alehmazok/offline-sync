import 'dart:async';

import 'package:flutter/widgets.dart';

import 'rust/api/engine.dart';

/// Decides when to sync. The engine never schedules itself
/// (docs/04-client-engine.md, section 4.11), so the app owns this.
///
/// It syncs:
/// - when the app comes to the foreground, and periodically while it is;
/// - a short while after local writes ([notifyLocalWrite]);
/// - when the app reports connectivity is back ([notifyConnectivityRestored]);
/// - on a push notification ([notifyRemoteChange]).
///
/// Background sync (WorkManager on Android, BGTaskScheduler on iOS) is set
/// up by the app; from that task, call [SyncEngine.syncNow] directly.
class SyncScheduler with WidgetsBindingObserver {
  SyncScheduler(
    this.engine, {
    this.writeDebounce = const Duration(seconds: 2),
    this.foregroundInterval = const Duration(minutes: 5),
    this.onError,
  });

  final SyncEngine engine;
  final Duration writeDebounce;
  final Duration foregroundInterval;

  /// Called with any error from a sync cycle. Errors are otherwise
  /// swallowed: being offline is normal.
  final void Function(Object error)? onError;

  Timer? _debounce;
  Timer? _periodic;
  bool _running = false;
  bool _again = false;

  void start() {
    WidgetsBinding.instance.addObserver(this);
    _startPeriodic();
    syncSoon();
  }

  void stop() {
    WidgetsBinding.instance.removeObserver(this);
    _debounce?.cancel();
    _periodic?.cancel();
  }

  void notifyLocalWrite() {
    _debounce?.cancel();
    _debounce = Timer(writeDebounce, syncSoon);
  }

  void notifyConnectivityRestored() => syncSoon();

  void notifyRemoteChange() => syncSoon();

  @override
  void didChangeAppLifecycleState(AppLifecycleState state) {
    switch (state) {
      case AppLifecycleState.resumed:
        _startPeriodic();
        syncSoon();
      case AppLifecycleState.paused:
      case AppLifecycleState.detached:
        _periodic?.cancel();
      default:
        break;
    }
  }

  /// Run a sync cycle now, or right after the one in progress.
  Future<void> syncSoon() async {
    if (_running) {
      _again = true;
      return;
    }
    _running = true;
    try {
      do {
        _again = false;
        try {
          await engine.syncNow();
        } catch (e) {
          onError?.call(e);
        }
      } while (_again);
    } finally {
      _running = false;
    }
  }

  void _startPeriodic() {
    _periodic?.cancel();
    _periodic = Timer.periodic(foregroundInterval, (_) => syncSoon());
  }
}
