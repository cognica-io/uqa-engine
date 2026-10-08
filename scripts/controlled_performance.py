#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Validate the fixed controlled-runner workloads and summarize complete observations."""

from __future__ import annotations

import json
from pathlib import Path
import statistics

from performance_qualification import QualificationError, number


def analytical_observations(root: Path, names: list[str], sample_count: int) -> dict[str, float]:
    result = {}
    for name in names:
        directory = root.joinpath(*name.split("/"), "new")
        estimate = json.loads((directory / "estimates.json").read_text())
        samples = json.loads((directory / "sample.json").read_text())
        if samples.get("sampling_mode") != "Linear":
            raise QualificationError("controlled analytical samples must use linear sampling")
        iterations, times = samples.get("iters", []), samples.get("times", [])
        if len(iterations) != sample_count or len(times) != sample_count:
            raise QualificationError("controlled analytical sample count changed")
        for value in iterations + times:
            number(value, "linear sample")
        result[name] = number(estimate["slope"]["point_estimate"], "Criterion slope")
    return result


def claim_observations(report: dict, manifest: dict) -> dict[str, float]:
    if report.get("schema_version") != 1 or report.get("measurement") is not True:
        raise QualificationError("claim timing requires the measurement workload")
    expected = {(processes, keys) for processes in manifest["processes"]
                for keys in manifest["keys_per_process"]}
    seen = set()
    result = {}
    for case in report.get("cases", []):
        identity = (case.get("processes"), case.get("keys_per_process"))
        if any(type(value) is not int for value in identity) or identity not in expected or identity in seen:
            raise QualificationError("claim timing case inventory changed")
        seen.add(identity)
        processes, keys = identity
        if (case.get("correctness") is not True
                or case.get("warmup_rounds") != manifest["warmup_rounds"]
                or case.get("rounds") != manifest["measurement_rounds"]
                or len(case.get("samples", [])) != manifest["measurement_rounds"]):
            raise QualificationError("claim timing correctness or sample count changed")
        for phase, wall_name in (("acquire", "acquisition_wall_ns"), ("release", "release_wall_ns")):
            windows = []
            for sample in case["samples"]:
                window = number(sample.get(wall_name), "claim phase completion")
                workers = sample.get(phase + "_ns", [])
                if len(workers) != processes or any(number(value, "worker latency") > window for value in workers):
                    raise QualificationError("claim completion window does not cover every worker")
                windows.append(window)
            result[f"claims/{processes}/{keys}/{phase}"] = statistics.median(windows)
    if seen != expected:
        raise QualificationError("claim timing is missing a required process/key case")
    return result


def scaling_summary(samples: dict[str, list[float]], manifest: dict) -> list[dict]:
    result = []
    for keys in manifest["keys_per_process"]:
        for phase in manifest["phases"]:
            single = statistics.median(samples[f"claims/1/{keys}/{phase}"])
            for processes in manifest["processes"]:
                elapsed = statistics.median(samples[f"claims/{processes}/{keys}/{phase}"])
                result.append({"processes": processes, "keys_per_process": keys, "phase": phase,
                               "median_completion_ns": elapsed,
                               "keys_per_second": processes * keys * 1e9 / elapsed,
                               "throughput_relative_to_one_process": processes * single / elapsed})
    return result
