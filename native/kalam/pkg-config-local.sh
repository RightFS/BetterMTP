#!/bin/bash
# Minimal cgo pkg-config adapter ONLY for explicitly supplied local libusb.
# Not a replacement for a general pkg-config installation.
set -euo pipefail
: "${KALAM_LIBUSB_INCLUDE:?}" "${KALAM_LIBUSB_LINK_DIR:?}"
case "$KALAM_LIBUSB_INCLUDE$KALAM_LIBUSB_LINK_DIR" in *[[:space:]]*) echo 'local libusb paths must not contain whitespace' >&2; exit 2;; esac
cflags=0; libs=0; package=0
for arg in "$@"; do
 case "$arg" in
 --cflags) cflags=1;;
 --libs) libs=1;;
 --|--static|--print-errors|--short-errors) ;;
 libusb-1.0) package=1;;
 *) echo "unsupported local pkg-config query: $arg" >&2; exit 2;;
 esac
done
[[ $package == 1 ]] || { echo 'only libusb-1.0 is supported' >&2; exit 2; }
[[ $cflags == 0 ]] || printf '%s ' "-I$KALAM_LIBUSB_INCLUDE"
[[ $libs == 0 ]] || printf '%s ' "-L$KALAM_LIBUSB_LINK_DIR" '-lusb-1.0'
printf '\n'
