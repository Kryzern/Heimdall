#!/bin/sh
# Builds dist/Heimdall.app (Apple Silicon and Intel in one binary) and
# dist/Heimdall-<version>-macos.zip. Runs on a Mac with Xcode's command line
# tools; CI runs it on every push.
set -eu
cd "$(dirname "$0")/../.."
version=$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -n 1)
export MACOSX_DEPLOYMENT_TARGET=11.0
# Test builds keep line tables, so a crash in heimdall.log says where.
export CARGO_PROFILE_RELEASE_STRIP=none CARGO_PROFILE_RELEASE_DEBUG=line-tables-only
# Objective-C exceptions can only be caught if Rust frames may unwind.
export CARGO_PROFILE_RELEASE_PANIC=unwind
for target in aarch64-apple-darwin x86_64-apple-darwin; do
    cargo build --release -p heimdall-mac --features objc-exceptions --target "$target"
done
app=dist/Heimdall.app
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
lipo -create -output "$app/Contents/MacOS/Heimdall" \
    target/aarch64-apple-darwin/release/heimdall-mac \
    target/x86_64-apple-darwin/release/heimdall-mac
sed "s/@VERSION@/$version/g" packaging/macos/Info.plist > "$app/Contents/Info.plist"
cp packaging/macos/Heimdall.icns "$app/Contents/Resources/"
# An ad-hoc signature: Apple Silicon won't run unsigned code at all. It isn't
# notarised, so a downloaded copy needs right-click > Open the first time.
codesign --force --sign - "$app"
rm -f "dist/Heimdall-$version-macos.zip"
(cd dist && ditto -c -k --keepParent Heimdall.app "Heimdall-$version-macos.zip")
echo "Wrote dist/Heimdall-$version-macos.zip"
