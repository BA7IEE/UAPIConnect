#!/usr/bin/env bash
set -euo pipefail

VERSION="${1:-0.0.0}"
ARCH="${2:-$(uname -m)}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
DIST="$ROOT/dist/uapi/macos"
STAGE="$DIST/stage"
BINARY_DIR="${BINARY_DIR:-$ROOT/target/release}"
DMG="$DIST/UAPIConnect-${VERSION}-macos-${ARCH}.dmg"
ICON_SOURCE="$ROOT/apps/codex-plus-manager/src-tauri/icons/icon.png"
ICON_NAME="uapi-connect.icns"
ICON_ICNS="$DIST/$ICON_NAME"

rm -rf "$DIST"
mkdir -p "$STAGE"

prepare_icon() {
  local iconset="$DIST/uapi-connect.iconset"
  mkdir -p "$iconset"
  sips -z 16 16 "$ICON_SOURCE" --out "$iconset/icon_16x16.png" >/dev/null
  sips -z 32 32 "$ICON_SOURCE" --out "$iconset/icon_16x16@2x.png" >/dev/null
  sips -z 32 32 "$ICON_SOURCE" --out "$iconset/icon_32x32.png" >/dev/null
  sips -z 64 64 "$ICON_SOURCE" --out "$iconset/icon_32x32@2x.png" >/dev/null
  sips -z 128 128 "$ICON_SOURCE" --out "$iconset/icon_128x128.png" >/dev/null
  sips -z 256 256 "$ICON_SOURCE" --out "$iconset/icon_128x128@2x.png" >/dev/null
  sips -z 256 256 "$ICON_SOURCE" --out "$iconset/icon_256x256.png" >/dev/null
  sips -z 512 512 "$ICON_SOURCE" --out "$iconset/icon_256x256@2x.png" >/dev/null
  sips -z 512 512 "$ICON_SOURCE" --out "$iconset/icon_512x512.png" >/dev/null
  sips -z 1024 1024 "$ICON_SOURCE" --out "$iconset/icon_512x512@2x.png" >/dev/null
  iconutil -c icns "$iconset" -o "$ICON_ICNS"
}

create_app() {
  local app_name="$1"
  local executable_name="$2"
  local binary_path="$3"
  local bundle_id="$4"
  local lsui_element="$5"
  local manager="$6"
  local app_dir="$STAGE/$app_name.app"
  local url_types=""

  test -x "$binary_path" || { echo "missing binary: $binary_path" >&2; exit 1; }
  mkdir -p "$app_dir/Contents/MacOS" "$app_dir/Contents/Resources"
  cp "$binary_path" "$app_dir/Contents/MacOS/$executable_name"
  cp "$ICON_ICNS" "$app_dir/Contents/Resources/$ICON_NAME"
  chmod +x "$app_dir/Contents/MacOS/$executable_name"
  printf 'APPL????' > "$app_dir/Contents/PkgInfo"

  if [ "$manager" = "true" ]; then
    url_types='  <key>CFBundleURLTypes</key>
  <array>
    <dict>
      <key>CFBundleURLName</key>
      <string>U-API Connect Links</string>
      <key>CFBundleURLSchemes</key>
      <array><string>uapiconnect</string></array>
    </dict>
  </array>'
  fi

  cat > "$app_dir/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
  <key>CFBundleName</key><string>$app_name</string>
  <key>CFBundleDisplayName</key><string>$app_name</string>
  <key>CFBundleIdentifier</key><string>$bundle_id</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleExecutable</key><string>$executable_name</string>
  <key>CFBundleIconFile</key><string>$ICON_NAME</string>
$url_types
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSUIElement</key><$lsui_element/>
</dict></plist>
PLIST
  codesign --force --sign - "$app_dir/Contents/MacOS/$executable_name"
  codesign --force --sign - "$app_dir"
  plutil -lint "$app_dir/Contents/Info.plist" >/dev/null
  codesign --verify --deep --strict "$app_dir"
}

prepare_icon
create_app "U-API Connect" "CodexPlusPlus" "$BINARY_DIR/codex-plus-plus" "cn.u-studio.uapi.connect" "true" "false"
create_app "U-API Connect 设置" "CodexPlusPlusManager" "$BINARY_DIR/codex-plus-plus-manager" "cn.u-studio.uapi.connect.manager" "false" "true"
ln -s /Applications "$STAGE/Applications"
test "$(readlink "$STAGE/Applications")" = "/Applications"

DMG_WORK_DIR="$(mktemp -d "${TMPDIR:-/tmp}/uapi-connect-dmg.XXXXXX")"
DMG_WORK_PATH="$DMG_WORK_DIR/$(basename "$DMG")"
trap 'rm -rf "$DMG_WORK_DIR"' EXIT

created=false
for attempt in 1 2 3; do
  if hdiutil create -volname "U-API Connect" -srcfolder "$STAGE" -ov -format UDZO "$DMG_WORK_PATH"; then
    mv "$DMG_WORK_PATH" "$DMG"
    created=true
    break
  fi
  [ "$attempt" -eq 3 ] || sleep "$((attempt * 2))"
done

[ "$created" = true ] || { echo "failed to create DMG" >&2; exit 1; }
echo "$DMG"
