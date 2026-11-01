#!/usr/bin/env python3
"""Release fault injection with fake AWS/Terraform; no credentials or network."""
import contextlib
import copy
import io
import json
from pathlib import Path
import subprocess
import tempfile
import tarfile
import unittest
from unittest.mock import Mock, patch

import deploy


class CompletionTests(unittest.TestCase):
    def test_migration_requires_exact_definition_container_and_explicit_zero(self):
        good = {"lastStatus": "STOPPED", "stopCode": "EssentialContainerExited", "taskDefinitionArn": "expected",
                "containers": [{"name": "migration", "exitCode": 0}]}
        self.assertTrue(deploy.migration_success(good, "expected"))
        for field, value in [("lastStatus", "RUNNING"), ("stopCode", "TaskFailedToStart"),
                             ("taskDefinitionArn", "old"), ("containers", []),
                             ("containers", [{"name": "migration"}]),
                             ("containers", [{"name": "migration", "exitCode": 1}]),
                             ("containers", [{"name": "api", "exitCode": 0}])]:
            with self.subTest(field=field, value=value):
                self.assertFalse(deploy.migration_success(dict(good, **{field: value}), "expected"))

    def test_stable_rollback_is_not_candidate_success(self):
        service = {"desiredCount": 1, "runningCount": 1, "pendingCount": 0, "taskDefinition": "candidate",
                   "deployments": [{"rolloutState": "COMPLETED", "taskDefinition": "candidate"}]}
        task = {"lastStatus": "RUNNING", "taskDefinitionArn": "candidate", "containers": [{"imageDigest": "sha256:new"}]}
        self.assertTrue(deploy.exact_service(service, [task], "candidate", "sha256:new", 1))
        old = dict(service, taskDefinition="previous")
        self.assertFalse(deploy.exact_service(old, [task], "candidate", "sha256:new", 1))
        for tasks in ([], [dict(task, containers=[{"imageDigest": "sha256:old"}])], [task, task],
                      [dict(task, taskDefinitionArn="previous")]):
            self.assertFalse(deploy.exact_service(service, tasks, "candidate", "sha256:new", 1))

    def test_commands_never_echo_secret_diagnostics(self):
        with patch.object(deploy.subprocess, "run", return_value=subprocess.CompletedProcess([], 1, b"secret", b"token")):
            with self.assertRaisesRegex(deploy.GateError, "^command_failed_aws$"):
                deploy.run(["aws", "anything"])


class StateMachineTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.deployment = object.__new__(deploy.Deployment)
        d = self.deployment
        d.work = Path(self.work.name)
        d.key = "record"
        d.accepted_key = "accepted"
        d.bucket = "synthetic"
        d.region = "us-east-2"
        d.release_id = "1-1"
        d.commit = "a" * 40
        d.config = dict(aws_account_id="000000000000", aws_region="us-east-2", project="ernote", deploy_artifacts_bucket="synthetic")
        d.put = Mock()
        d.init = Mock()
        d.plan = Mock(return_value=(Path("candidate.tfplan"), {"planned_values": {"outputs": {
            "deployment": {"value": {"migration": {}, "network": {}}}}}}))
        d.tf = Mock()
        d.publish_image = Mock(return_value="sha256:" + "a" * 64)
        d.verify = Mock(return_value={})
        d.smoke_outputs = Mock()

    def test_failed_migration_never_applies_services_or_accepts(self):
        d = self.deployment
        d.get = Mock(side_effect=[None, {"resources": {"ecs_cluster_arn": "cluster"}}, None])
        d.migrate = Mock(side_effect=deploy.GateError("migration_exit_gate"))
        with patch.object(deploy, "aws", return_value={"VersionId": "v1"}), patch.object(deploy, "output"):
            with self.assertRaisesRegex(deploy.GateError, "migration_exit_gate"):
                d.deploy()
        d.tf.assert_not_called()
        d.verify.assert_not_called()
        self.assertEqual(d.put.call_args.args[1]["phase"], "prepared")

    def test_rollout_failure_is_persisted_before_apply_for_recovery(self):
        d = self.deployment
        d.get = Mock(side_effect=[None, {"resources": {"ecs_cluster_arn": "cluster"}}, None])
        d.migrate = Mock(return_value={"exit_code": 0})
        d.tf.side_effect = deploy.GateError("rollout_failure")
        with patch.object(deploy, "aws", return_value={"VersionId": "v1"}), patch.object(deploy, "output"):
            with self.assertRaisesRegex(deploy.GateError, "rollout_failure"):
                d.deploy()
        self.assertEqual(d.put.call_args.args[1]["phase"], "rollout_started")
        d.verify.assert_not_called()

    def test_submission_failures_and_already_active_task_block(self):
        for responses, label in [([{"taskArns": ["active"]}], "already_active"),
            ([{"taskArns": []}, {"taskDefinition": {"taskDefinitionArn": "def"}}, {"failures": [{"reason": "private"}], "tasks": []}], "submission")]:
            with self.subTest(label=label), patch.object(deploy, "aws", side_effect=responses) as aws:
                with self.assertRaisesRegex(deploy.GateError, label):
                    self.deployment.migrate({"family": "migration"}, {}, "cluster")
                if label == "already_active":
                    self.assertEqual(aws.call_count, 1)

    def test_missing_exit_code_blocks_and_timeout_stops_only_migration(self):
        responses = [{"taskArns": []}, {"taskDefinition": {"taskDefinitionArn": "def"}},
                     {"tasks": [{"taskArn": "task"}]}, {"tasks": [{"lastStatus": "STOPPED"}]}]
        with patch.object(deploy, "aws", side_effect=responses), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(deploy.GateError, "migration_exit_gate"):
                self.deployment.migrate({"family": "migration"}, {}, "cluster")
        with patch.object(deploy, "aws", side_effect=responses[:3] + [{}]) as aws, \
                patch.object(deploy.time, "monotonic", side_effect=[0, 901]), contextlib.redirect_stdout(io.StringIO()):
            with self.assertRaisesRegex(deploy.GateError, "migration_timeout"):
                self.deployment.migrate({"family": "migration"}, {}, "cluster")
            self.assertEqual(aws.call_args.args, ("ecs", "stop-task"))

    def test_accept_requires_smoke_phase_and_unchanged_previous_pointer(self):
        for record, current in [({"phase": "rollout_started"}, None),
                                ({"phase": "awaiting_smoke", "previous": None, "release_id": "1-1"}, {"release_id": "other"})]:
            self.deployment.get = Mock(side_effect=[record, current])
            with self.assertRaises(deploy.GateError):
                self.deployment.accept()
            self.deployment.put.assert_not_called()

    def test_success_advances_pointer_only_at_accept(self):
        d = self.deployment
        d.get = Mock(side_effect=[{"phase": "awaiting_smoke", "previous": None, "release_id": "1-1"}, None])
        with contextlib.redirect_stdout(io.StringIO()):
            d.accept()
        self.assertEqual([call.args[0] for call in d.put.call_args_list], ["accepted", "record"])
        self.assertEqual(d.put.call_args.args[1]["phase"], "accepted")

    def test_accept_retry_finishes_after_pointer_write_response_was_lost(self):
        d = self.deployment
        d.get = Mock(side_effect=[{"phase": "awaiting_smoke", "previous": None, "release_id": "1-1"},
                                 {"phase": "accepted", "release_id": "1-1", "accepted_at": "original"}])
        with contextlib.redirect_stdout(io.StringIO()):
            d.accept()
        self.assertEqual(d.put.call_args.args[1]["accepted_at"], "original")

    def test_first_release_failure_stops_through_terraform_without_down_migration(self):
        d = self.deployment
        d.get = Mock(return_value={"phase": "rollout_started", "previous": None, "inputs": {"services_enabled": True}})
        with contextlib.redirect_stdout(io.StringIO()):
            d.recover()
        self.assertFalse(d.plan.call_args.args[1]["services_enabled"])
        self.assertEqual(d.put.call_args.args[1]["phase"], "stopped_failed_bootstrap")

    def test_unknown_schema_compatibility_stops_instead_of_guessing_rollback(self):
        d = self.deployment
        d.get = Mock(return_value={"phase": "awaiting_smoke", "previous": {"migrations": {"old": "hash"}},
                                  "migrations": {"new": "hash"}, "inputs": {}})
        with contextlib.redirect_stdout(io.StringIO()):
            d.recover()
        self.assertFalse(d.plan.call_args.args[1]["services_enabled"])
        self.assertEqual(d.put.call_args.args[1]["phase"], "stopped_forward_repair_required")

    def test_compatible_rollback_uses_previous_source_and_pinned_inputs(self):
        d = self.deployment
        previous = {"commit": "b" * 40, "migrations": {"migration.sql": "hash"},
                    "inputs": {"image_digest": "sha256:previous", "manifest_version_id": "old-version", "services_enabled": True}}
        d.get = Mock(return_value={"phase": "awaiting_smoke", "previous": copy.deepcopy(previous),
                                  "migrations": previous["migrations"], "inputs": {"image_digest": "sha256:candidate"}})
        archive = io.BytesIO()
        with tarfile.open(fileobj=archive, mode="w"):
            pass
        with patch.object(deploy, "run", return_value=archive.getvalue()) as command, \
                patch.object(deploy, "output") as output, contextlib.redirect_stdout(io.StringIO()):
            d.recover()
        self.assertEqual(command.call_args.args[0], ["git", "archive", "b" * 40])
        self.assertEqual(d.plan.call_args.args[1], previous["inputs"])
        d.verify.assert_called_once()
        self.assertEqual(d.put.call_args.args[1]["phase"], "rollback_awaiting_smoke")
        output.assert_called_with(rollback="true")

    def test_preflight_failure_recovery_has_no_mutations(self):
        self.deployment.get = Mock(return_value={"phase": "prepared"})
        with contextlib.redirect_stdout(io.StringIO()):
            self.deployment.recover()
        self.deployment.tf.assert_not_called()
        self.deployment.put.assert_not_called()


if __name__ == "__main__":
    unittest.main()
