#!/bin/sh
# Build the Linux and Windows downloads.
#
#     tools/package/make_dist.sh [output-dir]
#
# The counterpart to make_app.sh, which builds the macOS bundles. Same two
# applications, same reasoning behind them — see that script's header, which is
# where the naming and the licensing are explained rather than repeated here.
#
# What differs is the layout. macOS has a bundle to put things in; Linux and
# Windows have a folder with a binary in it, so the file that pins the second
# application to one module sits beside the executable instead of in
# `Contents/Resources`. `pinned_beside` in crates/ad-player/src/main.rs reads
# both, and a test holds it to both.
#
# Two folders means two copies of the binary, which is a few megabytes of
# duplication rather than a launcher script or a command-line flag. That is the
# trade the macOS side already makes, and it keeps the promise that there is
# nothing to type and nothing to configure: the folder you open decides what you
# get.
#
# Neither folder carries modules or fonts. Those are Berkeley Systems' and
# Apple's; the application asks for the user's own disk image on first launch.
set -eu

REPO=$(cd "$(dirname "$0")/../.." && pwd)
OUT=${1:-"$REPO/dist"}

# The module the second application opens, by the title the browser lists it
# under. One line to change, and one line to delete if a second folder stops
# being wanted — the same knob as make_app.sh.
PINNED="Lunatic Fringe"

# Windows executables need the suffix, and this script runs on the Windows
# runner under Git Bash, where `uname` says MINGW64_NT and `$OS` says
# Windows_NT. Checking both is cheaper than being wrong on one of them.
EXE=""
case "$(uname -s 2>/dev/null || echo)${OS:-}" in
  MINGW* | MSYS* | CYGWIN* | *Windows_NT*) EXE=".exe" ;;
esac

echo "==> building the player"
cd "$REPO"
cargo build -p ad-player --release

# Where cargo actually put it. Reading `$REPO/target` directly is wrong the
# moment `CARGO_TARGET_DIR` is set — cargo builds where it is told and the copy
# below would take whatever stale binary was left in `target/release`, which is
# how a Linux tarball ends up with a macOS binary in it and nothing says a word.
BIN="${CARGO_TARGET_DIR:-$REPO/target}/release/ad-player$EXE"
[ -f "$BIN" ] || { echo "no binary at $BIN" >&2; exit 1; }

# Assemble one folder:
#
#     folder <path> <executable name> [pinned module]
#
# The executable is named after the application rather than left as `ad-player`
# because this is what the person downloading it double-clicks; `ad-player` in a
# folder called "After Dark Player" reads like the wrong file.
folder() {
  dir=$1
  name=$2
  module=${3:-}

  echo "==> assembling $dir"
  rm -rf "$dir"
  mkdir -p "$dir"
  cp "$BIN" "$dir/$name$EXE"
  chmod +x "$dir/$name$EXE"

  # Not a module: the *title* of one. See `pinned_beside`.
  if [ -n "$module" ]; then
    printf '%s\n' "$module" > "$dir/module"
  fi
}

folder "$OUT/After Dark Player" "After Dark Player"
folder "$OUT/$PINNED Player" "$PINNED Player" "$PINNED"

echo
echo "Built: $OUT/After Dark Player"
echo "       $OUT/$PINNED Player  — opens $PINNED instead of the list"
echo
echo "Neither ships with modules. The first launch asks for an After Dark disk"
echo "image and extracts one into the platform's application-data directory,"
echo "where both folders read it along with one shared set of high scores."
