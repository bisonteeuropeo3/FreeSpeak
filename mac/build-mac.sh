#!/usr/bin/env bash
# Builds FreeSpeak on macOS.
#
# NeXTSTEP-style bundle, because a bare Mach-O binary cannot do two things this
# app needs on modern macOS:
#
#   * Microphone access. Since 10.14 the microphone is protected by TCC, and the
#     usage description has to come from the *main bundle's* Info.plist. A bare
#     binary has no bundle, so macOS gives it no Microphone entry at all and the
#     app records silence - which looked exactly like a mute microphone.
#   * Accessibility for the paste keystroke. TCC remembers the grant per code
#     signature. An unsigned binary rebuilt with `cargo build` changes identity
#     and loses the grant silently, so the bundle is signed (ad hoc) here.
#
# Needs the Xcode command line tools:  xcode-select --install
set -euo pipefail

root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$root/rust"

if ! command -v cargo >/dev/null 2>&1; then
    cat <<'EOF'
cargo was not found. Install Rust first:

    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
    source "$HOME/.cargo/env"
EOF
    exit 1
fi

if ! xcode-select -p >/dev/null 2>&1; then
    echo "The Xcode command line tools are missing (the linker and CoreAudio headers)."
    echo "Install them with:  xcode-select --install"
    exit 1
fi

cargo build --release --locked

# $CARGO_TARGET_DIR or --target-dir moves the output; ask cargo where it went
# instead of assuming, so the printed instructions cannot point at nothing.
target_dir="$(cargo metadata --format-version 1 --no-deps 2>/dev/null \
    | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
target_dir="${target_dir:-$root/rust/target}"
binary="$target_dir/release/freespeak"

if [ ! -f "$binary" ]; then
    echo "expected the build at $binary but it is not there" >&2
    exit 1
fi

version="$(sed -n 's/^version *= *"\(.*\)"/\1/p' Cargo.toml | head -n1)"
app="$root/mac/dist/FreeSpeak.app"
contents="$app/Contents"

rm -rf "$app"
mkdir -p "$contents/MacOS" "$contents/Resources"
cp "$binary" "$contents/MacOS/freespeak"
chmod +x "$contents/MacOS/freespeak"

# ---------------------------------------------------------------- Info.plist
#
# LSUIElement keeps it out of the Dock and the app switcher: this is a
# background agent, not a window.
cat > "$contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>CFBundleName</key>
    <string>FreeSpeak</string>
    <key>CFBundleDisplayName</key>
    <string>FreeSpeak</string>
    <key>CFBundleIdentifier</key>
    <string>com.freespeak.dictation</string>
    <key>CFBundleExecutable</key>
    <string>freespeak</string>
    <key>CFBundlePackageType</key>
    <string>APPL</string>
    <key>CFBundleShortVersionString</key>
    <string>${version:-0.1.0}</string>
    <key>CFBundleVersion</key>
    <string>${version:-0.1.0}</string>
    <key>CFBundleIconFile</key>
    <string>freespeak</string>
    <key>LSMinimumSystemVersion</key>
    <string>10.15</string>
    <key>LSUIElement</key>
    <true/>
    <key>NSHighResolutionCapable</key>
    <true/>
    <key>NSMicrophoneUsageDescription</key>
    <string>FreeSpeak records while you hold the dictation hotkey, so your speech can be transcribed.</string>
</dict>
</plist>
PLIST

# A property list that does not parse is rejected by macOS with no visible
# error, so check it here where the message can still be read.
plutil -lint "$contents/Info.plist" >/dev/null

# ---------------------------------------------------------------------- icon
#
# iconutil wants an .iconset: the same drawing at the sizes macOS asks for.
# Optional - a missing icon is cosmetic, a missing plist is not.
src_icon="$root/windows/assets/freespeak-preview.png"
if [ -f "$src_icon" ] && command -v sips >/dev/null 2>&1; then
    iconset="$(mktemp -d)/freespeak.iconset"
    mkdir -p "$iconset"
    # Only the sizes iconutil expects: an unexpected name makes it reject the
    # whole set, not just that image.
    for size in 16 32 128 256 512; do
        sips -z "$size" "$size" "$src_icon" --out "$iconset/icon_${size}x${size}.png" >/dev/null
        double=$((size * 2))
        sips -z "$double" "$double" "$src_icon" --out "$iconset/icon_${size}x${size}@2x.png" >/dev/null
    done
    iconutil -c icns "$iconset" -o "$contents/Resources/freespeak.icns" >/dev/null 2>&1 \
        || echo "note: could not build the icon, carrying on without it"
    rm -rf "$(dirname "$iconset")"
fi

# ------------------------------------------------------------------- signing
#
# Ad hoc ("-") is enough for TCC to recognise a stable identity, which is what
# makes a Microphone or Accessibility grant survive a rebuild. A Developer ID
# would additionally allow notarisation for distribution; it is not needed to
# run this on your own Mac.
codesign --force --sign - "$app" >/dev/null 2>&1 \
    || echo "note: ad-hoc signing failed; macOS may ask for permissions again after each build"

# The installer the app itself uses must point at the bundle, not at target/.
"$contents/MacOS/freespeak" --init >/dev/null 2>&1 || true

cat <<EOF

built: $app
       (this is the thing to double-click, and the thing to keep)

Next steps:
  1. Set your API key:   "$contents/MacOS/freespeak" --set-key
  2. Grant the microphone when macOS asks, or add the app by hand in
     System Settings > Privacy & Security > Microphone.
  3. Start at login:     "$contents/MacOS/freespeak" --install-autostart
  4. Run it now:         open "$app"

Permissions, once, in System Settings > Privacy & Security:
  * Microphone    - required, or every recording is silence. The bundle carries
                    NSMicrophoneUsageDescription, which is what lets macOS ask.
  * Accessibility - required only for pasting into the focused app (Cmd+V is a
                    synthetic keystroke). Add "FreeSpeak.app" with the + button.
                    Without it the transcript still reaches the clipboard.
  * Hotkeys and the tones need no permission at all.

The app has no window and no Dock icon on purpose: press the hotkey
(ctrl+alt+space) to start and stop dictation, ctrl+alt+shift+q to quit.
EOF
