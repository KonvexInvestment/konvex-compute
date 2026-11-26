#!/usr/bin/env bash
set -euo pipefail

ROOT="$(cd "$(dirname "$0")" && pwd)"
TAURI_DIR="$ROOT/src-tauri"
APP_NAME="Konvex Compute"
BUNDLE_ID="de.konvexinvestment.compute"
BIN_NAME="konvex-compute"
OUT_APP="$TAURI_DIR/target/release/bundle/macos/${APP_NAME}.app"

echo "==> Baue Release-Binary (Rust + Tauri 2, locked)..."
(cd "$TAURI_DIR" && cargo build --release --locked)

echo "==> Verpacke macOS .app-Bundle..."
rm -rf "$OUT_APP"
mkdir -p "$OUT_APP/Contents/MacOS" "$OUT_APP/Contents/Resources"

cp "$TAURI_DIR/target/release/$BIN_NAME" "$OUT_APP/Contents/MacOS/$BIN_NAME"
chmod +x "$OUT_APP/Contents/MacOS/$BIN_NAME"
cp "$TAURI_DIR/icons/icon.icns" "$OUT_APP/Contents/Resources/icon.icns"

cat > "$OUT_APP/Contents/Info.plist" <<EOF
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleDevelopmentRegion</key>
  <string>de</string>
  <key>CFBundleDisplayName</key>
  <string>${APP_NAME}</string>
  <key>CFBundleExecutable</key>
  <string>${BIN_NAME}</string>
  <key>CFBundleIconFile</key>
  <string>icon.icns</string>
  <key>CFBundleIdentifier</key>
  <string>${BUNDLE_ID}</string>
  <key>CFBundleInfoDictionaryVersion</key>
  <string>6.0</string>
  <key>CFBundleName</key>
  <string>${APP_NAME}</string>
  <key>CFBundlePackageType</key>
  <string>APPL</string>
  <key>CFBundleShortVersionString</key>
  <string>1.0.0</string>
  <key>CFBundleVersion</key>
  <string>1</string>
  <key>LSMinimumSystemVersion</key>
  <string>14.0</string>
  <key>NSHighResolutionCapable</key>
  <true/>
</dict>
</plist>
EOF

echo "==> Ad-hoc Code-Signatur für macOS..."
codesign --force --deep --sign - "$OUT_APP"
codesign --verify --deep --strict "$OUT_APP"

if [[ "${1:-}" == "--install" ]]; then
  DEST="/Applications/${APP_NAME}.app"
  STAGING="/Applications/.${APP_NAME}.staging.$$.app"
  BACKUP="/Applications/.${APP_NAME}.backup.$$.app"
  cleanup_on_exit() {
    if [[ ! -d "$DEST" && -d "$BACKUP" ]]; then
      mv "$BACKUP" "$DEST" || true
    fi
    rm -rf "$STAGING"
  }
  trap cleanup_on_exit EXIT

  echo "==> Kopiere sicher in Staging-Bundle..."
  cp -R "$OUT_APP" "$STAGING"
  codesign --verify --deep --strict "$STAGING"

  if [[ -d "$DEST" ]]; then
    mv "$DEST" "$BACKUP"
  fi
  if mv "$STAGING" "$DEST"; then
    rm -rf "$BACKUP"
    echo "Installiert: $DEST"
  else
    echo "Fehler beim Aktivieren, vorherige Installation wird wiederhergestellt." >&2
    exit 1
  fi
else
  echo "Gebaut: $OUT_APP"
fi
