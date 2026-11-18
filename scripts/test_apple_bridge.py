#!/usr/bin/env python3
"""Packaging failure tests and an optimized, generated foreign-language smoke test.

Python/host FFI evidence supplements, never replaces, the iOS Swift smoke tests.
"""
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
from unittest.mock import patch

sys.dont_write_bytecode = True
import apple_bridge as bridge


class PackagingTests(unittest.TestCase):
    def test_failed_publish_preserves_previous_artifact_set(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            old, staged = root / "Artifacts", root / "staged"
            old.mkdir()
            (old / "manifest").write_text("previous complete artifact")
            staged.mkdir()
            original_rename = Path.rename

            def fail_staged(path, destination):
                if path == staged:
                    raise OSError("synthetic publish failure")
                return original_rename(path, destination)

            with patch.object(Path, "rename", fail_staged), self.assertRaises(OSError):
                bridge.publish_directory(staged, old)
            self.assertEqual((old / "manifest").read_text(), "previous complete artifact")
            self.assertFalse((root / "Artifacts.previous").exists())

    def test_source_drift_is_rejected(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            generated, package = root / "generated", root / "package"
            generated.mkdir()
            for name in bridge.GENERATED:
                (generated / name).write_text("matching generation\n")
            bridge.copy_generated(generated, package)
            bridge.check_generated(generated, package)
            (generated / "RustBridge.swift").write_text("different API\n")
            with self.assertRaises(SystemExit):
                bridge.check_generated(generated, package)

    def test_runtime_graph_rejects_provider_and_trace_features(self):
        for line in ("er-ai v0.1.0 features=[]", "aws-sdk-s3 v1.0.0 features=[]",
                     "er-pipeline v0.1.0 features=[cloud-stt]",
                     "uniffi_core v0.32.2 features=[ffi-trace]",
                     "uniffi_bindgen v0.32.2 features=[]"):
            with self.subTest(line=line), patch.object(bridge, "cargo", return_value=line):
                with self.assertRaises(SystemExit):
                    bridge.dependencies()

    def test_profile_overrides_are_rejected(self):
        for key, value in (("RUSTFLAGS", "-C panic=abort"),
                           ("CARGO_ENCODED_RUSTFLAGS", "-C\x1fpanic=abort"),
                           ("CARGO_PROFILE_IOS_RELEASE_PANIC", "abort")):
            with self.subTest(key=key), patch.dict(os.environ, {key: value}):
                with self.assertRaises(SystemExit):
                    bridge.validate_environment()


def foreign_smoke():
    generator, library = bridge.build_host(panic_test=True)
    build, _ = bridge.paths(True)
    with tempfile.TemporaryDirectory(prefix="foreign-smoke-", dir=build) as directory:
        output = Path(directory)
        bridge.run(generator, "generate", "--library", library, "--language", "python",
                   "--out-dir", output, "--no-format")
        # The generated module looks for the library next to itself.
        import shutil
        shutil.copy2(library, output / library.name)
        script = '''
import er_ffi
assert er_ffi.ping() == "pong"
config = er_ffi.DeviceConfig(database_path="/synthetic/device.sqlite", account_scope="synthetic-account")
result = er_ffi.validate_config(config)
assert result.database_path == config.database_path
assert result.account_scope == config.account_scope
try:
    er_ffi.validate_config(er_ffi.DeviceConfig(database_path="ER_FFI_PRIVATE_SENTINEL", account_scope="account"))
except er_ffi.FfiError.InvalidInput as error:
    assert "ER_FFI_PRIVATE_SENTINEL" not in str(error)
else:
    raise AssertionError("invalid config accepted")
try:
    er_ffi.panic_for_test()
except er_ffi.FfiError.Internal as error:
    assert "ER_FFI_PRIVATE_SENTINEL" not in str(error)
else:
    raise AssertionError("panic did not return declared error")
assert er_ffi.ping() == "pong"
import tempfile
from pathlib import Path
with tempfile.TemporaryDirectory() as directory:
    config = er_ffi.DeviceConfig(database_path=str(Path(directory) / "device.sqlite"), account_scope="synthetic-account")
    device = er_ffi.ErDevice(config)
    shift = device.start_shift(1)
    first = device.add_patient("Synthetic complaint — 简洁", "Room 4", 2)
    second = device.add_patient("Synthetic second", "Hall 3", 3)
    device.select_patient(first, 4)
    device.set_consent(first, er_ffi.ConsentChoice.DECLINED, "synthetic-policy", 0, 5)
    device.set_mode(first, True)
    device.record_objective(first, er_ffi.ObjectiveInput.LAB(name="Synthetic potassium", value=4.2, unit="mmol/L", abnormal=False), 6)
    device.record_objective(first, er_ffi.ObjectiveInput.IMAGING(modality="XR", body_part="Chest", impression="Synthetic report", critical=False), 6)
    device.record_objective(first, er_ffi.ObjectiveInput.VITAL(kind=er_ffi.VitalKind.HEART_RATE, value=80.0, unit="bpm"), 6)
    device.record_medication(first, er_ffi.MedicationInput(name="Synthetic medication", dose="1 mg", route=None, kind=er_ffi.MedicationKindDto.HOME, frequency="daily", administered_at_ms=6), 6)
    device.record_diagnosis(first, "Synthetic diagnosis", None, True, 6)
    device.record_data_reviewed(first, "Synthetic data reviewed", 6)
    device.set_disposition(first, er_ffi.DispositionInput.DISCHARGE(instructions="Synthetic instructions", follow_up=None), 6)
    device.save_context(er_ffi.ChartContextDto(id="", patient_id=first, version=0, category=er_ffi.ChartCategory.HISTORY, text="ER_FFI_PRIVATE_SENTINEL", source="Clinician supplied", clinical_at_ms=None, entered_at_ms=7, reviewed=False), 0)
    definition = device.save_definition(er_ffi.DefinitionDto(id="", version=0, name="Synthetic template", kind=er_ffi.DefinitionKindDto.TEMPLATE, instructions="", sections=[er_ffi.SectionPreferenceDto(section="hpi", instructions="Brief prose")], archived=False, is_default=True), 0)
    assert device.note(first).hpi is None
    assert len(device.note(first).results) == 3
    assert len(device.note(first).medications) == 1
    assert len(device.note(first).diagnoses) == 1
    assert device.note(first).ros is None
    device.close()
    device = er_ffi.ErDevice(config)
    state = device.snapshot()
    assert state.shift.id == shift
    assert [p.id for p in state.patients] == [first, second]
    assert state.patients[0].consent.status == er_ffi.ConsentChoice.DECLINED
    assert state.patients[0].recording.solo
    assert not state.patients[0].recording.hardware_available
    assert device.chart_context(first)[0].clinical_at_ms is None
    assert device.definition_versions(definition)[0].version == 1
    try:
        device.panic_device_for_test()
    except er_ffi.FfiError.Internal:
        pass
    else:
        raise AssertionError("stateful panic was not typed")
    try:
        device.snapshot()
    except er_ffi.FfiError.Internal:
        pass
    else:
        raise AssertionError("faulted handle remained available")
    device.close()
    device = er_ffi.ErDevice(config)
    assert device.snapshot().shift.id == shift
    device.close()
assert er_ffi.device_interface_version() == 2
print("Optimized generated FFI lifecycle smoke passed")
'''
        result = subprocess.run([sys.executable, "-c", script], cwd=output,
                                capture_output=True, text=True,
                                env=dict(os.environ, RUST_BACKTRACE="1"))
        if "ER_FFI_PRIVATE_SENTINEL" in result.stdout + result.stderr:
            raise SystemExit("Private panic/input sentinel leaked across the boundary")
        if result.returncode:
            # Output contains only the synthetic test; sentinels were checked above.
            print(result.stderr, file=sys.stderr)
            raise SystemExit("Optimized foreign smoke failed")
        print(result.stdout.strip())


if __name__ == "__main__":
    result = unittest.main(exit=False).result
    if not result.wasSuccessful():
        raise SystemExit(1)
    foreign_smoke()
