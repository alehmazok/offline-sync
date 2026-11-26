#!/usr/bin/env bash
# Creates the example's android/ and ios/ folders with `flutter create`, then
# lets the app reach a reference server over plain HTTP on the local network.
# The platform folders are generated rather than committed so they always
# match the installed Flutter version. Safe to run again.
set -euo pipefail
cd "$(dirname "$0")/.."

flutter create --platforms=android,ios --project-name offline_sync_example --org dev.offlinesync .
# `flutter create` adds a sample widget test for the counter app.
rm -f test/widget_test.dart
rmdir test 2>/dev/null || true

# Android: network access in release builds and cleartext HTTP (the
# reference server has no TLS).
/usr/bin/env python3 - android/app/src/main/AndroidManifest.xml <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
if "android.permission.INTERNET" not in s:
    s = s.replace("<application", '<uses-permission android:name="android.permission.INTERNET"/>\n    <application', 1)
if "usesCleartextTraffic" not in s:
    s = s.replace("<application", '<application android:usesCleartextTraffic="true"', 1)
open(p, "w").write(s)
PY

# iOS: allow plain HTTP and explain the local-network prompt.
plist=ios/Runner/Info.plist
if ! grep -q 'NSAppTransportSecurity' "$plist"; then
  /usr/bin/env python3 - "$plist" <<'PY'
import sys
p = sys.argv[1]
s = open(p).read()
extra = """	<key>NSAppTransportSecurity</key>
	<dict>
		<key>NSAllowsArbitraryLoads</key>
		<true/>
	</dict>
	<key>NSLocalNetworkUsageDescription</key>
	<string>Syncs notes with the demo server on your local network.</string>
"""
i = s.rindex("</dict>")
open(p, "w").write(s[:i] + extra + s[i:])
PY
fi

echo "Platform folders ready."
