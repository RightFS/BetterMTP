# Kalam native provenance (local developer use only)

The initial Go files were copied from the read-only sibling OpenMTP checkout,
`../openmtp/ffi/kalam/native`, after reading the original project LICENSE and
every copied source file. Upstream project: https://github.com/ganeshrvel/openmtp.
The original MIT notice is preserved verbatim in [LICENSE](LICENSE):
**Copyright (c) 2010-present Ganesh Rathinavel**.

Copied Go source groups: top-level Kalam exports/helpers/input structs/utilities,
[send_to_js](send_to_js/main.go) and its JSON/error helpers, and the original
[debug report](kalam_debug_report/main.go). The debug report retains its upstream
`!go1.17` build restriction, so it is not part of the modern shared build.
The Node `.mjs` scripts and prebuilt binaries were NOT copied into this source tree.
This project's Bash builder is a new implementation, informed by the old
`build-arm-seg5` script's libusb install-name and Go >=1.26 chained-fixups findings.
The sibling project was not modified.

Local changes: operation-wide mutex, deterministic synchronous callbacks,
CString ownership, unconditional container clearing, SafetyVersion marker,
fail-closed legacy destructive writes, descriptor-only single upload/download,
random final KeepBoth names, metadata/count checks, and no-device tests.

## Dependencies and redistribution stop

[go.mod](go.mod) and [go.sum](go.sum) retain these pinned upstream versions:

- go-mtpx: `v0.0.0-20240426092756-18f12db021cc`
- go-mtpfs: `v1.0.4-0.20240426083057-1c3302b3c476`
- ganeshrvel/usb: `v0.0.0-20210103155855-14d96f5ae403` (transitive)
- json-iterator/go: `v1.1.12`

The downloaded go-mtpx module has **no LICENSE file**; authorization is NOT
confirmed. OpenMTP's MIT license cannot be presumed to license independently
published dependency modules. **This is a local developer build, NOT an official
redistributable build**. Do not package/upload the native artifacts pending an
explicit dependency/license review (including go-mtpfs, usb and libusb terms).
No local dependency fork or blanket license substitution was made.

Detailed ABI, build commands and verified/unverified boundaries are in
[the native safety note](../../docs/kalam-native-safety.md).
