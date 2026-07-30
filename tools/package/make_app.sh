#!/bin/sh
# Build "After Dark Player.app" — the double-clickable macOS build.
#
#     tools/package/make_app.sh [output-dir]
#
# The point of this script is a person who is not a programmer. They get one
# icon, they double-click it, and the app walks them through the one thing it
# cannot do for them. No terminal, no cargo, no "first put your modules in
# ./modules".
#
# It also builds "Lunatic Fringe Player.app": the same binary, in a bundle that
# names one module (see PINNED below). That app opens the game; the other opens
# the list. They are two icons over one build, and they share everything that
# matters — the imported library, the settings and the high score table all live
# in the application-support directory, which is keyed on the user's home and
# not on which of the two was double-clicked.
#
# Both are named "... Player" rather than after the products they run. After
# Dark and Lunatic Fringe are Berkeley Systems' and are not in this repository;
# an app that took their name outright would be claiming to be the thing it
# merely plays. Greg Parker's Fringe Player is the precedent, and `APP_NAME` in
# crates/ad-player/src/main.rs is the same decision inside the program.
#
# The bundle carries **no modules and no fonts**. Those are Berkeley Systems'
# and Apple's work, and shipping them would be redistributing someone else's
# software — so the app asks for the user's own copy of the original disk on
# first launch and extracts it itself (see crates/ad-runtime/src/library.rs).
# What it extracts goes to the application-support directory, not in here, so
# replacing this bundle with a newer one never costs somebody their library.
#
# The app is ad-hoc signed with `codesign -s -`. That is free, needs no Apple
# account, and is not optional on Apple Silicon: a completely unsigned binary is
# killed by the kernel rather than merely warned about. It is not *notarized*,
# so the first launch still needs right-click -> Open; see README.
set -eu

REPO=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:-"$REPO/dist"}
APP="$OUT/After Dark Player.app"

# Where cargo actually puts things. Assuming `$REPO/target` is wrong the moment
# `CARGO_TARGET_DIR` is set: cargo builds where it is told, and the copy below
# would pick up whatever stale binary was left behind in `target/release`.
TARGET_DIR=${CARGO_TARGET_DIR:-"$REPO/target"}
BIN="$TARGET_DIR/release/ad-player"

# The module the second app opens, by the title the browser lists it under —
# which is also the module file's name, so this is not a label but a lookup. The
# app itself is called "$PINNED Player". One line to change, and one line to
# delete if a second icon stops being wanted.
PINNED="Lunatic Fringe"

echo "==> building the player"
cd "$REPO"
cargo build -p ad-player --release

# A universal binary when both targets are installed, so the app runs on an
# Intel Mac as well as Apple Silicon. You cannot ask the person you are giving
# it to which chip they have and then send them the wrong one.
UNIVERSAL=""
if rustup target list --installed 2>/dev/null | grep -q x86_64-apple-darwin; then
  if cargo build -p ad-player --release --target x86_64-apple-darwin >/dev/null 2>&1; then
    lipo -create -output /tmp/ad-player-universal \
      "$TARGET_DIR/release/ad-player" \
      "$TARGET_DIR/x86_64-apple-darwin/release/ad-player" 2>/dev/null &&
      UNIVERSAL=/tmp/ad-player-universal
  fi
fi
if [ -n "$UNIVERSAL" ]; then
  BIN=$UNIVERSAL
  echo "    universal (arm64 + x86_64)"
else
  echo "    this machine's architecture only — \`rustup target add x86_64-apple-darwin\`"
  echo "    then re-run to build one that also works on Intel Macs"
fi

# The icons are committed next to this script, so packaging needs no Python and
# no drawing program. Regenerated only when one is missing — see make_icons.py,
# which is where they are changed.
for icon in after-dark lunatic-fringe; do
  if [ ! -f "$REPO/tools/package/$icon.icns" ]; then
    echo "==> drawing the icons"
    python3 "$REPO/tools/package/make_icons.py"
    break
  fi
done

# Assemble one bundle:
#
#     bundle <path> <display name> <bundle id> <icon> [pinned module]
#
# Two apps out of one function rather than two copies of a plist, because the
# only honest difference between them is the name, the identifier and whether
# `Resources/module` is there. Anything else that drifted between the two would
# be a bug nobody notices until one of them behaves oddly.
bundle() {
  app=$1
  name=$2
  ident=$3
  icon=$4
  module=${5:-}

  echo "==> assembling $app"
  rm -rf "$app"
  mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
  cp "$BIN" "$app/Contents/MacOS/ad-player"
  cp "$REPO/tools/package/$icon.icns" "$app/Contents/Resources/$icon.icns"

  # Nothing else is copied into Resources. Which modules a person can play is
  # decided at run time from the disk they import, filtered by the same survey
  # baseline this script used to filter by — see `DOES_NOT_PLAY` in
  # crates/ad-player/src/library.rs, which a test holds to the baseline.
  #
  # `module` is not a module: it is the *title* of one, and it is what makes
  # this bundle open that module instead of the list. See `pinned_module` in
  # crates/ad-player/src/main.rs.
  if [ -n "$module" ]; then
    echo "$module" > "$app/Contents/Resources/module"
  fi

  # A distinct identifier per bundle. The save location does not depend on it —
  # that is the point, it is how both apps reach one high score table — but
  # Launch Services keys the Dock, the "always open with" association and
  # `open -b` on it, and two apps claiming one identifier confuses all three.
  cat > "$app/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN"
  "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
  <key>CFBundleName</key>              <string>$name</string>
  <key>CFBundleDisplayName</key>       <string>$name</string>
  <key>CFBundleExecutable</key>        <string>ad-player</string>
  <key>CFBundleIconFile</key>          <string>$icon</string>
  <key>CFBundleIdentifier</key>        <string>$ident</string>
  <key>CFBundlePackageType</key>       <string>APPL</string>
  <key>CFBundleShortVersionString</key><string>0.1.0</string>
  <key>CFBundleVersion</key>           <string>0.1.0</string>
  <key>LSMinimumSystemVersion</key>    <string>11.0</string>
  <!-- A normal windowed application: it needs a Dock icon and a menu bar to be
       clickable and focusable, which an agent/background app would not get. -->
  <key>LSUIElement</key>               <false/>
  <key>NSHighResolutionCapable</key>   <true/>
  <!-- No app sandbox is requested, and that is deliberate: a sandboxed app gets
       its own redirected ~/Library/Application Support, which would give each
       of these two bundles a private library and a private high score table. -->
</dict>
</plist>
PLIST

  printf 'APPL????' > "$app/Contents/PkgInfo"

  echo "    signing (ad-hoc)"
  codesign --force --deep -s - "$app"
  codesign --verify --verbose=1 "$app" 2>&1 | sed 's/^/    /'
}

SLUG=$(echo "$PINNED" | tr '[:upper:]' '[:lower:]' | tr -c '[:alnum:]' '-' | sed 's/-*$//')
bundle "$APP" "After Dark Player" "net.panth.afterdark" "after-dark"
bundle "$OUT/$PINNED Player.app" "$PINNED Player" \
  "net.panth.afterdark.$(echo "$SLUG" | tr -d '-')" "$SLUG" "$PINNED"

echo
echo "Built: $APP"
echo "       $OUT/$PINNED Player.app  — opens $PINNED instead of the list"
echo "Double-click one. On first launch macOS will refuse because it is not"
echo "notarized: right-click the app -> Open -> Open. It remembers after that."
echo
echo "Neither ships with modules. The first launch asks for an After Dark disk"
echo "image and extracts one into the application-support directory, where it"
echo "survives these bundles being replaced — and where both of them read it,"
echo "along with one shared set of high scores."
