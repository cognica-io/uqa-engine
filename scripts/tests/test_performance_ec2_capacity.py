#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import copy
from pathlib import Path
import subprocess
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from performance_ec2_capacity import describe_instance, start_controller, validate_guest, validate_instance
from performance_qualification import QualificationError


def fixture():
    return {"InstanceId": "instance", "InstanceType": "c7g.4xlarge", "Architecture": "arm64",
            "RootDeviceType": "ebs", "State": {"Name": "stopped"}, "Placement": {"Tenancy": "dedicated"}}


class CapacityTest(unittest.TestCase):
    def setUp(self):
        self.instance = fixture()
        self.calls, self.attempts = [], []
        self.errors = {}

    def aws(self, *args):
        self.calls.append(args)
        if args[:2] == ("ec2", "describe-instances"):
            return {"Reservations": [{"Instances": [copy.deepcopy(self.instance)]}]}
        if args[:2] == ("ec2", "modify-instance-attribute"):
            self.assertEqual(self.instance["State"]["Name"], "stopped")
            self.instance["InstanceType"] = args[-1].removeprefix("Value=")
        if args[:2] == ("ec2", "start-instances"):
            error = self.errors.get(self.instance["InstanceType"])
            if error:
                raise subprocess.CalledProcessError(254, ["aws", *args], stderr=error)
            self.instance["State"]["Name"] = "running"

    @staticmethod
    def capacity_error(code="InsufficientInstanceCapacity"):
        return f"An error occurred ({code}) when calling the StartInstances operation: capacity unavailable"

    def starts(self):
        return [call for call in self.calls if call[:2] == ("ec2", "start-instances")]

    def changes(self):
        return [call[-1] for call in self.calls if call[:2] == ("ec2", "modify-instance-attribute")]

    def test_primary_success_never_changes_the_instance_or_retries(self):
        self.assertEqual(start_controller(self.aws, "instance", self.attempts), "c7g.4xlarge")
        self.assertEqual(len(self.starts()), 1)
        self.assertEqual(self.changes(), [])
        self.assertEqual(self.attempts, [{"instance_type": "c7g.4xlarge", "status": "started"}])

    def test_only_explicit_capacity_failure_selects_one_reviewed_alternative(self):
        for code in ("InsufficientInstanceCapacity", "Server.InsufficientInstanceCapacity"):
            with self.subTest(code=code):
                self.setUp()
                self.errors["c7g.4xlarge"] = self.capacity_error(code)
                self.assertEqual(start_controller(self.aws, "instance", self.attempts), "m7g.4xlarge")
                self.assertEqual(len(self.starts()), 2)
                self.assertEqual(self.changes(), ["Value=m7g.4xlarge"])
                self.assertEqual([attempt["status"] for attempt in self.attempts], ["failed", "started"])
                self.assertIn(("ec2", "wait", "instance-stopped", "--instance-ids", "instance"), self.calls)

    def test_quota_permission_and_uncertain_failures_do_not_resize_or_retry(self):
        for code in ("UnauthorizedOperation", "InstanceLimitExceeded", "InternalError"):
            with self.subTest(code=code):
                self.setUp()
                self.errors["c7g.4xlarge"] = self.capacity_error(code) + " (InsufficientInstanceCapacity)"
                with self.assertRaises(subprocess.CalledProcessError):
                    start_controller(self.aws, "instance", self.attempts)
                self.assertEqual(len(self.starts()), 1)
                self.assertEqual(self.changes(), [])

    def test_both_pools_exhausted_are_bounded_and_keep_original_provider_errors(self):
        self.errors = {name: self.capacity_error() for name in ("c7g.4xlarge", "m7g.4xlarge")}
        with self.assertRaises(subprocess.CalledProcessError):
            start_controller(self.aws, "instance", self.attempts)
        self.assertEqual(len(self.starts()), 2)
        self.assertEqual([attempt["status"] for attempt in self.attempts], ["failed", "failed"])
        self.assertTrue(all("InsufficientInstanceCapacity" in attempt["stderr"] for attempt in self.attempts))
        self.assertEqual(self.instance["State"]["Name"], "stopped")

    def test_next_session_prefers_the_smaller_profile_again(self):
        self.instance["InstanceType"] = "m7g.4xlarge"
        self.assertEqual(start_controller(self.aws, "instance", self.attempts), "c7g.4xlarge")
        self.assertEqual(self.changes(), ["Value=c7g.4xlarge"])
        self.assertEqual(len(self.starts()), 1)

    def test_running_host_is_never_resized_or_restarted(self):
        self.instance.update(InstanceType="m7g.4xlarge", State={"Name": "running"})
        self.assertEqual(start_controller(self.aws, "instance", self.attempts), "m7g.4xlarge")
        self.assertEqual(self.starts(), [])
        self.assertEqual(self.changes(), [])

    def test_unreviewed_or_transitioning_host_fails_before_mutation(self):
        for key, value in (("Architecture", "x86_64"), ("RootDeviceType", "instance-store"),
                           ("InstanceType", "c7g.8xlarge"), ("Placement", {"Tenancy": "default"}),
                           ("State", {"Name": "pending"}), ("State", {"Name": "stopping"})):
            with self.subTest(key=key, value=value):
                self.setUp()
                self.instance[key] = value
                with self.assertRaises(QualificationError):
                    start_controller(self.aws, "instance", self.attempts)
                self.assertEqual(self.starts(), [])
                self.assertEqual(self.changes(), [])

    def test_instance_identity_and_selected_profile_are_checked(self):
        self.instance["InstanceId"] = "another"
        with self.assertRaises(QualificationError):
            describe_instance(self.aws, "instance")
        instance = fixture()
        instance["State"]["Name"] = "running"
        validate_instance(instance, state="running", expected_type="c7g.4xlarge")
        with self.assertRaises(QualificationError):
            validate_instance(instance, state="running", expected_type="m7g.4xlarge")

    def test_guest_must_match_selected_cpu_and_memory_profile(self):
        for instance_type, nominal in (("c7g.4xlarge", 32 * 1024**2), ("m7g.4xlarge", 64 * 1024**2)):
            validate_guest(instance_type, 16, int(nominal * 0.95))
            for cpu_count, memory in ((8, nominal), (32, nominal), (16, nominal // 2), (16, nominal * 2)):
                with self.subTest(instance_type=instance_type, cpu_count=cpu_count, memory=memory), \
                        self.assertRaises(QualificationError):
                    validate_guest(instance_type, cpu_count, memory)


if __name__ == "__main__":
    unittest.main()
