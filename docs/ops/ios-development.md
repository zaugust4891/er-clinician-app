# Phase 5 Apple development setup and implementation

## Current status

Step 1 bridge sources, generated bindings, packaging, and CI wiring are now
implemented. See the [Step 1 implementation record](phase5-step1-implementation.md)
for the API, build commands, verified Linux results, and outstanding Apple gates.
Build the ignored XCFramework before opening the phone project from a clean checkout.

Part 0 source preparation is underway on Linux. SwiftUI compilation,
simulator execution, signing, and physical-device installation have not
been verified. The Xcode project and shared schemes now wire the source
files into phone, watch, widget, and app-hosted test targets. These targets
have not yet been built with Xcode.

The existing M1 MacBook Air is the Apple development machine. Linux
remains the Rust/backend development host. AWS staging is not required
for the synthetic workflow or the minimal Step 1 bridge.

| Item | Recorded status |
| --- | --- |
| Rust | Repository pin: 1.97.1 |
| Swift package manifest | Tools version 5.9; not an installed Xcode version |
| Swift language mode | Project configuration: Swift 5 |
| Deployment floors | Proposed iOS 17 / watchOS 10; device validation pending |
| macOS / Xcode version and build | Must be recorded from the M1 |
| iOS / watchOS SDK versions | Must be recorded from the selected Xcode |
| Apple Developer organization / Account Holder | Business ownership confirmation pending |
| Team and registered bundle identifiers | Pending; checked-in identifiers are placeholders |
| Signing keys and profiles | Ownership and secure storage location pending |
| Physical iPhone and Apple Watch | Models, OS versions, and access confirmation pending |

Zach owns the Mac, enrollment, and hardware follow-up. Do not record
private keys, credentials, or portal-only device identifiers here.
Reserve the intended bundle identifiers and future App Group under the
confirmed business team before provisioning.

## Implemented source files

| Files | Responsibility |
| --- | --- |
| `ios/ErNoteKit/Sources/ErNoteKit/DeviceService.swift` | Presentation records and explicit-patient commands. |
| `ios/ErNoteKit/Sources/ErNoteKit/FixtureDeviceService.swift` | Debug-only, in-memory synthetic data and preview states. |
| `ios/ErNoteKit/Sources/ErNoteKit/UnavailableDeviceService.swift` | Release fallback; commands throw unavailable instead of simulating persistence. |
| `ios/ErNote/ErNoteApp.swift` | Selects the service and prepares the protected storage directory. |
| `ios/ErNote/WorkspaceModel.swift` | Publishes snapshots, handles commands, and retains failed note edits per patient. |
| `ios/ErNote/WorkspaceView.swift` | Shift/patient navigation, Record/Note/Chart surfaces, manual context, and Debug preview controls. |
| `ios/ErNote/NoteEditor.swift` | Editable note binding and failed-save retry. |
| `ios/ErNote/PreferencesView.swift` | Creates and edits preference/prompt definitions without executing them. |
| `ios/ErNote/ProtectedStorage.swift` | Prepares a private database directory before returning its path. |
| `ios/ErNoteWatch/ErNoteWatchApp.swift` | Watch availability shell; no patient data or recorder controls. |
| `ios/ErNoteWidget/ErNoteWidget.swift` | Static widget shell; no clinical storage or patient information. |
| `ios/ErNoteTests/WorkspaceModelTests.swift` | App-hosted model failure/retry, preview, and storage-attribute tests. |
| `ios/Config/Shared.xcconfig` | Deployment, version, and placeholder signing configuration. |
| `ios/ErNote.xcodeproj/` | Four targets, shared schemes, local package linkage, and embedded watch/widget products. |
| `ios/ToolchainProbe/lib.rs` | Application-independent Rust static-library cross-compilation probe. |
| `scripts/check-apple-shell.sh` | Explicit toolchain checks, two Rust probe builds, Swift package checks, simulator shell builds, and app-hosted tests. |
| Target `Info.plist` files | Phone permission/background declarations, watch companion identity, and widget extension type. |
| `ios/Config/PrivacyInfo.xcprivacy` | Current shell privacy declarations; must be bundled into each application/extension target. |
| `.gitignore` | Excludes Apple build output, user settings, and local signing overrides. |

## Data flow and manual editing

A screen sends an explicit patient UUID to WorkspaceModel. The model
calls DeviceService and publishes its resulting snapshot. SwiftUI reads
that snapshot; it does not implement the Rust note state machine.

Note editing first retains the text in a patient-keyed draft dictionary,
then attempts the service command. Success refreshes the snapshot and
clears the retained draft. Failure leaves the text and an error available
for retry. Refreshing or selecting another patient does not discard the
draft. Empty text is an intentional edit, not a missing draft.

Consent, recording, generation previews, and stale results never disable
the note editor. Separate discharge previews do not replace note text.
All fixture data and retained drafts disappear when the process ends.
Step 2 must supply durable storage and manual revision preservation.

Preferences currently reuse the existing presentation record for both
section instructions and named saved prompts. They have no versioned
storage, generation execution, or enforced default-selection policy.

## Storage boundary

The phone prepares `Application Support/ClinicalData` with
`completeUntilFirstUserAuthentication` protection and backup exclusion
before returning `device.sqlite`. Directory setup errors propagate to the
app, which displays a generic failure without exposing paths.

This bootstrap creates no database. Future SQLite WAL/SHM siblings must
remain in the same directory. The clinical database is not in an App
Group; neither extension opens it. Step 2 must handle file creation and
migrations. Step 9 must verify actual files, backup behavior, and locked
device behavior on hardware.

The background-audio declaration and microphone purpose text do not
activate recording or request microphone permission.

## Project wiring and Mac checks

The phone links the local ErNoteKit product and embeds the watch app and
widget. The app-hosted test target links ErNoteKit and loads the phone
executable. Watch and widget targets link neither ErNoteKit nor Rust.
The privacy manifest is copied into all three shipping bundles.

Shared schemes are ErNote, ErNoteWatch, ErNoteWidget, and ErNoteSmoke.
ErNoteSmoke runs WorkspaceModelTests inside the phone app. The widget
scheme is for building the extension; it does not launch a standalone app.
Debug enables fixtures; Release compiles the unavailable service path.

On the M1, explicitly set DEVELOPER_DIR to the selected Xcode Developer
directory. Read its version/build using xcodebuild -version and record
them in the setup table. Set EXPECTED_XCODE_VERSION and
EXPECTED_XCODE_BUILD to those exact values. Select an available iPhone
simulator from xcrun simctl list devices available and set its UUID in
IOS_SIMULATOR_UDID. Then run:

    bash scripts/check-apple-shell.sh

The script rejects unsupported hosts, mismatched Xcode versions, and
missing Rust targets. It reads the existing Rust pin and iOS deployment
floor, builds separate device and arm64 simulator static-library probes,
builds the real Step 1 XCFramework, builds Debug and Release simulator shells,
and runs app-hosted tests (including the fixture package tests). ErNoteKit now
contains iOS-only binary slices, so host macOS `swift test` is unsupported.
Output goes to ignored target/apple-probe, target/apple-bridge,
ios/ErNoteKit/Artifacts, SwiftPM build directories, and ios/DerivedData.

The Rust probe is not linked into the phone and proves no application
dependency compatibility. The Step 1 bridge is linked into the phone;
run `scripts/check-bridge-apple.sh` to additionally check production Release
linking and the isolated optimized panic artifact. Step 2 repeats the Apple
build with the full device graph and bundled SQLite.

The Mac toolchain pin remains an external setup gate until actual values
are recorded; requiring explicit expected inputs does not establish which
Xcode release is installed. Local.xcconfig is for team/bundle identifier
overrides. Keep deployment floors in Shared.xcconfig so the probe and
project use the same values. No signed-device check runs in this script.

## Privacy and validation

The current shells contain no tracking, network collection, or declared
required-reason API use. Re-audit the manifests when Rust dependencies,
storage, capture, authentication, or telemetry are integrated. The final
Apple build and privacy report remain verification gates.

Linux checks do not establish Swift compilation or test success.
On the Mac, run the app-hosted simulator checks; these include the package
fixture tests. A host-only Swift package test cannot validate an iOS XCFramework.

Before selecting Xcode, verify compatibility with the installed macOS.
Record the exact Xcode build and SDKs rather than silently selecting
whichever installation happens to be active. Preserve the Rust pin.
Install `aarch64-apple-ios` and `aarch64-apple-ios-sim` on the Mac.
No Rust watchOS target is required.

Remaining Part 0 gates include running the Apple probe and simulator
checks, recording the verified toolchain, confirming accounts/identifiers
and hardware, and signed device installation. Step 1 has added the pinned UniFFI
runtime and generator, sanitized errors, panic tests, generated bindings,
XCFramework packaging, dependency checks, and Apple CI source configuration.
Actual Apple execution and signed-phone evidence remain outstanding.

References:
- [Apple backup exclusion](https://developer.apple.com/documentation/foundation/optimizing-your-app-s-data-for-icloud-backup)
- [Apple privacy manifests](https://developer.apple.com/documentation/bundleresources/privacy-manifest-files)
- [Single-target watch apps](https://developer.apple.com/documentation/watchos-apps/migrating-to-a-single-target-watchos-app)
