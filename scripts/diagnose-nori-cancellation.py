#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Trace allocator system calls without treating instrumented timing as calibration."""

import hashlib
import json
import pathlib
import platform
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPORTS = ROOT / "target/benchmark-runs"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def main():
    if platform.system() != "Linux":
        raise RuntimeError("allocator system-call diagnostics require Linux")
    source = REPORTS / "nori-cancellation-native-repeat.json"
    if not source.exists():
        source = REPORTS / "nori-cancellation-native-baseline.json"
    if not source.exists():
        print("No completed cancellation measurement is available for diagnostics")
        return
    trace = shutil.which("strace")
    if trace is None:
        raise RuntimeError("strace is required for allocator system-call diagnostics")
    report = json.loads(source.read_text())
    provenance = report["provenance"]
    if provenance["arguments"] != ["--cancellation"]:
        raise RuntimeError("expected the existing cancellation benchmark invocation")
    artifacts = provenance["artifacts"]
    if len(artifacts) != 1 or not re.fullmatch(r"nori-[0-9a-f]{16}", artifacts[0]["name"]):
        raise RuntimeError("expected exactly one native analysis benchmark executable")
    artifact = artifacts[0]
    binary = ROOT / "target/release/deps" / artifact["name"]
    if binary.stat().st_size != artifact["bytes"] or digest(binary) != artifact["sha256"]:
        raise RuntimeError("diagnostic executable differs from the measured executable")
    output = REPORTS / "cancellation-diagnostics"
    output.mkdir(exist_ok=True)
    receipt = {
        "schema_version": 1,
        "purpose": "allocator_system_call_diagnostic",
        "calibration_eligible": False,
        "source_report": source.name,
        "source_report_sha256": digest(source),
        "provenance": provenance,
        "strace": subprocess.check_output([trace, "--version"], text=True).splitlines()[0],
        "runs": [],
    }
    for index in range(2):
        raw = output / f"run-{index}.json"
        calls = output / f"run-{index}.syscalls.log"
        command = [trace, "-qq", "-e", "trace=brk,mmap,munmap,write", "-o", str(calls), str(binary), "--cancellation"]
        with raw.open("w") as stream:
            subprocess.run(command, cwd=ROOT, stdout=stream, check=True)
        diagnostic = json.loads(raw.read_text())
        if diagnostic["purpose"] != "cooperative_cancellation" or len(diagnostic["measurements"]) != 108:
            raise RuntimeError("incomplete instrumented cancellation execution")
        receipt["runs"].append({
            "report": raw.name,
            "report_sha256": digest(raw),
            "system_calls": calls.name,
            "system_calls_sha256": digest(calls),
        })
    if digest(binary) != artifact["sha256"]:
        raise RuntimeError("diagnostic executable changed during collection")
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
