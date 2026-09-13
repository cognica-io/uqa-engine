#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Inspect allocator calls and process state without changing regression gates."""

import hashlib
import json
import os
import pathlib
import platform
import re
import shutil
import subprocess

ROOT = pathlib.Path(__file__).resolve().parents[1]
REPORTS = ROOT / "target/benchmark-runs"


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def validate_output(path, expected):
    report = json.loads(path.read_text())
    if report["purpose"] != "cooperative_cancellation" or len(report["measurements"]) != 108:
        raise RuntimeError("incomplete diagnostic cancellation execution")
    rows = {row["name"]: row for row in report["measurements"]}
    if rows.keys() != expected.keys():
        raise RuntimeError("diagnostic cancellation workloads changed")
    for name, row in rows.items():
        for key in ("input_bytes", "input_utf16", "input_sha256", "output_sha256", "tokens", "complete_polls", "cancel_at_poll", "allocation"):
            if row[key] != expected[name][key]:
                raise RuntimeError(f"diagnostic cancellation result changed: {name}/{key}")
    return report


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
    expected = {row["name"]: row for row in report["measurements"]}
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
        "purpose": "allocator_process_state_diagnostic",
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
        validate_output(raw, expected)
        receipt["runs"].append({
            "control": "system_call_trace",
            "report": raw.name,
            "report_sha256": digest(raw),
            "system_calls": calls.name,
            "system_calls_sha256": digest(calls),
        })
    environment = dict(os.environ)
    tunables = environment.get("GLIBC_TUNABLES", "")
    uncached = ":".join([entry for entry in tunables.split(":") if entry and not entry.startswith("glibc.malloc.tcache_count=")] + ["glibc.malloc.tcache_count=0"])
    receipt["inherited_allocator_environment"] = {
        key: value for key, value in environment.items()
        if key == "GLIBC_TUNABLES" or key.startswith("MALLOC_")
    }
    setarch = shutil.which("setarch")
    controls = [("thread_cache_disabled", [], {"GLIBC_TUNABLES": uncached})]
    if setarch is None:
        receipt["address_layout_control"] = "setarch is unavailable"
    else:
        controls.append(("fixed_address_layout", [setarch, platform.machine(), "-R"], {}))
    ordinary = [report]
    baseline = REPORTS / "nori-cancellation-native-baseline.json"
    if baseline != source and baseline.exists():
        ordinary.append(json.loads(baseline.read_text()))
    counts = {
        name: 2 * max(value for candidate in ordinary for row in candidate["measurements"]
                      if row["name"] == name for value in row["operation_timing"]["iterations"])
        for name in expected
    }
    sampling = output / "sampling-iterations.json"
    sampling.write_text(json.dumps(counts, sort_keys=True, indent=2) + "\n")
    receipt["sampling_iterations"] = {
        "path": sampling.name,
        "sha256": digest(sampling),
        "rule": "Twice the largest observed operation count for each workload in the ordinary pair.",
    }
    for mode in ("timed", "fixed"):
        controls.append((f"sampling_control_{mode}", [], {
            "UQA_NORI_CANCELLATION_SAMPLING": sampling.relative_to(ROOT).as_posix(),
            "UQA_NORI_CANCELLATION_SAMPLING_MODE": mode,
        }))
    for label, prefix, overrides in controls:
        for index in range(2):
            raw = output / f"{label}-{index}.json"
            command = [*prefix, str(binary), "--cancellation"]
            with raw.open("w") as stream:
                result = subprocess.run(command, cwd=ROOT, env={**environment, **overrides}, stdout=stream, stderr=subprocess.PIPE, text=True)
            entry = {"control": label, "environment_overrides": overrides, "returncode": result.returncode}
            if result.returncode:
                entry["error"] = result.stderr[-4096:]
                receipt["runs"].append(entry)
                (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")
                if label != "fixed_address_layout" or "failed to set personality" not in result.stderr:
                    raise RuntimeError(f"diagnostic cancellation execution failed: {label}")
                break
            diagnostic = validate_output(raw, expected)
            if label.startswith("sampling_control_"):
                mode = overrides["UQA_NORI_CANCELLATION_SAMPLING_MODE"]
                if diagnostic["protocol"].get("sampling_control") != {"mode": mode, "iterations_sha256": digest(sampling)}:
                    raise RuntimeError("sampling control identity differs from its input")
                if mode == "fixed":
                    for row in diagnostic["measurements"]:
                        wanted = [counts[row["name"]]] * diagnostic["protocol"]["samples"]
                        for scope in ("operation_timing", "response_timing"):
                            if row[scope]["iterations"] != wanted:
                                raise RuntimeError("sampling control did not execute its fixed workload")
            entry.update({"report": raw.name, "report_sha256": digest(raw)})
            receipt["runs"].append(entry)
    if digest(binary) != artifact["sha256"]:
        raise RuntimeError("diagnostic executable changed during collection")
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n")


if __name__ == "__main__":
    main()
