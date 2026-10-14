#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reviewed EC2 host profiles and bounded, pre-measurement capacity failover."""

from __future__ import annotations

import re
import subprocess

from performance_qualification import QualificationError


# Both profiles retain sixteen physical Graviton3 cores and the same EBS volume.
# Memory differs, so comparisons require fresh calibration on the selected boot.
HOST_MEMORY_MIB = {"c7g.4xlarge": 32768, "m7g.4xlarge": 65536}


def validate_instance(instance, *, state, expected_type=None):
    if (instance.get("InstanceType") not in HOST_MEMORY_MIB
            or (expected_type is not None and instance["InstanceType"] != expected_type)
            or instance.get("State", {}).get("Name") != state
            or instance.get("Placement", {}).get("Tenancy") != "dedicated"
            or instance.get("Architecture") != "arm64"
            or instance.get("RootDeviceType") != "ebs"):
        raise QualificationError("EC2 does not match a reviewed controlled-host profile")


def validate_guest(instance_type, cpu_count, memory_kib):
    nominal_kib = HOST_MEMORY_MIB[instance_type] * 1024
    if cpu_count != 16 or not nominal_kib * 0.9 <= memory_kib <= nominal_kib:
        raise QualificationError("guest CPU or memory differs from the reviewed EC2 profile")


def describe_instance(aws, instance_id):
    response = aws("ec2", "describe-instances", "--instance-ids", instance_id)
    instances = [instance for reservation in response["Reservations"] for instance in reservation["Instances"]]
    if len(instances) != 1 or instances[0].get("InstanceId") != instance_id:
        raise QualificationError("EC2 returned another controlled instance")
    return instances[0]


def verify_resize_permissions(aws, instance_id):
    """Ask EC2 to authorize each reviewed change without performing it."""
    for instance_type in HOST_MEMORY_MIB:
        try:
            aws("ec2", "modify-instance-attribute", "--instance-id", instance_id,
                "--instance-type", "Value=" + instance_type, "--dry-run")
        except subprocess.CalledProcessError as error:
            if "(DryRunOperation) when calling the ModifyInstanceAttribute operation" not in (error.stderr or ""):
                raise
        else:
            raise QualificationError("EC2 did not confirm the capacity authorization check")


def start_controller(aws, instance_id, attempts):
    instance = describe_instance(aws, instance_id)
    state = instance["State"]["Name"]
    validate_instance(instance, state=state)
    if state == "running":
        # An already running host is never resized or restarted by failover.
        attempts.append({"instance_type": instance["InstanceType"], "status": "already-running"})
        return instance["InstanceType"]
    if state != "stopped":
        raise QualificationError("capacity selection requires a stopped controller")
    current_type = instance["InstanceType"]
    for instance_type in HOST_MEMORY_MIB:
        if current_type != instance_type:
            aws("ec2", "modify-instance-attribute", "--instance-id", instance_id,
                "--instance-type", "Value=" + instance_type)
            current_type = instance_type
        attempt = {"instance_type": instance_type, "status": "starting"}
        attempts.append(attempt)
        try:
            aws("ec2", "start-instances", "--instance-ids", instance_id)
        except subprocess.CalledProcessError as error:
            match = re.search(r"\((?:Server\.)?(InsufficientInstanceCapacity)\) when calling the StartInstances operation",
                              error.stderr or "")
            attempt.update(status="failed", stderr=(error.stderr or "")[-16384:])
            if not match or instance_type == next(reversed(HOST_MEMORY_MIB)):
                raise
            # No measured execution has started. Never retry SSM or a workload.
            aws("ec2", "wait", "instance-stopped", "--instance-ids", instance_id)
        else:
            attempt["status"] = "started"
            return instance_type
    raise QualificationError("no reviewed EC2 capacity profile was selected")
