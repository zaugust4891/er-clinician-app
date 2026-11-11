# Phase 5 Step 1 implementation: Rust–Swift bridge

Implemented November 11, 2026. The minimal bridge, generated production sources,
Apple packaging scripts, hosted smoke tests, and CI jobs are present. Linux
verification is recorded below. **Apple builds, simulator execution, hosted CI
execution, and signed physical-phone acceptance remain unverified.** This record
does not close the complete Step 1 acceptance gate.

## Scope and public contract

`er-ffi` is a new workspace crate producing a Rust library, static library, and
dynamic library. It depends only on UniFFI and `thiserror`; it does not yet attach
`er-device`, application orchestration, SQLite, network clients, or providers.
There are no new credentials, server requirements, deployments, or account changes.

The workspace pins UniFFI to **0.32.2** with default features disabled. The separate
`er-bindgen` binary enables its CLI/Cargo-metadata features using the same pin.
The lockfile includes both crates and their dependencies. The existing Rust
**1.97.1** pin is unchanged.

| Rust API | Generated Swift API | Behavior |
| --- | --- | --- |
| `ping()` | `try ping()` | Returns `"pong"`; initializes the safe panic hook. |
| `DeviceConfig` | `DeviceConfig(databasePath:accountScope:)` | Immutable Swift record carrying a local database path and opaque account scope. |
| `validate_config(config)` | `try validateConfig(config:)` | Validates transport fields and returns the same record without I/O. |
| `panic_for_test()` | `try panicForTest()` | Deliberately panics; available only with the explicit `ffi-test` Cargo feature. |

Config validation requires an absolute path, no NUL, and at most 4096 UTF-8 bytes.
Scope must contain non-whitespace content, have no control characters, and fit
within 256 UTF-8 bytes. This does not verify path accessibility, create a database,
authenticate the scope, or establish durable scope binding. Those are Step 2 work.
The config deliberately has no `Debug` implementation that could print its fields.

`FfiError` has stable cases for `InvalidInput`, `InvalidState`, `NotFound`,
`Storage`, `Closed`, `Busy`, and `Internal`. Errors carry no arbitrary strings.
Rust display messages are fixed codes such as `ER_INVALID_INPUT` and `ER_INTERNAL`;
Swift receives the declared enum case. Future domain/storage errors must map by
category, without copying underlying error messages.

## Panic and lifetime contract

Every handwritten export calls `ffi_guard`. It installs a process-wide no-output
panic hook exactly once, then uses `catch_unwind` to map panics to `Internal`.
The hook deliberately does not chain the previous hook, which might print private
payloads, source paths, or backtraces. The phone composition root calls the health
path at launch. Other Rust code must not subsequently replace this hook.

The guard forgets a caught panic payload: an arbitrary payload could itself panic
when dropped, bypassing the typed mapping. This intentionally leaks that exceptional
payload rather than attempting unsafe recovery. Aborts, allocation failure, and
foreign traps are outside this guarantee.

The root `ios-release` profile inherits `release`, uses `panic = "unwind"`, retains
line-table debug information, and does not strip symbols. `er-ffi` also has a
compile-time rejection for any effective panic strategy other than unwind. The
build scripts reject profile overrides, Rust flag overrides, and compiler wrappers.

There are currently no stateful handles. Step 2 must fault a handle after a command
panic, reject further mutations, and require explicit close/reopen and replay.
Do not recover a poisoned lock and continue. A successful `ping` after a panic
only proves that this stateless bridge remains callable.

## Sources and packaging

| Files | Responsibility |
| --- | --- |
| `crates/er-ffi/` | Minimal API, guard, fixed errors, tests, and per-crate UniFFI naming config. |
| `crates/er-bindgen/` | Repository-owned generator entry point. |
| `ios/ErNoteKit/Sources/ErNoteKit/Generated/RustBridge.swift` | Generated production Swift wrapper and ABI checksums. |
| `ios/ErNoteKit/Generated/` | Generated C header and canonical `module.modulemap`. |
| `ios/ErNoteKit/Package.swift` | iOS 17 library wrapping the `RustCoreFFI` binary target at `Artifacts/RustCore.xcframework`. |
| `scripts/apple_bridge.py` | Shared generation, dependency inspection, toolchain validation, packaging, and artifact publication. Requires Python 3.11+. |
| `scripts/generate-swift-bindings.sh` | Regenerate production sources or reject drift. |
| `scripts/build-xcframework.sh` | Sole XCFramework build entry point. |
| `scripts/check-device-deps.sh` | Inspect each Apple target's resolved normal graph separately from host tools. |
| `scripts/check-bridge-linux.sh` | Linux bridge validation entry point. |
| `scripts/check-bridge-apple.sh` | Production and isolated panic-artifact simulator validation. |
| `scripts/test_apple_bridge.py` | Packaging failure tests and optimized generated-Python FFI smoke. |

The Swift module is `ErNoteKit`; the generated wrapper filename is `RustBridge.swift`
and its C module is `RustCoreFFI`. Watch and widget targets have no package or Rust
dependency. ErNoteKit now supports iOS only: plain host `swift test` cannot link an
XCFramework containing only iOS slices. Existing fixture tests are also included in
the phone-hosted XCTest target.

Generation reads `crates/er-ffi/uniffi.toml` through Cargo metadata. UniFFI 0.32's
CLI `--config` option expects a separate global configuration, so it is intentionally
not pointed at that per-crate file. Generated whitespace is normalized consistently;
there is no dependency on a globally installed Swift formatter or bindgen binary.

On Linux or macOS, run:

```bash
scripts/generate-swift-bindings.sh --check
# Only when deliberately changing the public API or generator:
scripts/generate-swift-bindings.sh --write
scripts/check-bridge-linux.sh
```

`--write` invalidates an existing ignored `Artifacts` directory before updating
source bindings. Rebuild the XCFramework before opening/building the phone again.
The normal Apple build checks committed bindings; it never silently replaces them.

The Apple builder:

1. Requires an arm64 Mac, an explicitly selected/matching Xcode version/build,
   both Rust Apple targets, and the deployment floor from `Shared.xcconfig`.
2. Builds the pinned host generator and metadata-bearing library with `--locked`,
   using separate generator, production, and panic-test target directories.
3. Builds optimized `aarch64-apple-ios` and `aarch64-apple-ios-sim` static libraries
   with the same explicit feature set. It never uses workspace/all-features builds.
4. Regenerates from each actual Apple archive and compares its Swift/header/module
   output with the host metadata output, rejecting target-dependent API drift.
5. Checks production bindings and library symbols for the test export, and library
   bytes for the private test sentinel.
6. Packages distinct device and simulator libraries with matching headers using
   `xcodebuild -create-xcframework`; it does not combine them with `lipo`.
7. Writes `Artifacts/build-manifest.json`, recording commit/dirty state, lockfile
   hash, UniFFI version, generator hash, Rust/Xcode/macOS/SDK versions, profile,
   features, deployment target, target triples, generated-source hashes, and
   XCFramework file hashes.
8. Publishes the complete ignored artifact directory only after success. Ordinary
   rename failure restores the previous directory. An interrupted publication
   leaving `Artifacts.previous` requires inspection; the next build refuses to
   overwrite it. Compiler intermediates and unstripped libraries stay under
   `target/apple-bridge` for local diagnostics.

The separate panic-test build requires a new output directory under `target/`.
It writes a complete matching package there and never changes production sources
or the production framework. The Apple checker copies the phone project into an
isolated tree, supplies this package, enables `ER_FFI_PANIC_TEST`, and runs the
panic XCTest with Release Swift and optimized Rust. Debugger attachment is disabled
for that run. Test output is checked for the synthetic private sentinel before
being printed.

## Phone integration and CI

`BridgeHealth.check()` runs the real Rust health call at app initialization and
surfaces a fixed, generic message if it fails. `validateLocalConfiguration` supplies
the config conversion path. Neither grants recording or persistence capabilities:
Debug still uses synthetic fixtures, and Release still uses the unavailable
device service until Step 2.

`BridgeSmokeTests` checks the real package import/link, health call, config
round-trip, typed invalid input, and—in the isolated artifact—typed panic recovery
followed by another successful health call. The existing workspace-model and
fixture tests run in Debug. The production bridge also runs in Release.

CI now has required Linux and Apple bridge jobs. The Apple job selects
`macos-26` and Xcode **26.6 / 17F113** explicitly, installs the pinned Rust Apple
targets, selects an available iPhone simulator, and runs the Apple checker. This
selection follows the [published runner image inventory](https://github.com/actions/runner-images/blob/main/images/macos/macos-26-arm64-Readme.md);
it is not evidence of a successful run or the version installed on the M1.
Missing Xcode/runtimes fail the gate; no `continue-on-error` or signing secrets are
introduced. Both bridge jobs participate in the existing Required CI result.
The pinned actionlint 1.7.7 predates the `macos-26` label; `.github/actionlint.yaml`
allows that exact hosted label without disabling runner-label validation generally.

On the M1, follow [Apple setup](ios-development.md), record the actual selected
Xcode/version/build, and install the two Rust targets. Then set `DEVELOPER_DIR`,
`EXPECTED_XCODE_VERSION`, `EXPECTED_XCODE_BUILD`, and `IOS_SIMULATOR_UDID` and run:

```bash
scripts/check-bridge-apple.sh
```

The older `check-apple-shell.sh` now builds the real production framework before
building shells and running hosted tests. It no longer runs unsupported host-only
Swift package tests. Use the bridge checker for the optimized panic gate.

## Verification record and remaining gates

Linux evidence from this implementation session:

- Rust bridge tests: five pass with default features and with `ffi-test`.
- Package-specific Clippy with warnings denied and workspace formatting pass.
- Both Apple target runtime graphs exclude backend/provider/generator packages
  and cloud STT/FFI tracing features; host tooling is inspected separately.
- Production binding regeneration matches the checked-in source; production
  host symbols and bytes contain neither the deliberate panic export nor sentinel.
- Four packaging-policy tests pass: drift rejection, forbidden runtime dependencies,
  unsafe build overrides, and restoration of a previous artifact on publish failure.
- Generated Python calls against the actual `ios-release` host library pass config,
  typed validation, deliberate panic, subsequent health, and output-sanitization checks.
- A direct `cargo rustc ... -- -C panic=abort` build fails with the intended
  `er-ffi requires panic=unwind` diagnostic.
- Apple packaging rejects this Linux host before modifying package artifacts.
- Static-library-only cross-compilation succeeds for both Apple target triples
  with the pinned toolchain and optimized unwind profile. Rust reports the native
  link requirements `-lSystem -lc -lm`. Both actual Apple archives regenerate the
  same production bindings as the host library. This does not link or run an app.
- The repository-pinned actionlint, workflow YAML parsing, shell syntax,
  Xcode project object/reference checks, and whitespace checks pass.

`scripts/ci-services.sh` passes in its disposable local service environment:
workspace tests and Clippy, OpenAPI consistency, standalone AI regression checks,
online SQLx metadata freshness, deliberate stale-metadata rejection, and readiness
failure for each missing Postgres/MinIO/ElasticMQ service. The optional ignored
load gate was not enabled. Local logs are `/tmp/er-note-phase5-step1-regression.log`
and `/tmp/er-note-phase5-step1-bridge.log`; these are ephemeral evidence, not
checked-in artifacts or a claim that hosted CI has executed.

Outstanding acceptance: device and simulator XCFramework creation, Swift/Xcode
compilation, optimized simulator throwing behavior, hosted CI execution, and a signed
physical iPhone smoke. No Mac/Swift toolchain is installed on this host. The source
implementation and Linux checks cannot establish those results. Part 0 enrollment,
signing, registered identifiers, and hardware records remain separate setup gates.

Step 2 should attach the durable device core, implement scope binding and faulted
handle recovery, and repeat dependency/build checks with the actual SQLite graph.
No recording, sync, local AI, or persistent clinical command implementation was
added in Step 1.

References: [UniFFI generation](https://mozilla.github.io/uniffi-rs/latest/tutorial/foreign_language_bindings.html),
[Swift configuration](https://mozilla.github.io/uniffi-rs/latest/swift/configuration.html),
and [UniFFI panic behavior](https://mozilla.github.io/uniffi-rs/latest/internals/rust_calls.html).
