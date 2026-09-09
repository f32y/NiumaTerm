#!/bin/sh
# Assemble NiumaTerm.app around an already-built binary.
#
# A bare executable is not an application as far as macOS is concerned: it has
# no bundle identifier, which is what `UNUserNotificationCenter` refuses to
# work without and what the permissions the user grants are remembered against.
# It also cannot carry an icon or become a regular, activatable application.
#
# Usage:
#   scripts/bundle-macos.sh [--profile release] [--binary PATH] [--out DIR]
#                           [--identifier ID] [--icon PNG] [--sign IDENTITY]
#
# The signing identity defaults to `-`, an ad-hoc signature, which is enough to
# run the result locally. Distribution needs a Developer ID identity and, after
# that, notarization.
set -eu

profile=release
binary=
out=dist
identifier=${NMT_BUNDLE_ID:-io.f32.NiumaTerm}
# A shared high-resolution source supplies both standard and Retina slices.
icon=assets/app-icon.png
sign=-
min_macos=13.0

while [ $# -gt 0 ]; do
  case $1 in
    --profile) profile=$2; shift 2 ;;
    --binary) binary=$2; shift 2 ;;
    --out) out=$2; shift 2 ;;
    --identifier) identifier=$2; shift 2 ;;
    --icon) icon=$2; shift 2 ;;
    --sign) sign=$2; shift 2 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

root=$(CDPATH= cd -- "$(dirname -- "$0")/.." && pwd)
cd "$root"

[ -n "$binary" ] || binary="target/$profile/NiumaTerm"
if [ ! -x "$binary" ]; then
  echo "no executable at $binary; build it first" >&2
  exit 1
fi

# Apple silicon is the only supported target, so a binary without an arm64
# slice would produce a bundle that cannot run where it is meant to — and
# nothing later in the assembly would notice.
if ! lipo -archs "$binary" | tr ' ' '\n' | grep -qx arm64; then
  echo "$binary is $(lipo -archs "$binary"); an arm64 slice is required" >&2
  exit 1
fi

# `CFBundleShortVersionString` and `CFBundleVersion` must be dotted numbers, so
# they take the crate version. The build's own label — which is what the
# application compares against a release feed, and which for a nightly is not a
# dotted number at all — is carried beside them.
short_version=$(sed -n 's/^version = "\(.*\)"$/\1/p' Cargo.toml | head -1)
version_label=${NIUMATERM_VERSION:-v$short_version}

app="$out/NiumaTerm.app"
rm -rf "$app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"

# `iconutil` reads a directory of exactly these names; anything else in it is
# an error rather than an ignored file.
iconset=$(mktemp -d)
trap 'rm -rf "$iconset"' EXIT
mkdir -p "$iconset/AppIcon.iconset"
for spec in 16:16x16 32:16x16@2x 32:32x32 64:32x32@2x 128:128x128 256:128x128@2x 256:256x256 512:256x256@2x 512:512x512 1024:512x512@2x; do
  pixels=${spec%%:*}
  name=${spec#*:}
  sips -z "$pixels" "$pixels" "$icon" --out "$iconset/AppIcon.iconset/icon_$name.png" >/dev/null
done
iconutil --convert icns --output "$app/Contents/Resources/AppIcon.icns" "$iconset/AppIcon.iconset"

cp "$binary" "$app/Contents/MacOS/NiumaTerm"

# The parser bundle is opened by path at startup, from the directory holding
# the executable, so it travels beside it rather than in `Resources`.
bundle_dylib=$(dirname "$binary")/libtree_sitter.dylib
if [ -f "$bundle_dylib" ]; then
  cp "$bundle_dylib" "$app/Contents/MacOS/libtree_sitter.dylib"
else
  echo "note: no libtree_sitter.dylib beside $binary; syntax highlighting will be limited" >&2
fi

# The executable is linked against Sparkle and will not launch without it. The
# loader is told to look in `@executable_path` and in `Contents/Frameworks`; the
# build leaves a copy beside the binary for the first, and this puts it where the
# second finds it.
framework=$(dirname "$binary")/Sparkle.framework
if [ ! -d "$framework" ]; then
  echo "no Sparkle.framework beside $binary; build first" >&2
  exit 1
fi
mkdir -p "$app/Contents/Frameworks"
# ditto rather than cp -R: a framework's Versions/Current symlinks are part of
# what its code signature seals, and a copy that resolves them will not load.
ditto "$framework" "$app/Contents/Frameworks/Sparkle.framework"
# The XPC services let a sandboxed host reach the network and the installer
# through separate processes. A terminal emulator cannot be sandboxed, so they
# would only add two more bundles to sign.
rm -rf "$app/Contents/Frameworks/Sparkle.framework/Versions/B/XPCServices"

sed \
  -e "s|@@BUNDLE_ID@@|$identifier|g" \
  -e "s|@@SHORT_VERSION@@|$short_version|g" \
  -e "s|@@VERSION_LABEL@@|$version_label|g" \
  -e "s|@@MIN_MACOS@@|$min_macos|g" \
  assets/macos/Info.plist > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist" >/dev/null

# Classic-era metadata that a few tools still read to identify the bundle kind.
printf 'APPL????' > "$app/Contents/PkgInfo"

# Signed last: a signature covers the bundle's contents, so anything written
# afterwards invalidates it.
codesign --force --sign "$sign" --timestamp=none "$app" >/dev/null 2>&1 ||
  codesign --force --sign "$sign" "$app"
codesign --verify --strict "$app"

echo "$app"
