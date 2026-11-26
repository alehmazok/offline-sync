import 'dart:async';

// Flutter 3.47 moved Material to package:material_ui; the example keeps the
// old import so it still builds on Flutter 3.35, the package's minimum.
// ignore: deprecated_member_use
import 'package:flutter/material.dart';
import 'package:offline_sync/offline_sync.dart';

import 'sync_setup.dart';

/// Set by CI (`--dart-define=SMOKE=true`): on start, write a note and sync
/// it, so CI can find the note on the server.
const _smoke = bool.fromEnvironment('SMOKE');

void main() {
  WidgetsFlutterBinding.ensureInitialized();
  runApp(const DemoApp());
}

class DemoApp extends StatelessWidget {
  const DemoApp({super.key});

  @override
  Widget build(BuildContext context) => MaterialApp(
        title: 'offline_sync demo',
        theme: ThemeData(colorSchemeSeed: Colors.teal, useMaterial3: true),
        home: const NotesPage(),
      );
}

class NotesPage extends StatefulWidget {
  const NotesPage({super.key});

  @override
  State<NotesPage> createState() => _NotesPageState();
}

class _NotesPageState extends State<NotesPage> {
  final _url = defaultSyncUrl();
  final _input = TextEditingController();
  final _subs = <StreamSubscription<Object?>>[];

  SyncEngine? _engine;
  SyncScheduler? _scheduler;
  Object? _openError;
  List<Note> _notes = [];
  FfiStatus? _status;
  String _deviceId = '';

  /// When set, the app does not sync: a stand-in for airplane mode on
  /// simulators.
  bool _offline = false;

  @override
  void initState() {
    super.initState();
    _open();
  }

  Future<void> _open() async {
    try {
      final e = await openDemoEngine(baseUrl: _url);
      _subs.add(e
          .watch(sql: notesQuery, params: [])
          .listen((rows) => setState(() => _notes = Note.fromRows(rows))));
      _subs.add(e.statusStream().listen((s) {
        // CI looks for this line to confirm a sync worked on the device.
        if (s.phase == 'idle' && s.lastSyncAt != null && s.lastError == null) {
          debugPrint('offline_sync: synced, ${s.pendingChanges} unsent');
        }
        setState(() => _status = s);
      }));
      _subs.add(e.eventStream().listen(_onEvent));
      final status = await e.status();
      final deviceId = await e.deviceId();
      debugPrint('offline_sync: engine open, device $deviceId, server $_url');
      final scheduler = SyncScheduler(e, writeDebounce: const Duration(milliseconds: 500));
      setState(() {
        _engine = e;
        _scheduler = scheduler;
        _status = status;
        _deviceId = deviceId;
      });
      scheduler.start();
      if (_smoke) {
        await addNote(e, 'smoke $deviceId');
        await e.syncNow();
      }
    } catch (err) {
      setState(() => _openError = err);
    }
  }

  void _onEvent(FfiEvent e) {
    if (!mounted) return;
    final text = switch (e.kind) {
      'change_rejected' => 'Server rejected a change: ${e.message ?? e.code}',
      'upgrade_required' => 'Server needs a newer app version',
      _ => null,
    };
    if (text != null) {
      ScaffoldMessenger.of(context).showSnackBar(SnackBar(content: Text(text)));
    }
  }

  @override
  void dispose() {
    for (final s in _subs) {
      s.cancel();
    }
    _scheduler?.stop();
    _input.dispose();
    super.dispose();
  }

  void _afterWrite() {
    if (!_offline) _scheduler?.notifyLocalWrite();
  }

  Future<void> _add() async {
    final title = _input.text.trim();
    final e = _engine;
    if (title.isEmpty || e == null) return;
    _input.clear();
    await addNote(e, title);
    _afterWrite();
  }

  Future<void> _rename(Note n) async {
    final controller = TextEditingController(text: n.title);
    final title = await showDialog<String>(
      context: context,
      builder: (context) => AlertDialog(
        title: const Text('Rename'),
        content: TextField(controller: controller, autofocus: true),
        actions: [
          TextButton(
              onPressed: () => Navigator.pop(context), child: const Text('Cancel')),
          FilledButton(
              onPressed: () => Navigator.pop(context, controller.text.trim()),
              child: const Text('Save')),
        ],
      ),
    );
    controller.dispose();
    if (title == null || title.isEmpty || _engine == null) return;
    await renameNote(_engine!, n.id, title);
    _afterWrite();
  }

  void _setOffline(bool offline) {
    setState(() => _offline = offline);
    if (offline) {
      _scheduler?.stop();
    } else {
      _scheduler?.start();
    }
  }

  @override
  Widget build(BuildContext context) {
    final e = _engine;
    return Scaffold(
      appBar: AppBar(
        title: const Text('Notes'),
        actions: [
          Row(children: [
            const Text('Offline'),
            Switch(value: _offline, onChanged: e == null ? null : _setOffline),
          ]),
          IconButton(
            tooltip: 'Sync now',
            icon: const Icon(Icons.sync),
            onPressed: e == null || _offline ? null : () => _scheduler?.syncSoon(),
          ),
        ],
      ),
      body: _openError != null
          ? Padding(
              padding: const EdgeInsets.all(16),
              child: Text('Could not open the engine:\n$_openError'),
            )
          : e == null
              ? const Center(child: CircularProgressIndicator())
              : Column(children: [
                  _StatusBar(status: _status, url: _url, deviceId: _deviceId, offline: _offline),
                  Padding(
                    padding: const EdgeInsets.fromLTRB(16, 8, 8, 8),
                    child: Row(children: [
                      Expanded(
                        child: TextField(
                          key: const Key('new-note'),
                          controller: _input,
                          decoration: const InputDecoration(hintText: 'New note'),
                          onSubmitted: (_) => _add(),
                        ),
                      ),
                      IconButton(onPressed: _add, icon: const Icon(Icons.add)),
                    ]),
                  ),
                  Expanded(
                    child: ListView(children: [
                      for (final n in _notes)
                        Dismissible(
                          key: ValueKey(n.id),
                          onDismissed: (_) async {
                            await deleteNote(e, n.id);
                            _afterWrite();
                          },
                          child: CheckboxListTile(
                            value: n.done,
                            title: Text(n.title),
                            secondary: IconButton(
                              icon: const Icon(Icons.edit),
                              onPressed: () => _rename(n),
                            ),
                            onChanged: (v) async {
                              await setDone(e, n.id, v ?? false);
                              _afterWrite();
                            },
                          ),
                        ),
                    ]),
                  ),
                ]),
    );
  }
}

class _StatusBar extends StatelessWidget {
  const _StatusBar({
    required this.status,
    required this.url,
    required this.deviceId,
    required this.offline,
  });

  final FfiStatus? status;
  final String url;
  final String deviceId;
  final bool offline;

  @override
  Widget build(BuildContext context) {
    final s = status;
    final last = s?.lastSyncAt == null
        ? 'never'
        : DateTime.fromMillisecondsSinceEpoch(s!.lastSyncAt! * 1000)
            .toLocal()
            .toString()
            .substring(11, 19);
    final short = deviceId.length > 8 ? deviceId.substring(deviceId.length - 8) : deviceId;
    return Container(
      width: double.infinity,
      color: Theme.of(context).colorScheme.surfaceContainerHighest,
      padding: const EdgeInsets.symmetric(horizontal: 16, vertical: 8),
      child: DefaultTextStyle(
        style: Theme.of(context).textTheme.bodySmall!,
        child: Column(crossAxisAlignment: CrossAxisAlignment.start, children: [
          Text(offline
              ? 'offline (sync paused) · ${s?.pendingChanges ?? 0} unsent'
              : '${s?.phase ?? '?'} · ${s?.pendingChanges ?? 0} unsent · last sync $last'),
          if (s?.lastError != null)
            Text(s!.lastError!, style: TextStyle(color: Theme.of(context).colorScheme.error)),
          Text('$url · device …$short'),
        ]),
      ),
    );
  }
}
