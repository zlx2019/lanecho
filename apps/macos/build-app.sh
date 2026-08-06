#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"

if ! command -v xcodegen >/dev/null 2>&1; then
    echo "lack xcodegen: brew install xcodegen" >&2
    exit 1
fi

echo "==> Generate an Xcode project"
xcodegen generate --quiet

APP="build/Build/Products/Release/Lanecho.app"

rm -rf "$APP"

echo "==> Compile Release"
set +e
xcodebuild \
    -project Lanecho.xcodeproj \
    -scheme LanechoApp \
    -configuration Release \
    -derivedDataPath build \
    build | grep -E "error:|warning:|BUILD"
build_status=${PIPESTATUS[0]}
set -e
if [[ $build_status -ne 0 ]]; then
    echo "Build failed: xcodebuild exit code $build_status" >&2
    exit 1
fi

if [[ ! -d "$APP" ]]; then
    echo "Build failed: No output produced $APP" >&2
    exit 1
fi

echo "==> Temporary Signature"
codesign --force --sign - "$APP"

echo "==> Done: $APP"
if [[ "${1:-}" == "--install" ]]; then
    echo "==> Install to /Applications"
    rm -rf /Applications/Lanecho.app
    cp -R "$APP" /Applications/
    echo "==> Installed: /Applications/Lanecho.app"
fi
