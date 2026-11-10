#!/bin/bash
# Local developer bundle ONLY. Not a release, license clearance or notarization.
set -euo pipefail
ROOT="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")/.." && pwd -P)"
PROFILE=debug
SKIP_BUILD=0
AD_HOC_SIGN=0
MAKE_ZIP=0
BINARY_DIR=
NATIVE_DIR="$ROOT/target/native/arm64"

usage() {
    printf '%s\n' \
        'Usage: bash scripts/package-local.sh [options]' \
        '  --skip-build       Package existing verified binaries (no Cargo)' \
        '  --profile PROFILE  debug (default) or release' \
        '  --binary-dir PATH  Existing binaries; requires --skip-build' \
        '  --native-dir PATH  Local build directory under repo target/native/' \
        '  --ad-hoc-sign      Explicit local ad-hoc signing of bundle copies only' \
        '  --zip              Create a local ZIP beside the bundle; never upload' \
        '  --help             Show help without building or packaging' \
        'Default runs cargo build --locked for app-gpui + mtp-worker.' \
        'macOS arm64 developer use only; no production release or Intel claim.'
}
fail() { printf 'package-local: %s\n' "$*" >&2; exit 1; }
while [[ $# -gt 0 ]]; do
    case "$1" in
        --skip-build) SKIP_BUILD=1; shift ;;
        --profile|--binary-dir|--native-dir)
            [[ $# -ge 2 && -n "$2" ]] || fail "$1 requires a value"
            case "$1" in
                --profile) PROFILE="$2" ;;
                --binary-dir) BINARY_DIR="$2" ;;
                --native-dir) NATIVE_DIR="$2" ;;
            esac
            shift 2 ;;
        --ad-hoc-sign) AD_HOC_SIGN=1; shift ;;
        --zip) MAKE_ZIP=1; shift ;;
        --help) usage; exit 0 ;;
        *) usage >&2; fail "unknown option: $1" ;;
    esac
done
case "$PROFILE" in debug|release) ;; *) fail 'profile must be debug or release' ;; esac
[[ $(uname -s) == Darwin ]] || fail 'macOS only'
[[ $(uname -m) == arm64 ]] || fail 'arm64 developer host only; Intel/Rosetta is not validated'
[[ -z "$BINARY_DIR" || $SKIP_BUILD == 1 ]] || fail '--binary-dir requires --skip-build'
for tool in /usr/bin/lipo /usr/bin/otool /usr/bin/shasum /usr/bin/cmp /usr/bin/plutil /usr/bin/uuidgen; do
    [[ -x "$tool" ]] || fail "required tool unavailable: $tool"
done
if [[ $AD_HOC_SIGN == 1 ]]; then [[ -x /usr/bin/codesign ]] || fail 'codesign unavailable'; fi
if [[ $MAKE_ZIP == 1 ]]; then [[ -x /usr/bin/ditto ]] || fail 'ditto unavailable'; fi

if [[ $SKIP_BUILD == 0 ]]; then
    printf 'Building Rust artifacts; coordinate with the parent agent before invoking this default mode.\n'
    export PATH="${HOME:?}/.cargo/bin:$PATH"
    export CARGO_HOME="${CARGO_HOME:-$ROOT/.cache/cargo}"
    export CARGO_TARGET_DIR="${CARGO_TARGET_DIR:-$ROOT/target}"
    BUILD_ARGS=(build --locked --manifest-path "$ROOT/Cargo.toml" -p app-gpui -p mtp-worker)
    if [[ $PROFILE == release ]]; then BUILD_ARGS+=(--release); fi
    cargo "${BUILD_ARGS[@]}"
    BINARY_DIR="$CARGO_TARGET_DIR/$PROFILE"
else
    BINARY_DIR="${BINARY_DIR:-$ROOT/target/$PROFILE}"
fi
[[ -d "$BINARY_DIR" && -d "$NATIVE_DIR" ]] || fail 'binary/native input directory missing'
BINARY_DIR="$(cd -- "$BINARY_DIR" && pwd -P)"
NATIVE_DIR="$(cd -- "$NATIVE_DIR" && pwd -P)"
case "$NATIVE_DIR/" in "$ROOT/target/native/"*) ;; *) fail 'native inputs must be repo-local target/native builds, not an old app or external library' ;; esac
APP="$BINARY_DIR/app-gpui"
WORKER="$BINARY_DIR/mtp-worker"
KALAM="$NATIVE_DIR/kalam.dylib"
LIBUSB="$NATIVE_DIR/libusb.dylib"
[[ -x "$APP" && -x "$WORKER" && -f "$KALAM" && -f "$LIBUSB" ]] || fail 'app-gpui, mtp-worker, kalam.dylib or libusb.dylib input missing'
[[ -f "$ROOT/native/kalam/LICENSE" ]] || fail 'local Kalam license notice missing'

# Refuse unresolved package-manager/old-app dependencies instead of silently
# rewriting them. The local native builder emits @loader_path/libusb.dylib.
MIN_OS=13.0
version_greater() {
    local a b c x y z
    IFS=. read -r a b c <<< "$1"
    IFS=. read -r x y z <<< "$2"
    b="${b:-0}"; c="${c:-0}"; y="${y:-0}"; z="${z:-0}"
    (( 10#$a > 10#$x || (10#$a == 10#$x && 10#$b > 10#$y) || (10#$a == 10#$x && 10#$b == 10#$y && 10#$c > 10#$z) ))
}
check_macho() {
    local input="$1" arch line dependency rpath dependencies load_commands rpaths deployment input_min
    arch="$(/usr/bin/lipo -archs "$input")"
    [[ "$arch" == arm64 ]] || fail "expected thin arm64 Mach-O: $input (got $arch)"
    dependencies="$(/usr/bin/otool -L "$input")"
    load_commands="$(/usr/bin/otool -l "$input")"
    while IFS= read -r line; do
        if [[ "$line" =~ ^[[:space:]]+([^[:space:]]+) ]]; then
            dependency="${BASH_REMATCH[1]}"
            case "$dependency" in
                /usr/lib/*|/System/Library/*) ;;
                @rpath/kalam.dylib) [[ "$input" == "$KALAM" ]] || fail "unexpected dependency: $dependency" ;;
                @loader_path/libusb.dylib) [[ "$input" == "$KALAM" || "$input" == "$LIBUSB" ]] || fail "unexpected dependency: $dependency" ;;
                *) fail "nonportable dependency in $input: $dependency" ;;
            esac
        fi
    done <<< "$dependencies"
    rpaths="$(printf '%s\n' "$load_commands" | /usr/bin/awk '/cmd LC_RPATH/ { r=1; next } r && $1=="path" { print $2; r=0 }')"
    while IFS= read -r rpath; do
        [[ -n "$rpath" ]] || continue
        case "$rpath" in
            @loader_path|@loader_path/../Resources|@executable_path/../Resources) ;;
            *) fail "nonportable LC_RPATH in $input: $rpath" ;;
        esac
    done <<< "$rpaths"
    deployment="$(printf '%s\n' "$load_commands" | /usr/bin/awk '/cmd LC_BUILD_VERSION/ { b=1; next } b && $1=="minos" { print $2; b=0 } /cmd LC_VERSION_MIN_MACOSX/ { v=1; next } v && $1=="version" { print $2; v=0 }')"
    [[ -n "$deployment" ]] || fail "missing deployment target in $input"
    while IFS= read -r input_min; do
        [[ "$input_min" =~ ^[0-9]+\.[0-9]+(\.[0-9]+)?$ ]] || fail "invalid minimum OS in $input"
        if version_greater "$input_min" "$MIN_OS"; then MIN_OS="$input_min"; fi
    done <<< "$deployment"
}
for input in "$APP" "$WORKER" "$KALAM" "$LIBUSB"; do check_macho "$input"; done
printf 'Detected payload minimum macOS: %s (13 is an unverified future target)\n' "$MIN_OS"
INPUT_HASHES="$(/usr/bin/shasum -a 256 "$APP" "$WORKER" "$KALAM" "$LIBUSB")"
[[ $(/usr/bin/otool -D "$KALAM" | /usr/bin/tail -n 1) == '@rpath/kalam.dylib' ]] || fail 'unexpected Kalam install name; rebuild native inputs'
[[ $(/usr/bin/otool -D "$LIBUSB" | /usr/bin/tail -n 1) == '@loader_path/libusb.dylib' ]] || fail 'unexpected libusb install name; rebuild native inputs'

# No rm/mv/overwrite path: every attempt owns a brand-new UUID-named bundle.
[[ ! -L "$ROOT/target" && ! -L "$ROOT/target/bundles" ]] || fail 'output ancestors must not be symlinks'
mkdir -p "$ROOT/target/bundles"
OUT="$(cd -- "$ROOT/target/bundles" && pwd -P)"
[[ "$OUT" == "$ROOT/target/bundles" ]] || fail 'unexpected resolved output directory'
UUID="$(/usr/bin/uuidgen | /usr/bin/tr '[:upper:]' '[:lower:]')"
[[ "$UUID" =~ ^[a-f0-9-]+$ ]] || fail 'invalid unique bundle identifier'
BUNDLE="$OUT/$UUID-MTP-Native.app"
[[ ! -e "$BUNDLE" && ! -L "$BUNDLE" ]] || fail 'unique output already exists; nothing overwritten'
mkdir "$BUNDLE"
printf 'Creating LOCAL DEVELOPER bundle: %s\n' "$BUNDLE"
# A failure intentionally leaves the new partial bundle for inspection, never
# deletes old bundles, and never prints a success message.
mkdir -p "$BUNDLE/Contents/MacOS" "$BUNDLE/Contents/Resources"
cp -p "$APP" "$BUNDLE/Contents/MacOS/app-gpui"
cp -p "$WORKER" "$BUNDLE/Contents/MacOS/mtp-worker"
cp -p "$KALAM" "$BUNDLE/Contents/Resources/kalam.dylib"
cp -p "$LIBUSB" "$BUNDLE/Contents/Resources/libusb.dylib"
cp -p "$ROOT/native/kalam/LICENSE" "$BUNDLE/Contents/Resources/Kalam-LICENSE.txt"
chmod u+x "$BUNDLE/Contents/MacOS/app-gpui" "$BUNDLE/Contents/MacOS/mtp-worker"
/usr/bin/cmp "$APP" "$BUNDLE/Contents/MacOS/app-gpui"
/usr/bin/cmp "$WORKER" "$BUNDLE/Contents/MacOS/mtp-worker"
/usr/bin/cmp "$KALAM" "$BUNDLE/Contents/Resources/kalam.dylib"
/usr/bin/cmp "$LIBUSB" "$BUNDLE/Contents/Resources/libusb.dylib"
cat > "$BUNDLE/Contents/Info.plist" <<PLIST
<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0"><dict>
<key>CFBundleName</key><string>MTP Native Local Developer</string>
<key>CFBundleDisplayName</key><string>MTP Native (Local Developer)</string>
<key>CFBundleIdentifier</key><string>dev.local.mtp-native.$UUID</string>
<key>CFBundleExecutable</key><string>app-gpui</string>
<key>CFBundlePackageType</key><string>APPL</string>
<key>CFBundleShortVersionString</key><string>0.1.0</string>
<key>CFBundleVersion</key><string>1</string>
<key>LSMinimumSystemVersion</key><string>$MIN_OS</string>
<key>LSArchitecturePriority</key><array><string>arm64</string></array>
<key>NSHighResolutionCapable</key><true/>
<key>NSPrincipalClass</key><string>NSApplication</string>
<key>NSHumanReadableCopyright</key><string>Local developer build; upstream license clearance pending. Not a production release.</string>
</dict></plist>
PLIST
cat > "$BUNDLE/Contents/Resources/LOCAL-DEVELOPER-NOTICE.txt" <<'NOTICE'
LOCAL DEVELOPER BUILD ONLY — NOT A PRODUCTION RELEASE

This bundle packages the local Rust app/worker and locally built Kalam/libusb
artifacts for testing on macOS arm64. macOS 13 is a development target, not a
verified support promise. Intel, true-device transfers, all permissions, and
upstream license/provenance clearance are not complete.

Kalam's checked-in MIT license is preserved as Kalam-LICENSE.txt. libusb and
other upstream dependencies retain their own licenses; this notice does not
substitute for a full redistribution/legal review or grant additional rights.
Do not distribute/publicly upload this bundle or ZIP before that review.
No notarization, Developer ID signature, production hardening, or release
approval is claimed. Optional ad-hoc signing is local test signing only.

Finder startup uses only this app's own Resources/kalam.dylib unless --library
or MTP_KALAM_LIBRARY explicitly overrides it. It never selects an old app's
library or a Fake backend. Connect is still a deliberate user action.
NOTICE
/usr/bin/plutil -lint "$BUNDLE/Contents/Info.plist"
[[ "$(/usr/bin/shasum -a 256 "$APP" "$WORKER" "$KALAM" "$LIBUSB")" == "$INPUT_HASHES" ]] || fail 'source inputs changed during packaging; partial bundle retained'
{
    printf 'Source inputs before optional copy-only signing:\n%s\n' "$INPUT_HASHES"
} > "$BUNDLE/Contents/Resources/SOURCE-SHA256.txt"
if [[ $AD_HOC_SIGN == 1 ]]; then
    for copy in "$BUNDLE/Contents/Resources/libusb.dylib" "$BUNDLE/Contents/Resources/kalam.dylib" "$BUNDLE/Contents/MacOS/mtp-worker" "$BUNDLE/Contents/MacOS/app-gpui"; do
        chmod u+w "$copy"
        /usr/bin/codesign --force --sign - "$copy"
    done
    /usr/bin/codesign --force --sign - "$BUNDLE"
    /usr/bin/codesign --verify --deep --strict "$BUNDLE"
fi
# Keep final payload hashes outside signed Resources so no sealed file is
# mutated after signing. Paths are local developer provenance, not device IDs.
/usr/bin/shasum -a 256 "$BUNDLE/Contents/MacOS/app-gpui" "$BUNDLE/Contents/MacOS/mtp-worker" "$BUNDLE/Contents/Resources/kalam.dylib" "$BUNDLE/Contents/Resources/libusb.dylib" > "$BUNDLE.payload-sha256.txt"
if [[ $MAKE_ZIP == 1 ]]; then
    ZIP="$BUNDLE.zip"
    [[ ! -e "$ZIP" && ! -L "$ZIP" ]] || fail 'ZIP output already exists'
    /usr/bin/ditto -c -k --sequesterRsrc --keepParent "$BUNDLE" "$ZIP"
    printf 'LOCAL ONLY ZIP: %s\n' "$ZIP"
fi
printf 'LOCAL bundle ready (not formally released or device-validated): %s\n' "$BUNDLE"
printf 'Finder launch: open "%s"\n' "$BUNDLE"
if [[ $AD_HOC_SIGN == 0 ]]; then printf 'Bundle not explicitly signed; --ad-hoc-sign is available for local testing.\n'; fi
