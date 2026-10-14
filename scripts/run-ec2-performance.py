#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Start the fixed EC2 controller from automatic main CI and verify its signed result."""

from __future__ import annotations

import argparse
import json
import os
from pathlib import Path
import subprocess
import sys
import time

from performance_qualification import QualificationError, verified_document
from performance_ec2_capacity import describe_instance, start_controller, validate_instance


def aws(*args):
    result = subprocess.run(["aws", *args, "--output", "json"], capture_output=True, text=True, check=True)
    return json.loads(result.stdout) if result.stdout.strip() else None


def failure_detail(error):
    """Keep the provider's reason, which CalledProcessError.__str__ omits."""
    detail = {"type": type(error).__name__, "message": str(error)}
    if isinstance(error, subprocess.CalledProcessError):
        detail.update(returncode=error.returncode, stderr=(error.stderr or "")[-16384:])
    return detail


def await_online(instance):
    aws("ec2", "wait", "instance-running", "--instance-ids", instance)
    for _ in range(60):
        response = aws("ssm", "describe-instance-information", "--filters", f"Key=InstanceIds,Values={instance}")
        if any(item["PingStatus"] == "Online" for item in response["InstanceInformationList"]):
            return
        time.sleep(5)
    raise QualificationError("performance controller did not become online")


def verify_result(output: Path, key: bytes, head: str, run_id: str):
    result, _ = verified_document(output / "result.json", output / "result.json.sig", key)
    if result.get("head_revision") != head or result.get("run_id") != run_id:
        raise QualificationError("performance result names another revision or CI run")
    if result.get("acceptance_status") == "invalid":
        raise QualificationError(result.get("error", "invalid controlled run"))
    if set(result.get("reports", {})) != {"analytical", "claims"}:
        raise QualificationError("performance result is incomplete")
    for name, entry in result["reports"].items():
        report, sha = verified_document(output / (name + "-report.json"), output / (name + "-report.json.sig"), key)
        if (sha != entry["sha256"] or report.get("git_commit") != head
                or report.get("acceptance_status") != entry["acceptance_status"]):
            raise QualificationError("signed workload report differs from the session result")
        if report.get("acceptance_status") != "accepted" or report.get("timing_acceptance") is not True:
            raise QualificationError(f"{name}: {report.get('acceptance_status')}; retained intervals require review")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    instance, bucket = os.environ["PERFORMANCE_INSTANCE_ID"], os.environ["PERFORMANCE_ARTIFACT_BUCKET"]
    revision = os.environ["GITHUB_SHA"]
    run_id = os.environ["GITHUB_RUN_ID"] + "-" + os.environ["GITHUB_RUN_ATTEMPT"]
    args.output.mkdir(parents=True, exist_ok=True)
    invocation = None
    failure = None
    lifecycle = {"head_revision": revision, "run_id": run_id, "timing_acceptance": False,
                 "capacity_attempts": [], "errors": []}
    stage = "start-instance"

    def record_failure(error, failed_stage):
        lifecycle["errors"].append({"stage": failed_stage, **failure_detail(error)})
        print(json.dumps(lifecycle["errors"][-1]), file=sys.stderr)

    def retain(name, document):
        nonlocal failure
        try:
            (args.output / name).write_text(json.dumps(document, indent=2) + "\n")
        except Exception as error:
            failure = failure or error
            record_failure(error, "retain-" + name)

    try:
        selected_type = start_controller(aws, instance, lifecycle["capacity_attempts"])
        stage = "await-controller"
        await_online(instance)
        validate_instance(describe_instance(aws, instance), state="running", expected_type=selected_type)
        stage = "submit-controller"
        response = aws("ssm", "send-command", "--instance-ids", instance,
                       "--document-name", "UQAEngineControlledPerformance", "--timeout-seconds", "120",
                       "--parameters", json.dumps({"headRevision": [revision], "runId": [run_id]}))
        command_id = response["Command"]["CommandId"]
        stage = "await-result"
        for attempt in range(540):
            time.sleep(10)
            try:
                invocation = aws("ssm", "get-command-invocation", "--command-id", command_id, "--instance-id", instance)
            except subprocess.CalledProcessError as error:
                if attempt < 6 and "InvocationDoesNotExist" in (error.stderr or ""):
                    continue
                raise
            if invocation["Status"] not in {"Pending", "InProgress", "Delayed"}:
                break
        else:
            raise QualificationError("controlled run exceeded its fixed 90-minute CI deadline")
    except Exception as error:
        failure = error
        record_failure(error, stage)
    finally:
        if invocation is not None:
            retain("ssm-invocation.json", invocation)
        # Persist startup errors before collection or cleanup can fail as well.
        retain("ci-lifecycle.json", lifecycle)
        try:
            subprocess.run(["aws", "s3", "cp", f"s3://{bucket}/runs/{run_id}/", str(args.output),
                            "--recursive", "--only-show-errors"], capture_output=True, text=True, check=True)
        except Exception as error:
            failure = failure or error
            record_failure(error, "collect-artifacts")
        finally:
            for stop_stage, command in (("stop-instance", ("stop-instances",)),
                                        ("await-stopped", ("wait", "instance-stopped"))):
                try:
                    aws("ec2", *command, "--instance-ids", instance)
                except Exception as error:
                    failure = failure or error
                    record_failure(error, stop_stage)
            retain("ci-lifecycle.json", lifecycle)
    if failure is not None:
        raise failure
    result = verify_result(args.output, os.environ["PERFORMANCE_ISSUER_PUBLIC_KEY"].encode(), revision, run_id)
    if invocation is None or invocation["Status"] != "Success":
        raise QualificationError("SSM did not complete the fixed controller successfully")
    print(json.dumps(result, indent=2))


if __name__ == "__main__":
    main()
