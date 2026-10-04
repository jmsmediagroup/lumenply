#!/bin/sh
# Wraps Lumenply.app in a compressed disk image with an Applications
# shortcut, the usual "drag it to Applications" window.
# Usage: scripts/make-dmg.sh path/to/Lumenply.app output.dmg
set -eu
APP=$1
DMG=$2
STAGE=$(mktemp -d)/Lumenply
mkdir -p "$STAGE"
cp -R "$APP" "$STAGE/"
ln -s /Applications "$STAGE/Applications"
rm -f "$DMG"
hdiutil create -volname Lumenply -srcfolder "$STAGE" -fs HFS+ -format UDZO -imagekey zlib-level=9 "$DMG" >/dev/null
echo "built $DMG ($(du -h "$DMG" | cut -f1))"
