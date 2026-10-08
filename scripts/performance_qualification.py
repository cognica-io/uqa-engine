#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify externally issued timing evidence; never infer host control from samples."""

from __future__ import annotations

import datetime
from fractions import Fraction
import hashlib
import json
import math
from pathlib import Path
import re
import subprocess
import tempfile


class QualificationError(RuntimeError):
    pass


def digest(value: object) -> str:
    return hashlib.sha256(json.dumps(
        value, sort_keys=True, separators=(",", ":"), allow_nan=False,
    ).encode()).hexdigest()


def read_bounded(path: Path, maximum: int = 1024 * 1024) -> bytes:
    with path.open("rb") as source:
        contents = source.read(maximum + 1)
    if len(contents) > maximum:
        raise QualificationError(f"oversized qualification input: {path.name}")
    return contents


def unique_object(pairs: list[tuple[str, object]]) -> dict:
    result = {}
    for name, value in pairs:
        if name in result:
            raise QualificationError(f"duplicate qualification field: {name}")
        result[name] = value
    return result


def invalid_constant(value: str) -> None:
    raise QualificationError(f"non-finite qualification number: {value}")


def verified_document(path: Path, signature: Path, public_key: bytes) -> tuple[dict, str]:
    """Verify the same bounded byte snapshots that are decoded and attributed."""
    contents = read_bounded(path)
    signature_bytes = read_bounded(signature, 8192)
    with tempfile.TemporaryDirectory(prefix="uqa-timing-evidence-") as temporary:
        root = Path(temporary)
        key_snapshot, signature_snapshot = root / "issuer.pem", root / "signature"
        key_snapshot.write_bytes(public_key)
        signature_snapshot.write_bytes(signature_bytes)
        result = subprocess.run(
            ["openssl", "dgst", "-sha256", "-verify", str(key_snapshot),
             "-signature", str(signature_snapshot)],
            input=contents, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
            timeout=10, check=False,
        )
    if result.returncode:
        raise QualificationError(f"invalid qualification signature: {path.name}")
    try:
        document = json.loads(contents, object_pairs_hook=unique_object,
                              parse_constant=invalid_constant)
    except (ValueError, UnicodeError) as error:
        raise QualificationError(f"invalid qualification JSON: {path.name}") from error
    if (not isinstance(document, dict)
            or type(document.get("schema_version")) is not int
            or document["schema_version"] != 1):
        raise QualificationError("unsupported qualification document")
    return document, hashlib.sha256(contents).hexdigest()


def text(value: object, label: str) -> str:
    if not isinstance(value, str) or not value.strip():
        raise QualificationError(f"missing {label}")
    return value


def identifier(value: object, label: str, length: int = 64) -> str:
    if not isinstance(value, str) or not re.fullmatch(f"[0-9a-f]{{{length}}}", value):
        raise QualificationError(f"invalid {label}")
    return value


def number(value: object, label: str) -> float:
    if isinstance(value, bool) or not isinstance(value, (int, float)):
        raise QualificationError(f"invalid {label}")
    try:
        value = float(value)
    except OverflowError as error:
        raise QualificationError(f"out-of-range {label}") from error
    if not math.isfinite(value) or value <= 0:
        raise QualificationError(f"non-finite or non-positive {label}")
    return value


def timestamp(value: object, label: str) -> datetime.datetime:
    try:
        result = datetime.datetime.fromisoformat(text(value, label).replace("Z", "+00:00"))
    except ValueError as error:
        raise QualificationError(f"invalid {label}") from error
    if result.utcoffset() != datetime.timedelta(0):
        raise QualificationError(f"{label} must have an explicit UTC offset")
    return result


def observations(report: dict) -> dict:
    """The exact checked inputs, independent of optional presentation/provenance fields."""
    baseline = report.get("baseline")
    if not isinstance(baseline, dict):
        raise QualificationError("qualified timing requires paired baseline evidence")
    identity = identifier(report.get("workload_identity_sha256"), "workload digest")
    if baseline.get("workload_identity_sha256") != identity:
        raise QualificationError("qualified baseline workload differs")
    return {
        "workload_identity_sha256": identity,
        "head_revision": identifier(report.get("git_commit"), "head revision", 40),
        "baseline_revision": identifier(baseline.get("git_commit"), "baseline revision", 40),
        "head_executable_sha256": identifier(
            report.get("benchmark_executable_sha256"), "head executable digest"),
        "baseline_executable_sha256": identifier(
            baseline.get("benchmark_executable_sha256"), "baseline executable digest"),
        "regression_protocol": report["regression_protocol"],
        "head_samples": report["criterion_slope_samples_nanoseconds_per_iteration"],
        "baseline_samples": baseline["criterion_slope_samples_nanoseconds_per_iteration"],
        "regression_ratio_checks": report["regression_ratio_checks"],
    }


def ratio_decision(ratio: object, maximum: object, noise: object) -> dict:
    ratio = number(ratio, "observed ratio")
    maximum = number(maximum, "reviewed regression limit")
    noise = number(noise, "independent noise factor")
    if maximum < 1 or noise < 1:
        raise QualificationError("regression limit and noise factor must be at least one")
    # Compare exact binary-float values; rounding must not turn uncertainty into acceptance.
    lower = Fraction(ratio) / Fraction(noise)
    upper = Fraction(ratio) * Fraction(noise)
    limit = Fraction(maximum)
    low, high = ratio / noise, ratio * noise
    if low <= 0 or not math.isfinite(high):
        raise QualificationError("noise interval overflow or underflow")
    if Fraction(low) > lower:
        low = math.nextafter(low, 0.0)
    if Fraction(high) < upper:
        high = math.nextafter(high, math.inf)
    if low <= 0 or not math.isfinite(high):
        raise QualificationError("noise interval overflow or underflow")
    return {
        "noise_factor": noise, "ratio_interval": [low, high],
        "decision": "accepted" if upper <= limit else
                    "regression" if lower > limit else "inconclusive",
    }


def qualify(report: dict, calibration_path: Path, calibration_signature: Path,
            run_path: Path, run_signature: Path, issuer_key: Path, *,
            now: datetime.datetime | None = None) -> dict:
    """Require signed independent calibration and actual exclusive-control evidence."""
    key = read_bounded(issuer_key, 65536)
    calibration, calibration_hash = verified_document(
        calibration_path, calibration_signature, key)
    run, run_hash = verified_document(run_path, run_signature, key)
    if calibration.get("kind") != "independent_timing_calibration" or run.get("kind") != "controlled_timing_run":
        raise QualificationError("wrong timing evidence kind")
    calibration_id = text(calibration.get("calibration_id"), "calibration identity")
    run_id = text(run.get("run_id"), "measurement identity")
    if calibration_id == run_id:
        raise QualificationError("candidate execution cannot establish its own noise bound")
    for field in ("host_id", "environment_id"):
        if text(calibration.get(field), field) != text(run.get(field), field):
            raise QualificationError(f"timing evidence {field} differs")
    control = run.get("exclusive_control")
    if not isinstance(control, dict):
        raise QualificationError("missing actual exclusive-control attestation")
    for field in ("authority", "lease_id", "mechanism"):
        text(control.get(field), f"exclusive control {field}")
    provenance = calibration.get("independent_evidence")
    if not isinstance(provenance, dict):
        raise QualificationError("missing independent calibration evidence")
    text(provenance.get("reference"), "independent evidence reference")
    identifier(provenance.get("sha256"), "independent evidence digest")
    completed = timestamp(calibration.get("completed_at"), "calibration completion")
    valid_from = timestamp(calibration.get("valid_from"), "calibration validity start")
    valid_until = timestamp(calibration.get("valid_until"), "calibration validity end")
    started = timestamp(run.get("started_at"), "measurement start")
    finished = timestamp(run.get("finished_at"), "measurement finish")
    now = now or datetime.datetime.now(datetime.timezone.utc)
    if not completed <= valid_from <= started < finished <= valid_until or completed >= started:
        raise QualificationError("noise calibration is not independent or does not cover the run")
    if finished > now:
        raise QualificationError("measurement evidence names an unfinished run")
    observed = observations(report)
    if run.get("calibration_sha256") != calibration_hash:
        raise QualificationError("run names a different noise calibration")
    if calibration.get("workload_identity_sha256") != observed["workload_identity_sha256"]:
        raise QualificationError("noise calibration covers a different workload")
    if calibration.get("regression_protocol") != observed["regression_protocol"]:
        raise QualificationError("noise calibration sampling protocol differs")
    if run.get("observations_sha256") != digest(observed):
        raise QualificationError("attested observations, revisions or executables differ")
    limits = calibration.get("benchmarks")
    gates = report.get("regression_ratio_checks")
    if not isinstance(gates, list) or any(not isinstance(gate, dict) for gate in gates):
        raise QualificationError("invalid regression inventory")
    names = [text(gate.get("benchmark"), "regression benchmark") for gate in gates]
    if not isinstance(limits, dict) or not names or len(set(names)) != len(names) or set(limits) != set(names):
        raise QualificationError("calibration benchmark inventory differs")
    if calibration.get("estimator") != "median_of_paired_slope_ratios":
        raise QualificationError("noise calibration estimator differs")
    decisions = []
    for gate in gates:
        bound = limits[gate["benchmark"]]
        if not isinstance(bound, dict):
            raise QualificationError("invalid calibrated workload bound")
        maximum = number(bound.get("max_ratio"), "reviewed regression limit")
        if number(gate["maximum"], "candidate regression limit") != maximum:
            raise QualificationError("candidate changes a reviewed regression limit")
        confidence = number(bound.get("confidence_level"), "noise confidence")
        if not 0.99 <= confidence < 1:
            raise QualificationError("noise confidence must be at least 0.99 and below one")
        decision = ratio_decision(gate["ratio"], maximum, bound.get("noise_factor"))
        decisions.append({"benchmark": gate["benchmark"], "maximum": maximum,
                          "confidence_level": confidence, **decision})
    status = "regression" if any(gate["decision"] == "regression" for gate in decisions) else (
        "inconclusive" if any(gate["decision"] == "inconclusive" for gate in decisions) else "accepted")
    return {
        "timing_acceptance": status == "accepted", "acceptance_status": status,
        "qualification_missing": [], "qualified_ratio_checks": decisions,
        "qualification": {"calibration_id": calibration_id, "run_id": run_id,
                          "calibration_sha256": calibration_hash, "run_sha256": run_hash,
                          "issuer_public_key_sha256": hashlib.sha256(key).hexdigest()},
    }
