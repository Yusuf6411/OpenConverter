#!/bin/sh
# Signs, notarizes and staples a macOS binary or .app. Needs an Apple Developer account.
# Credentials come from the environment only; nothing is stored in the repository:
#   APPLE_SIGN_IDENTITY  "Developer ID Application: Name (TEAMID)"
#   APPLE_ID, APPLE_TEAM_ID, APPLE_APP_PASSWORD  for notarytool
# Usage: tools/macos_sign_notarize.sh PATH
set -eu
p="${1:?usage: macos_sign_notarize.sh PATH}"
: "${APPLE_SIGN_IDENTITY:?}" "${APPLE_ID:?}" "${APPLE_TEAM_ID:?}" "${APPLE_APP_PASSWORD:?}"
codesign --force --options runtime --timestamp --sign "$APPLE_SIGN_IDENTITY" "$p"
zip="$(mktemp -d)/notarize.zip"
ditto -c -k --keepParent "$p" "$zip"
xcrun notarytool submit "$zip" --apple-id "$APPLE_ID" --team-id "$APPLE_TEAM_ID" \
  --password "$APPLE_APP_PASSWORD" --wait
case "$p" in *.app|*.dmg|*.pkg) xcrun stapler staple "$p" ;; esac
