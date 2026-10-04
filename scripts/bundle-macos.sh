#!/bin/sh
# Builds Lumenply.app (release) with its icon and file associations.
# Usage: scripts/bundle-macos.sh [output dir, default target/release]
# LUMENPLY_BIN=path uses that binary (a universal one from CI) instead of
# building. Ad-hoc signed only: without a Developer ID the first launch
# needs System Settings > Privacy & Security > Open Anyway.
set -eu
cd "$(dirname "$0")/.."
OUT=${1:-target/release}
VERSION=$(grep -m1 '^version' Cargo.toml | sed 's/.*"\(.*\)".*/\1/')
if [ -n "${LUMENPLY_BIN:-}" ]; then
  BIN=$LUMENPLY_BIN
else
  cargo build --release -p lumenply-app
  BIN=${CARGO_TARGET_DIR:-target}/release/lumenply-app
fi
APP="$OUT/Lumenply.app"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"
cp "$BIN" "$APP/Contents/MacOS/lumenply-app"

# Icon: every size the .icns wants, rendered by the app itself.
SET=$(mktemp -d)/Lumenply.iconset
mkdir -p "$SET"
for s in 16 32 128 256 512; do
  "$BIN" --write-icon "$SET/icon_${s}x${s}.png" $s
  "$BIN" --write-icon "$SET/icon_${s}x${s}@2x.png" $((s * 2))
done
iconutil -c icns "$SET" -o "$APP/Contents/Resources/Lumenply.icns"

doc_type() { # name, role, rank, extensions...
  name=$1 role=$2 rank=$3; shift 3
  printf '    <dict>\n      <key>CFBundleTypeName</key><string>%s</string>\n' "$name"
  printf '      <key>CFBundleTypeRole</key><string>%s</string>\n' "$role"
  printf '      <key>LSHandlerRank</key><string>%s</string>\n' "$rank"
  printf '      <key>CFBundleTypeIconFile</key><string>Lumenply</string>\n'
  printf '      <key>CFBundleTypeExtensions</key><array>'
  for e in "$@"; do printf '<string>%s</string>' "$e"; done
  printf '</array>\n    </dict>\n'
}

cat > "$APP/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key><string>Lumenply</string>
  <key>CFBundleDisplayName</key><string>Lumenply</string>
  <key>CFBundleIdentifier</key><string>org.lumenply.Lumenply</string>
  <key>CFBundleVersion</key><string>$VERSION</string>
  <key>CFBundleShortVersionString</key><string>$VERSION</string>
  <key>CFBundleExecutable</key><string>lumenply-app</string>
  <key>CFBundleIconFile</key><string>Lumenply</string>
  <key>CFBundlePackageType</key><string>APPL</string>
  <key>LSMinimumSystemVersion</key><string>11.0</string>
  <key>NSHighResolutionCapable</key><true/>
  <key>LSApplicationCategoryType</key><string>public.app-category.photography</string>
  <key>CFBundleDocumentTypes</key>
  <array>
$(doc_type "Lumenply project" Editor Owner lumen nge)
$(doc_type "Photoshop document" Editor Alternate psd psb)
$(doc_type "OpenRaster image" Editor Alternate ora)
$(doc_type "Image" Editor Alternate png jpg jpeg tif tiff webp exr gif bmp tga ico qoi heic heif hif avif)
$(doc_type "Camera RAW" Viewer Alternate dng cr2 cr3 crw nef nrw arw srf sr2 raf orf rw2 pef srw rwl 3fr fff iiq mos mef mrw erf kdc dcr raw)
  </array>
</dict>
</plist>
PLIST
plutil -lint "$APP/Contents/Info.plist" >/dev/null
# Apple Silicon refuses unsigned code; an ad-hoc signature over the whole
# bundle keeps it consistent after the Info.plist and icon were added.
codesign --force --deep --sign - "$APP"
echo "built $APP ($VERSION)"
