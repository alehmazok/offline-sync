# offline_sync example

A notes list on two devices: add, rename, tick and swipe-delete notes with
or without a connection, and watch the devices converge through
`sync-refserver`. The "Offline" switch pauses sync, so you can try offline
edits on emulators without airplane mode.

## Run it

1. Start the reference server from the repository root. `0.0.0.0` lets
   phones on your network reach it; it has no authentication, so only do
   this on a trusted network.

   ```sh
   cargo run -p sync-refserver -- --config bindings/flutter/example/server.json --listen 0.0.0.0:8080
   ```

   Add `--db /tmp/notes-server.sqlite` to keep the server's data between
   restarts.

2. Create the platform folders (generated, not committed, so they match
   your Flutter version):

   ```sh
   cd bindings/flutter/example
   ./tool/create_platforms.sh
   ```

3. Run on two devices. The default server URL is `http://10.0.2.2:8080`
   on the Android emulator and `http://127.0.0.1:8080` on the iOS
   simulator. For a physical phone, pass your computer's address:

   ```sh
   flutter run -d <device-1>
   flutter run -d <device-2> --dart-define=SYNC_URL=http://192.168.1.20:8080/sync/v1
   ```

## Try this

- Add a note on one device; it appears on the other within a few seconds
  of a sync (after a write, on resume, every 5 minutes, or the sync button).
- Turn "Offline" on for both. Rename a note on one device and tick it on
  the other. Turn both back on: each device ends up with the new name and
  the tick, because different fields of a record merge.
- Edit the same field on both while offline: the change that reaches the
  server last wins that field.
- Stop the server, make changes, start it again: the unsent count drops to
  zero once sync succeeds.

## Files

| File | What it is |
| --- | --- |
| `assets/sync_manifest.json` | Which tables and columns sync (one table, `notes`, in the default scope) |
| `assets/migrations.json` | The local schema |
| `server.json` | Reference server config: the `notes` table and the user with token `demo` |
| `lib/sync_setup.dart` | Opening the engine, reading values, the note queries |
| `lib/main.dart` | The UI |
| `integration_test/sync_test.dart` | End-to-end test: two engines in one app against the server |

## End-to-end test

With the server running (step 1):

```sh
flutter test integration_test -d <device>
```

CI runs it on an Android emulator and an iOS simulator.
