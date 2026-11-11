#!/usr/bin/env bash
set -euo pipefail
repo_root="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo_root"
: "${IOS_SIMULATOR_UDID:?Select an available iPhone simulator UUID}"

# Production build checks the committed sources and the exported symbol table.
scripts/build-xcframework.sh --check
run_smoke() {
    local log status
    log="$(mktemp "$repo_root/target/apple-test-log.XXXXXXXX")"
    status=0
    "$@" >"$log" 2>&1 || status=$?
    if grep -q ER_FFI_PRIVATE_SENTINEL "$log"; then
        rm -f "$log"
        echo "Private sentinel appeared in Apple test output; output withheld." >&2
        return 1
    fi
    cat "$log"
    rm -f "$log"
    return "$status"
}
project="$repo_root/ios/ErNote.xcodeproj"
destination="platform=iOS Simulator,id=$IOS_SIMULATOR_UDID"
for configuration in Debug Release; do
    run_smoke xcodebuild -project "$project" -scheme ErNoteSmoke \
        -configuration "$configuration" -destination "$destination" \
        -derivedDataPath "$repo_root/target/apple-smoke/production-$configuration" \
        CODE_SIGNING_ALLOWED=NO test
done

# The panic API never replaces the production package, even temporarily.
test_root="$(mktemp -d "$repo_root/target/apple-panic-smoke.XXXXXXXX")"
trap 'rm -rf "$test_root"' EXIT
mkdir "$test_root/ios"
for item in ErNote.xcodeproj Config ErNote ErNoteWatch ErNoteWidget ErNoteTests; do
    cp -R "$repo_root/ios/$item" "$test_root/ios/$item"
done
scripts/build-xcframework.sh --panic-test --output "$test_root/ios/ErNoteKit"
# Disable debugger attachment so the deliberate Rust panic can unwind normally.
python3 - "$test_root/ios/ErNote.xcodeproj/xcshareddata/xcschemes/ErNoteSmoke.xcscheme" <<'PY'
import sys
import xml.etree.ElementTree as ET
path = sys.argv[1]
tree = ET.parse(path)
action = tree.getroot().find("TestAction")
action.set("selectedDebuggerIdentifier", "")
action.set("selectedLauncherIdentifier", "Xcode.IDEFoundation.Launcher.PosixSpawn")
tree.write(path, encoding="UTF-8", xml_declaration=True)
PY
run_smoke xcodebuild -project "$test_root/ios/ErNote.xcodeproj" -scheme ErNoteSmoke \
    -configuration Release -destination "$destination" \
    -derivedDataPath "$repo_root/target/apple-smoke/panic-test" \
    'SWIFT_ACTIVE_COMPILATION_CONDITIONS=$(inherited) ER_FFI_PANIC_TEST' \
    -only-testing:ErNoteTests/BridgeSmokeTests CODE_SIGNING_ALLOWED=NO test
