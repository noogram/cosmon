#!/usr/bin/env bash
# mac-pilot — ad-hoc rebuild + install
#
# Fallback path when the operator is not enrolled in an Apple Developer team.
# Produces an unsigned (ad-hoc signed with "-") build that runs locally but
# cannot be distributed. The team-signed path is `just install-mac-pilot`;
# see docs/guides/mac-pilot-signing-setup.md for setup.
#
# Usage:   scripts/mac-pilot-reinstall-adhoc.sh
# Exit:    0 on success, non-zero on xcodebuild / copy failure.

set -euo pipefail

REPO_ROOT="$(cd "$(dirname "$0")/.." && pwd)"
DERIVED="/tmp/mac-pilot-build-adhoc"
APP_NAME="mac-pilot.app"
INSTALL_DIR="$HOME/Applications"

cd "$REPO_ROOT"

echo "==> ad-hoc xcodebuild (no team)"
xcodebuild -project apps/mac-pilot/mac-pilot.xcodeproj \
  -scheme mac-pilot -configuration Release \
  -destination 'platform=macOS,arch=arm64' \
  -derivedDataPath "$DERIVED" \
  CODE_SIGN_IDENTITY="-" \
  CODE_SIGNING_REQUIRED=NO \
  CODE_SIGNING_ALLOWED=NO \
  -quiet \
  build

BUILT="$DERIVED/Build/Products/Release/$APP_NAME"
if [[ ! -d "$BUILT" ]]; then
    echo "error: build succeeded but $BUILT is missing" >&2
    exit 1
fi

echo "==> relaunch & install to $INSTALL_DIR"
pkill -f "/Applications/$APP_NAME/Contents/MacOS/mac-pilot" || true
mkdir -p "$INSTALL_DIR"
rm -rf "$INSTALL_DIR/$APP_NAME"
cp -R "$BUILT" "$INSTALL_DIR/"

# Strip quarantine bit — ad-hoc-signed apps can trip Gatekeeper if the
# binary was ever downloaded.
xattr -dr com.apple.quarantine "$INSTALL_DIR/$APP_NAME" 2>/dev/null || true

open "$INSTALL_DIR/$APP_NAME"
echo "==> done — $INSTALL_DIR/$APP_NAME relaunched (ad-hoc signed)"
