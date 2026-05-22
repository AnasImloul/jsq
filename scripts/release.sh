#!/usr/bin/env bash
# Build BigJSON for Release, package as a drag-to-install .dmg.
#
# Defaults to ad-hoc signing — works without an Apple Developer Program
# membership, but users will see a Gatekeeper warning the first time they
# launch the app and need to right-click → Open. Set the env vars below
# to upgrade to Developer-ID signing and/or notarization for a clean UX.
#
# Required tools (all preinstalled on macOS): xcodebuild, hdiutil, codesign.
# Optional: xcrun notarytool (for notarization, ships with Xcode).
#
# Usage:
#   scripts/release.sh                # ad-hoc signed, version=git short sha
#   scripts/release.sh 1.0.0          # tag the dmg with an explicit version
#
# Env vars (all optional):
#   APPLE_DEVELOPER_ID  "Developer ID Application: Your Name (TEAMID)"
#                       — if set, codesigns with that identity
#   NOTARIZE=1          — if set (and signed), submits to Apple notary
#   APPLE_ID            — Apple ID for notarytool
#   APPLE_TEAM_ID       — Team ID for notarytool
#   APPLE_APP_PASSWORD  — app-specific password for notarytool

set -euo pipefail

VERSION="${1:-$(git rev-parse --short HEAD 2>/dev/null || echo dev)}"
SCHEME="BigJSON"
PROJECT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
BUILD_DIR="$PROJECT_DIR/build/release"
APP_NAME="BigJSON.app"
DMG_VOL_NAME="BigJSON"
DMG_FILE="$PROJECT_DIR/build/BigJSON-${VERSION}.dmg"
STAGING_DIR="$BUILD_DIR/dmg-staging"

echo "==> Cleaning previous build outputs"
rm -rf "$BUILD_DIR" "$DMG_FILE"
mkdir -p "$BUILD_DIR" "$STAGING_DIR"

echo "==> Building Release configuration"
xcodebuild \
    -project "$PROJECT_DIR/app/BigJSON.xcodeproj" \
    -scheme "$SCHEME" \
    -configuration Release \
    -destination 'platform=macOS' \
    -derivedDataPath "$BUILD_DIR/DerivedData" \
    CODE_SIGN_STYLE=Manual \
    CODE_SIGNING_REQUIRED=NO \
    CODE_SIGN_IDENTITY="-" \
    build | grep -E '(error|warning|BUILD)' || true

APP_PATH="$BUILD_DIR/DerivedData/Build/Products/Release/$APP_NAME"
if [[ ! -d "$APP_PATH" ]]; then
    echo "ERROR: Built app not found at $APP_PATH" >&2
    exit 1
fi

echo "==> Staging app for DMG"
cp -R "$APP_PATH" "$STAGING_DIR/"

# Code-signing
if [[ -n "${APPLE_DEVELOPER_ID:-}" ]]; then
    echo "==> Codesigning with Developer ID: $APPLE_DEVELOPER_ID"
    codesign --force --deep --options runtime --timestamp \
        --sign "$APPLE_DEVELOPER_ID" \
        "$STAGING_DIR/$APP_NAME"
    codesign --verify --deep --strict --verbose=2 "$STAGING_DIR/$APP_NAME"
else
    echo "==> Ad-hoc signing (set APPLE_DEVELOPER_ID for distribution-quality signing)"
    codesign --force --deep --sign - "$STAGING_DIR/$APP_NAME"
fi

# Notarization (only useful if signed with Developer ID)
if [[ "${NOTARIZE:-0}" == "1" && -n "${APPLE_DEVELOPER_ID:-}" ]]; then
    : "${APPLE_ID:?APPLE_ID required for notarization}"
    : "${APPLE_TEAM_ID:?APPLE_TEAM_ID required for notarization}"
    : "${APPLE_APP_PASSWORD:?APPLE_APP_PASSWORD required for notarization}"
    NOTARY_ZIP="$BUILD_DIR/notary.zip"
    echo "==> Zipping for notarization"
    /usr/bin/ditto -c -k --keepParent "$STAGING_DIR/$APP_NAME" "$NOTARY_ZIP"
    echo "==> Submitting to Apple notary (this can take several minutes)"
    xcrun notarytool submit "$NOTARY_ZIP" \
        --apple-id "$APPLE_ID" \
        --team-id "$APPLE_TEAM_ID" \
        --password "$APPLE_APP_PASSWORD" \
        --wait
    echo "==> Stapling notarization ticket"
    xcrun stapler staple "$STAGING_DIR/$APP_NAME"
fi

# DMG layout: app + symlink to /Applications for the classic drag-install flow.
ln -s /Applications "$STAGING_DIR/Applications"

echo "==> Building DMG: $DMG_FILE"
hdiutil create \
    -volname "$DMG_VOL_NAME" \
    -srcfolder "$STAGING_DIR" \
    -ov \
    -format UDZO \
    -fs HFS+ \
    "$DMG_FILE" >/dev/null

# Sign the DMG itself if we have a Developer ID — required for Gatekeeper
# to accept it without warnings on first download.
if [[ -n "${APPLE_DEVELOPER_ID:-}" ]]; then
    echo "==> Signing DMG"
    codesign --force --sign "$APPLE_DEVELOPER_ID" --timestamp "$DMG_FILE"
    if [[ "${NOTARIZE:-0}" == "1" ]]; then
        echo "==> Notarizing DMG"
        xcrun notarytool submit "$DMG_FILE" \
            --apple-id "$APPLE_ID" \
            --team-id "$APPLE_TEAM_ID" \
            --password "$APPLE_APP_PASSWORD" \
            --wait
        xcrun stapler staple "$DMG_FILE"
    fi
fi

SIZE=$(du -h "$DMG_FILE" | cut -f1)
echo
echo "==> Done"
echo "    $DMG_FILE  ($SIZE)"
