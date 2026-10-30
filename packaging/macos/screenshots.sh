#!/bin/sh
# Opens the built app with a few queries and screenshots each, light and
# dark, so the look can be checked without a Mac at hand.
set -u
cd "$(dirname "$0")/../.."
exe=dist/Heimdall.app/Contents/MacOS/Heimdall
out=dist/screens
mkdir -p "$out"
config="$HOME/Library/Application Support/Heimdall/config.toml"

shot() {
    name=$1
    shift
    "$exe" "$@" &
    pid=$!
    sleep 5
    screencapture -x "$out/$name.png"
    kill "$pid" 2>/dev/null
    wait "$pid" 2>/dev/null
    sleep 1
}

# The first run writes the default config and builds the index.
shot first-run
for theme in light dark; do
    sed -i '' "s/^theme = .*/theme = \"$theme\"/" "$config"
    shot "$theme-empty"
    shot "$theme-apps" --query "te"
    shot "$theme-calc" --query "2^10 * 3"
    shot "$theme-settings" --query "dark mode"
    shot "$theme-tips" --query "?"
    shot "$theme-files" --query "Info.plist"
done
ls -la "$out"
