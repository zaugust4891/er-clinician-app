#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"

if [[ "$(uname -s)" != Darwin || "$(uname -m)" != arm64 ]]; then
    echo "Run this check on the Apple Silicon development Mac." >&2
    exit 1
fi

: "${DEVELOPER_DIR:?Select the intended Xcode Developer directory}"
: "${EXPECTED_XCODE_VERSION:?Set the exact selected Xcode version}"
: "${EXPECTED_XCODE_BUILD:?Set the exact selected Xcode build}"
: "${IOS_SIMULATOR_UDID:?Select an available iPhone simulator UUID}"
export DEVELOPER_DIR

expected_xcode="$(printf 'Xcode %s\nBuild version %s' "$EXPECTED_XCODE_VERSION" "$EXPECTED_XCODE_BUILD")"
actual_xcode="$(xcodebuild -version)"
if [[ "$actual_xcode" != "$expected_xcode" ]]; then
    echo "Selected Xcode does not match the expected version/build." >&2
    exit 1
fi
printf '%s\n' "$actual_xcode"
xcodebuild -showsdks

rust_toolchain="$(sed -n 's/^channel = "\(.*\)"/\1/p' rust-toolchain.toml)"
if [[ -z "$rust_toolchain" ]]; then
    echo "Cannot read the repository Rust toolchain pin." >&2
    exit 1
fi
rustup run "$rust_toolchain" rustc --version
installed_targets="$(rustup target list --installed --toolchain "$rust_toolchain")"
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
    if ! grep -Fxq "$target" <<< "$installed_targets"; then
        echo "Missing Rust target: $target ($rust_toolchain)." >&2
        exit 1
    fi
done

deployment_target="$(sed -n 's/^IPHONEOS_DEPLOYMENT_TARGET = //p' ios/Config/Shared.xcconfig)"
if [[ -z "$deployment_target" ]]; then
    echo "Cannot read the iOS deployment floor." >&2
    exit 1
fi
export IPHONEOS_DEPLOYMENT_TARGET="$deployment_target"
probe_output="$repo_root/target/apple-probe"
for target in aarch64-apple-ios aarch64-apple-ios-sim; do
    mkdir -p "$probe_output/$target"
    rustup run "$rust_toolchain" rustc \
        --edition 2024 --crate-name er_toolchain_probe --crate-type staticlib \
        --target "$target" -C panic=unwind -C opt-level=2 \
        -o "$probe_output/$target/liber_toolchain_probe.a" \
        ios/ToolchainProbe/lib.rs
done

xcrun swift test --package-path ios/ErNoteKit
xcrun swift build --package-path ios/ErNoteKit --configuration release

project="$repo_root/ios/ErNote.xcodeproj"
derived_data="$repo_root/ios/DerivedData"
for scheme in ErNote ErNoteWatch ErNoteWidget; do
    destination="generic/platform=iOS Simulator"
    if [[ "$scheme" == ErNoteWatch ]]; then
        destination="generic/platform=watchOS Simulator"
    fi
    for configuration in Debug Release; do
        xcodebuild -project "$project" -scheme "$scheme" \
            -configuration "$configuration" -destination "$destination" \
            -derivedDataPath "$derived_data" CODE_SIGNING_ALLOWED=NO build
    done
done

xcodebuild -project "$project" -scheme ErNoteSmoke -configuration Debug \
    -destination "platform=iOS Simulator,id=$IOS_SIMULATOR_UDID" \
    -derivedDataPath "$derived_data" CODE_SIGNING_ALLOWED=NO test
