#!/bin/sh
# Build Tabpak.app into dist/. Pass a folder (for example /Applications) to copy it there too.
set -eu
cd "$(dirname "$0")/.."
cargo build --release
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)
app=dist/Tabpak.app
rm -rf "$app" dist/icon.iconset
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources" dist/icon.iconset
cp target/release/tabpak "$app/Contents/MacOS/tabpak"
for size in 16 32 128 256 512; do
  sips -z "$size" "$size" assets/icon.png --out "dist/icon.iconset/icon_${size}x${size}.png" >/dev/null
  sips -z "$((size * 2))" "$((size * 2))" assets/icon.png --out "dist/icon.iconset/icon_${size}x${size}@2x.png" >/dev/null
done
iconutil -c icns dist/icon.iconset -o "$app/Contents/Resources/Tabpak.icns"
rm -rf dist/icon.iconset
cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Tabpak</string>
  <key>CFBundleDisplayName</key><string>Tabpak</string>
  <key>CFBundleIdentifier</key><string>app.tabpak.Tabpak</string>
  <key>CFBundleExecutable</key><string>tabpak</string>
  <key>CFBundleIconFile</key><string>Tabpak</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>CFBundleShortVersionString</key><string>$version</string>
  <key>CFBundleVersion</key><string>$version</string>
  <key>LSMinimumSystemVersion</key><string>12.0</string>
  <key>LSApplicationCategoryType</key><string>public.app-category.music</string>
  <key>NSHighResolutionCapable</key><true/>
</dict>
</plist>
PLIST
codesign --force --sign - "$app" >/dev/null 2>&1
echo "$app"
if [ "${1:-}" != "" ]; then
  rm -rf "$1/Tabpak.app"
  cp -R "$app" "$1/Tabpak.app"
  echo "$1/Tabpak.app"
fi
