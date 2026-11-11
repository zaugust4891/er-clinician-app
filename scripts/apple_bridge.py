#!/usr/bin/env python3
"""Locked bridge generation/packaging. Python 3.11+, no third-party modules."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import platform
import re
import shutil
import subprocess
import tempfile
import tomllib

ROOT = Path(__file__).resolve().parents[1]
PACKAGE = ROOT / "ios/ErNoteKit"
PROFILE = "ios-release"
TARGETS = ("aarch64-apple-ios", "aarch64-apple-ios-sim")
GENERATED = {
    "RustBridge.swift": "Sources/ErNoteKit/Generated/RustBridge.swift",
    "RustCoreFFI.h": "Generated/RustCoreFFI.h",
    "RustCoreFFI.modulemap": "Generated/module.modulemap",
}


def run(*args, capture=False):
    print("+", " ".join(map(str, args)), flush=True)
    result = subprocess.run(list(map(str, args)), cwd=ROOT, check=True,
                            text=True, stdout=subprocess.PIPE if capture else None)
    return result.stdout.strip() if capture else None


def cargo(*args, **kwargs):
    toolchain = tomllib.loads((ROOT / "rust-toolchain.toml").read_text())["toolchain"]["channel"]
    return run("cargo", f"+{toolchain}", *args, **kwargs)


def validate_environment():
    # Reject overrides instead of silently changing the shipped panic/profile contract.
    overrides = [key for key in os.environ if key.startswith("CARGO_PROFILE_IOS_RELEASE_")]
    if overrides or any(os.environ.get(key) for key in
                        ("RUSTFLAGS", "CARGO_ENCODED_RUSTFLAGS", "RUSTC_WRAPPER", "RUSTC_WORKSPACE_WRAPPER")):
        raise SystemExit("Remove Rust flag/wrapper/ios-release profile overrides for bridge builds")
    manifest = tomllib.loads((ROOT / "Cargo.toml").read_text())
    profile = manifest["profile"][PROFILE]
    if profile["inherits"] != "release" or profile["panic"] != "unwind":
        raise SystemExit("ios-release must inherit release and use panic=unwind")
    return manifest["workspace"]["dependencies"]["uniffi"]["version"].removeprefix("=")


def paths(panic_test):
    # Explicit, separate target dirs also avoid workspace feature unification.
    base = ROOT / "target/apple-bridge"
    return base / ("panic-test" if panic_test else "production"), base / "generator"


def build_host(panic_test):
    build, generator = paths(panic_test)
    host = run("rustc", "-vV", capture=True).split("host: ")[1].splitlines()[0]
    cargo("build", "--locked", "-p", "er-bindgen", "--target-dir", generator, "--target", host)
    build_library(build, host, panic_test)
    suffix = "dylib" if platform.system() == "Darwin" else "so"
    return generator / host / "debug/er-bindgen", build / host / PROFILE / f"liber_ffi.{suffix}"


def build_library(build, target, panic_test):
    args = ["build", "--locked", "-p", "er-ffi", "--lib", "--no-default-features",
            "--profile", PROFILE, "--target-dir", build, "--target", target]
    if panic_test:
        args += ["--features", "ffi-test"]
    cargo(*args)


def generate(generator, library, destination):
    destination.mkdir(parents=True)
    run(generator, "generate", "--library", library, "--language", "swift",
        "--out-dir", destination, "--no-format")
    # UniFFI reads crates/er-ffi/uniffi.toml through Cargo metadata. Its --config
    # option is for global configuration, not that per-crate file in 0.32.
    # Canonical whitespace avoids depending on a host-installed Swift formatter.
    for path in destination.iterdir():
        path.write_text("\n".join(line.rstrip() for line in path.read_text().splitlines()) + "\n")


def check_generated(generated, package=PACKAGE):
    for name, relative in GENERATED.items():
        checked_in = package / relative
        if not checked_in.exists() or (generated / name).read_bytes() != checked_in.read_bytes():
            raise SystemExit(f"Generated source drift: {relative}; run scripts/generate-swift-bindings.sh --write")


def copy_generated(generated, package):
    for name, relative in GENERATED.items():
        destination = package / relative
        destination.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(generated / name, destination)


def assert_production(generated, libraries):
    for path in generated.iterdir():
        if re.search(r"panic_?for_?test|ER_FFI_PRIVATE_SENTINEL", path.read_text(), re.I):
            raise SystemExit("Test declarations found in production bindings")
    for library in libraries:
        symbols = run("nm", "-g", library, capture=True)
        if "panic_for_test" in symbols.lower():
            raise SystemExit("Test export found in production library")
        if b"ER_FFI_PRIVATE_SENTINEL" in library.read_bytes():
            raise SystemExit("Test sentinel found in production library")


def dependencies():
    forbidden = {"er-server", "er-ai", "axum", "sqlx", "uniffi_bindgen", "clap"}
    for target in TARGETS:
        graph = cargo("tree", "--locked", "-p", "er-ffi", "--no-default-features",
                      "--target", target, "-e", "normal", "--prefix", "none",
                      "--format", "{p} features=[{f}]", capture=True)
        for line in graph.splitlines():
            name = line.split()[0]
            if name in forbidden or name.startswith(("aws-", "sqlx-")):
                raise SystemExit(f"Forbidden device runtime dependency: {name}")
            features = line.split("features=[", 1)[1].split("]", 1)[0].split(",")
            if (name == "er-pipeline" and "cloud-stt" in features) or (
                    name.startswith("uniffi") and "ffi-trace" in features):
                raise SystemExit(f"Forbidden device feature in {name}")
        print(f"Device runtime graph passed: {target}")
    # Host-only macro/build/generator dependencies are deliberately separate.
    cargo("tree", "--locked", "-p", "er-ffi", "--target", TARGETS[0], "-e", "build")
    cargo("tree", "--locked", "-p", "er-bindgen", "--depth", "1", "-e", "normal,build")


def apple_environment():
    if platform.system() != "Darwin" or platform.machine() != "arm64":
        raise SystemExit("XCFramework packaging requires an Apple Silicon Mac with Xcode")
    required = ("DEVELOPER_DIR", "EXPECTED_XCODE_VERSION", "EXPECTED_XCODE_BUILD")
    if any(not os.environ.get(key) for key in required):
        raise SystemExit("Set DEVELOPER_DIR, EXPECTED_XCODE_VERSION and EXPECTED_XCODE_BUILD")
    xcode = run("xcodebuild", "-version", capture=True)
    expected = f"Xcode {os.environ['EXPECTED_XCODE_VERSION']}\nBuild version {os.environ['EXPECTED_XCODE_BUILD']}"
    if xcode != expected:
        raise SystemExit("Selected Xcode differs from the expected version/build")
    floor = re.search(r"^IPHONEOS_DEPLOYMENT_TARGET = ([0-9.]+)$",
                      (ROOT / "ios/Config/Shared.xcconfig").read_text(), re.M).group(1)
    if os.environ.get("IPHONEOS_DEPLOYMENT_TARGET", floor) != floor:
        raise SystemExit("Deployment target differs from Shared.xcconfig")
    os.environ["IPHONEOS_DEPLOYMENT_TARGET"] = floor
    installed = run("rustup", "target", "list", "--installed", capture=True).splitlines()
    if not set(TARGETS).issubset(installed):
        raise SystemExit("Install aarch64-apple-ios and aarch64-apple-ios-sim for the pinned Rust toolchain")
    return {"xcode": xcode, "macos": platform.mac_ver()[0], "deployment_target": floor,
            "sdk_device": run("xcrun", "--sdk", "iphoneos", "--show-sdk-version", capture=True),
            "sdk_simulator": run("xcrun", "--sdk", "iphonesimulator", "--show-sdk-version", capture=True)}


def sha256(path):
    with path.open("rb") as source:
        return hashlib.file_digest(source, "sha256").hexdigest()


def publish_directory(staged, destination):
    """Publish only complete output, restoring the previous version on rename failure."""
    backup = destination.with_name(destination.name + ".previous")
    if backup.exists():
        raise SystemExit(f"Previous interrupted publish needs inspection: {backup}")
    if destination.exists():
        destination.rename(backup)
    try:
        staged.rename(destination)
    except BaseException:
        if backup.exists():
            backup.rename(destination)
        raise
    if backup.exists():
        shutil.rmtree(backup)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("command", choices=("bindings", "build", "deps"))
    parser.add_argument("--write", action="store_true", help="Regenerate checked-in production sources")
    parser.add_argument("--check", action="store_true", help="Fail on checked-in source drift (default)")
    parser.add_argument("--panic-test", action="store_true", help="Isolated test package only")
    parser.add_argument("--output", type=Path, help="New isolated panic-test package directory")
    args = parser.parse_args()
    if args.write and (args.check or args.command != "bindings" or args.panic_test):
        parser.error("--write is only for production bindings")
    if args.panic_test != (args.output is not None) or (args.panic_test and args.command != "build"):
        parser.error("--panic-test requires build and --output")
    if args.output and (args.output.resolve().exists() or ROOT not in args.output.resolve().parents
                        or ROOT / "target" not in args.output.resolve().parents):
        parser.error("Panic-test output must be a new directory under repository target/")
    version = validate_environment()
    if args.command == "deps":
        dependencies()
        return
    apple = apple_environment() if args.command == "build" else None
    generator, library = build_host(args.panic_test)
    build, _ = paths(args.panic_test)
    with tempfile.TemporaryDirectory(prefix="bridge-stage-", dir=build) as temporary:
        stage = Path(temporary)
        generated = stage / "generated"
        generate(generator, library, generated)
        if not args.panic_test:
            assert_production(generated, [library])
        if args.command == "bindings":
            if args.write:
                # Invalidate old binaries before replacing sources so a failed
                # regeneration can never leave an apparently usable stale pair.
                artifacts = PACKAGE / "Artifacts"
                if artifacts.exists():
                    shutil.rmtree(artifacts)
                copy_generated(generated, PACKAGE)
            else:
                check_generated(generated)
            return
        if not args.panic_test:
            check_generated(generated)
        dependencies()
        libraries = []
        for target in TARGETS:
            build_library(build, target, args.panic_test)
            archive = build / target / PROFILE / "liber_ffi.a"
            libraries.append(archive)
            target_generated = stage / target
            generate(generator, archive, target_generated)
            for name in GENERATED:
                if (generated / name).read_bytes() != (target_generated / name).read_bytes():
                    raise SystemExit(f"Host/Apple public API mismatch: {target}/{name}")
        if not args.panic_test:
            assert_production(generated, libraries)
        headers = stage / "headers"
        headers.mkdir()
        shutil.copy2(generated / "RustCoreFFI.h", headers)
        shutil.copy2(generated / "RustCoreFFI.modulemap", headers / "module.modulemap")
        artifacts = stage / "Artifacts"
        artifacts.mkdir()
        run("xcodebuild", "-create-xcframework", "-library", libraries[0], "-headers", headers,
            "-library", libraries[1], "-headers", headers, "-output", artifacts / "RustCore.xcframework")
        manifest = dict(apple, commit=run("git", "rev-parse", "HEAD", capture=True),
                        source_dirty=bool(run("git", "status", "--porcelain", capture=True)),
                        lockfile_sha256=sha256(ROOT / "Cargo.lock"), uniffi=version,
                        generator_sha256=sha256(generator), rust=run("rustc", "-vV", capture=True),
                        profile=PROFILE, features=["ffi-test"] if args.panic_test else [],
                        targets=list(TARGETS), artifacts={}, sources={})
        for path in sorted(artifacts.rglob("*")):
            if path.is_file():
                manifest["artifacts"][str(path.relative_to(artifacts))] = sha256(path)
        for name in GENERATED:
            manifest["sources"][name] = sha256(generated / name)
        (artifacts / "build-manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
        if args.panic_test:
            package = stage / "ErNoteKit"
            shutil.copytree(PACKAGE, package, ignore=shutil.ignore_patterns("Artifacts", ".build", ".swiftpm"))
            copy_generated(generated, package)
            artifacts.rename(package / "Artifacts")
            args.output.parent.mkdir(parents=True, exist_ok=True)
            package.rename(args.output)
        else:
            publish_directory(artifacts, PACKAGE / "Artifacts")


if __name__ == "__main__":
    try:
        main()
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.returncode) from error
