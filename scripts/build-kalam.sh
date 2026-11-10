#!/bin/bash
# Local developer build ONLY. No Node/npm/zx, Cargo, or system toolchain changes.
set -euo pipefail
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd -P)"
ARCH="${1:-$(go env GOARCH)}"
case "$ARCH" in arm64) CC_ARCH=arm64;; amd64) CC_ARCH=x86_64;; *) echo 'usage: build-kalam.sh [arm64|amd64]' >&2; exit 2;; esac
[[ $(uname -s) == Darwin ]] || { echo 'this build currently supports macOS only' >&2; exit 2; }
GO_VERSION="$(go env GOVERSION)"
if [[ $GO_VERSION =~ ^go([0-9]+)\.([0-9]+) ]]; then
 (( BASH_REMATCH[1] > 1 || (BASH_REMATCH[1] == 1 && BASH_REMATCH[2] >= 26) )) || { echo 'Go >=1.26 required for valid Mach-O chained fixups' >&2; exit 2; }
else echo "unrecognized Go version: $GO_VERSION" >&2; exit 2; fi
OUT="$ROOT/target/native/$ARCH"
CACHE="$ROOT/.cache/kalam/$ARCH"
mkdir -p "$OUT" "$CACHE/lib" "$ROOT/.cache/go-build" "$ROOT/.cache/go-mod" "$ROOT/.cache/go-tmp"
export GOCACHE="$ROOT/.cache/go-build" GOMODCACHE="$ROOT/.cache/go-mod" GOTMPDIR="$ROOT/.cache/go-tmp"
export GOOS=darwin GOARCH="$ARCH" CGO_ENABLED=1 GOTOOLCHAIN=local
export SDKROOT="${SDKROOT:-$(xcrun --sdk macosx --show-sdk-path)}"
export CC="${CC:-$(xcrun --find clang)}"
export CGO_CFLAGS="${CGO_CFLAGS:-} -arch $CC_ARCH -isysroot $SDKROOT -Wno-deprecated-declarations"
export CGO_LDFLAGS="${CGO_LDFLAGS:-} -arch $CC_ARCH -isysroot $SDKROOT"
LOCAL_LIBUSB=0
if [[ -n ${LIBUSB_DYLIB:-} || -n ${LIBUSB_INCLUDE_DIR:-} ]]; then
 : "${LIBUSB_DYLIB:?supply an explicit absolute libusb dylib input}" "${LIBUSB_INCLUDE_DIR:?supply the matching libusb.h directory}"
 [[ $LIBUSB_DYLIB == /* && $LIBUSB_INCLUDE_DIR == /* && -f $LIBUSB_DYLIB && -f $LIBUSB_INCLUDE_DIR/libusb.h ]] || { echo 'libusb inputs must be existing absolute paths' >&2; exit 2; }
 # Copy only as build artifact; input (including old OpenMTP) is strictly read-only.
 cp "$LIBUSB_DYLIB" "$OUT/libusb.dylib"
 chmod u+w "$OUT/libusb.dylib"
 install_name_tool -id '@loader_path/libusb.dylib' "$OUT/libusb.dylib"
 ln -sf "$OUT/libusb.dylib" "$CACHE/lib/libusb-1.0.dylib"
 export KALAM_LIBUSB_INCLUDE="$LIBUSB_INCLUDE_DIR" KALAM_LIBUSB_LINK_DIR="$CACHE/lib"
 export PKG_CONFIG="$ROOT/native/kalam/pkg-config-local.sh"
 LOCAL_LIBUSB=1
else
 export PKG_CONFIG="${PKG_CONFIG:-pkg-config}"
 command -v "$PKG_CONFIG" >/dev/null || { echo 'pkg-config not installed; supply LIBUSB_DYLIB and LIBUSB_INCLUDE_DIR for explicit-local mode' >&2; exit 2; }
 "$PKG_CONFIG" --cflags --libs libusb-1.0
fi
printf 'LOCAL DEVELOPER BUILD (upstream license clearance pending)\nGo: %s\nSDK: %s\nArch: %s\n' "$GO_VERSION" "$SDKROOT" "$ARCH"
# -mod=readonly: retain upstream checksum/version pins. Go >=1.26 fixes seg_count;
# no binary fixup patching, old linker replacement, or global SDK mutation.
go build -C "$ROOT/native/kalam" -mod=readonly -trimpath -buildmode=c-shared \
 -ldflags='-linkmode=external -extldflags=-Wl,-headerpad_max_install_names' \
 -o "$OUT/kalam.dylib" .
install_name_tool -id '@rpath/kalam.dylib' "$OUT/kalam.dylib"
# Explicit-local mode embeds the copied dependency's @loader_path install name.
if [[ $LOCAL_LIBUSB == 1 ]]; then
 otool -L "$OUT/kalam.dylib" | while IFS= read -r line; do
  case "$line" in *@@HOMEBREW*|*"$LIBUSB_DYLIB"*) echo 'unexpected libusb input install name in output' >&2; exit 1;; esac
 done
 codesign --force --sign - "$OUT/libusb.dylib"
fi
codesign --force --sign - "$OUT/kalam.dylib"
file "$OUT/kalam.dylib"
nm -gU "$OUT/kalam.dylib"
otool -L "$OUT/kalam.dylib"
printf 'Built %s (not redistributed, no device transfer validation)\n' "$OUT/kalam.dylib"
