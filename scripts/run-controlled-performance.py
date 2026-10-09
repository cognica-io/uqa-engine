#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Operator-owned EC2 controller. Candidate processes cannot issue timing evidence."""

from __future__ import annotations

import argparse
import datetime as dt
import fcntl
import importlib.util
import json
import os
from pathlib import Path
import re
import statistics
import sys

from controlled_performance import scaling_summary
from controlled_runner_host import CONTROL, ControlledHost, command, file_hash
from performance_noise import CALIBRATION_PAIRS, COMPARISON_PAIRS, controlled_analytical_protocol, reference_noise_bound
from performance_qualification import QualificationError, digest, observations, qualify, ratio_decision


REFERENCE = "0e8be6892340b2d1892e0abf68c2a9ad046f49d5"
HERE = Path(__file__).resolve().parent


def now() -> str:
    return dt.datetime.now(dt.timezone.utc).isoformat()


def save(path: Path, value: dict) -> Path:
    path.write_text(json.dumps(value, indent=2, sort_keys=True, allow_nan=False) + "\n")
    return path


def analytical_identity(manifest: dict) -> dict:
    spec = importlib.util.spec_from_file_location("analytical_gate", HERE / "check-analytical-benchmark.py")
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module.workload_identity(manifest)


def paired_samples(host, kind, first, second, manifest, count, stage):
    pairs = []
    for index in range(count):
        host.progress(stage, workload=kind, completed_pairs=index, total_pairs=count)
        pair = {}
        for role in (("first", "second") if index % 2 == 0 else ("second", "first")):
            source, artifact = first if role == "first" else second
            pair[role] = host.measure(kind, artifact, source, f"{kind}-{stage}-{index:02}-{role}", manifest)
        pairs.append(pair)
    save(host.output / f"{kind}-{stage}-observations.json", {"pairs": pairs})
    return pairs


def calibrate(host, kind, reference, manifest, identity, protocol, limits, environment):
    pairs = paired_samples(host, kind, reference, reference, manifest, CALIBRATION_PAIRS, "calibration")
    bounds = {}
    for name, maximum in limits.items():
        bound = reference_noise_bound([(pair["first"][name], pair["second"][name]) for pair in pairs])
        bounds[name] = {**bound, "max_ratio": maximum}
    completed = now()
    document = {"schema_version": 1,
                "kind": "independent_timing_calibration" if kind == "analytical" else "independent_claim_calibration",
                "calibration_id": host.run_id + "-" + kind + "-reference-only",
                "host_id": host.config["instance_id"], "environment_id": environment,
                "completed_at": completed, "valid_from": completed,
                "valid_until": (dt.datetime.now(dt.timezone.utc) + dt.timedelta(hours=3)).isoformat(),
                "workload_identity_sha256": identity, "regression_protocol": protocol,
                "estimator": protocol["point_estimator"], "benchmarks": bounds,
                "reference_executable": reference[1],
                "independent_evidence": {"reference": f"{kind}-calibration-observations.json",
                                         "sha256": file_hash(host.output / f"{kind}-calibration-observations.json")},
                "coverage_assumption": "Exchangeable absolute multiplicative pair errors within this controlled session; marginal per workload, not simultaneous coverage."}
    path = save(host.output / (kind + "-calibration.json"), document)
    host.sign(path)
    return document, path


def compare(host, kind, head, reference, manifest, identity, protocol, calibration, environment):
    bounds, calibration_path = calibration
    started = now()
    pairs = paired_samples(host, kind, head, reference, manifest, COMPARISON_PAIRS, "comparison")
    names = bounds["benchmarks"]
    heads = {name: [pair["first"][name] for pair in pairs] for name in names}
    bases = {name: [pair["second"][name] for pair in pairs] for name in names}
    gates = []
    for name, bound in names.items():
        ratios = [pair["first"][name] / pair["second"][name] for pair in pairs]
        gates.append({"benchmark": name, "paired_ratios": ratios, "ratio": statistics.median(ratios),
                      "maximum": bound["max_ratio"]})
    report = {"schema_version": 1, "workload": kind, "git_commit": head[1]["revision"],
              "workload_identity_sha256": identity, "benchmark_executable_sha256": head[1]["sha256"],
              "regression_protocol": protocol, "regression_ratio_checks": gates,
              "baseline": {"git_commit": reference[1]["revision"], "workload_identity_sha256": identity,
                           "benchmark_executable_sha256": reference[1]["sha256"]}}
    if kind == "analytical":
        report["criterion_slope_samples_nanoseconds_per_iteration"] = heads
        report["baseline"]["criterion_slope_samples_nanoseconds_per_iteration"] = bases
        observed = observations(report)
    else:
        report["phase_completion_samples_nanoseconds"] = heads
        report["baseline"]["phase_completion_samples_nanoseconds"] = bases
        report["scaling"] = scaling_summary(heads, manifest)
        observed = dict(report)
    attestation = {"schema_version": 1, "kind": "controlled_timing_run" if kind == "analytical" else "controlled_claim_run",
                   "run_id": host.run_id + "-" + kind, "host_id": host.config["instance_id"],
                   "environment_id": environment, "started_at": started, "finished_at": now(),
                   "calibration_sha256": file_hash(calibration_path), "observations_sha256": digest(observed),
                   "exclusive_control": {"authority": "operator-installed root controller on a dedicated EC2 instance",
                       "lease_id": host.run_id, "mechanism": "flock controller lease; exclusive cgroup CPU partition; no swap, unrelated workloads or CPU steal",
                       "evidence_sha256": file_hash(host.output / "host-control.json")},
                   "resource_evidence": {entry.name: file_hash(entry)
                       for entry in sorted(host.output.glob(kind + "-*.resources.json"))}}
    path = save(host.output / (kind + "-run.json"), attestation)
    host.sign(path)
    if kind == "analytical":
        report.update(qualify(report, calibration_path, calibration_path.with_suffix(".json.sig"),
                             path, path.with_suffix(".json.sig"), CONTROL / "issuer.pub.pem"))
    else:
        if dt.datetime.fromisoformat(attestation["finished_at"]) > dt.datetime.fromisoformat(bounds["valid_until"]):
            raise QualificationError("claim comparison exceeded the independent calibration window")
        decisions = [{"benchmark": gate["benchmark"], "maximum": gate["maximum"],
                      **ratio_decision(gate["ratio"], gate["maximum"], names[gate["benchmark"]]["noise_factor"])}
                     for gate in gates]
        status = "regression" if any(row["decision"] == "regression" for row in decisions) else (
            "inconclusive" if any(row["decision"] == "inconclusive" for row in decisions) else "accepted")
        report.update(qualified_ratio_checks=decisions, acceptance_status=status, timing_acceptance=status == "accepted")
    host.sign(save(host.output / (kind + "-report.json"), report))
    return report


def execute(host, revision):
    host.git("fetch", "origin", "main")
    host.git("merge-base", "--is-ancestor", revision, "origin/main")
    analytical = json.loads((HERE / "analytical-manifest.json").read_text())
    claims = json.loads((HERE / "claim-timing.json").read_text())
    if (analytical["regression_protocol"].get("ordering") != "counterbalanced"
            or analytical["regression_protocol"].get("point_estimator") != "median_of_paired_slope_ratios"
            or claims["calibration_pairs"] != CALIBRATION_PAIRS
            or claims["comparison_pairs"] != COMPARISON_PAIRS
            or claims["ordering"] != "counterbalanced"):
        raise QualificationError("reviewed paired calibration protocol differs")
    head_source = host.source(revision, "head")
    for installed in HERE.glob("*.py"):
        candidate = head_source / "scripts" / installed.name
        if not candidate.is_file() or file_hash(candidate) != file_hash(installed):
            raise QualificationError(f"operator-installed controller differs: {installed.name}; review and deploy the controller update")
    if json.loads((head_source / "benchmarks/analytical/manifest.json").read_text()) != analytical:
        raise QualificationError("analytical baseline protocol changed; operator review and calibration are required")
    if json.loads((head_source / "benchmarks/regressions/claim-timing.json").read_text()) != claims:
        raise QualificationError("claim baseline protocol changed; operator review and calibration are required")
    source = host.source(REFERENCE, "reference")
    artifacts = host.build(REFERENCE, "reference", False)
    if json.loads((source / "benchmarks/analytical/manifest.json").read_text()) != analytical:
        raise QualificationError("immutable analytical reference workload differs")
    reference = (source, artifacts["analytical_comparison"])
    claim_artifacts = host.build(claims["reference_revision"], "claims-reference", True)
    # A claim-reference cache miss may have used the shared candidate checkout.
    head_source = host.source(revision, "head")
    head_artifacts = host.build(revision, "head", True)
    control = host.isolate()
    environment = digest(control)
    analytical_protocol = controlled_analytical_protocol(analytical["regression_protocol"])
    analytical_limits = {row["benchmark"]: row["max"] for row in analytical["regression_gates"]}
    claim_protocol = {"pairs": COMPARISON_PAIRS, "ordering": "counterbalanced",
                      "point_estimator": claims["comparison_estimator"]}
    claim_limits = {f"claims/{processes}/{keys}/{phase}": claims["max_regression_ratio"]
                    for processes in claims["processes"] for keys in claims["keys_per_process"] for phase in claims["phases"]}
    workloads = [
        ("analytical", (head_source, head_artifacts["analytical_comparison"]), reference,
         analytical, digest(analytical_identity(analytical)), analytical_protocol, analytical_limits),
        ("claims", (None, head_artifacts["row_claim_contention"]),
         (None, claim_artifacts["row_claim_contention"]), claims, digest(claims), claim_protocol, claim_limits)]
    calibrations = {kind: calibrate(host, kind, base, manifest, identity, protocol, limits, environment)
                    for kind, head, base, manifest, identity, protocol, limits in workloads}
    reports = {kind: compare(host, kind, head, base, manifest, identity, protocol, calibrations[kind], environment)
               for kind, head, base, manifest, identity, protocol, limits in workloads}
    host.verify_control()
    return {"schema_version": 1, "run_id": host.run_id, "head_revision": revision,
            "host_id": host.config["instance_id"], "environment_id": environment,
            "completed_at": now(), "reports": {kind: {"sha256": file_hash(host.output / (kind + "-report.json")),
                 "acceptance_status": report["acceptance_status"]} for kind, report in reports.items()}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--head", required=True)
    parser.add_argument("--run-id", required=True)
    args = parser.parse_args()
    if os.geteuid() != 0 or not re.fullmatch(r"[0-9a-f]{40}", args.head) or not re.fullmatch(r"[A-Za-z0-9][A-Za-z0-9-]{0,63}", args.run_id):
        parser.error("root controller, exact commit and bounded run identifier are required")
    config = json.loads(Path("/etc/uqa-performance.json").read_text())
    with (CONTROL / "lease").open("a") as lease:
        fcntl.flock(lease, fcntl.LOCK_EX | fcntl.LOCK_NB)
        host = ControlledHost(args.run_id, config)
        command("shutdown", "-h", "+180")
        try:
            result = execute(host, args.head)
        except Exception as error:
            result = {"schema_version": 1, "run_id": args.run_id, "head_revision": args.head,
                      "acceptance_status": "invalid", "error": str(error), "completed_at": now()}
        try:
            host.sign(save(host.output / "result.json", result))
            host.upload()
        finally:
            command("shutdown", "-h", "+1")
        print(json.dumps(result), flush=True)
        if result.get("acceptance_status") == "invalid":
            return 2
        statuses = [row["acceptance_status"] for row in result["reports"].values()]
        return 1 if "regression" in statuses else (3 if "inconclusive" in statuses else 0)


if __name__ == "__main__":
    sys.exit(main())
