#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reference-only finite-sample noise bounds for the eight-pair median protocol."""

from __future__ import annotations

from fractions import Fraction
import math

from performance_qualification import QualificationError, number


CALIBRATION_PAIRS = 32
COMPARISON_PAIRS = 8
REFERENCE_RANK_FROM_LARGEST = 3
ANALYTICAL_FILTER = r"^(analytical_external_(q1|q6)/uqa|analytical_result_scan/uqa_(cursor|materialized))$"


def reference_noise_bound(pairs: list[tuple[float, float]]) -> dict:
    """Return marginal prediction coverage under the documented exchangeability assumption."""
    if len(pairs) != CALIBRATION_PAIRS:
        raise QualificationError(f"noise calibration requires exactly {CALIBRATION_PAIRS} pairs")
    errors = []
    for left, right in pairs:
        ratio = Fraction(number(left, "reference slope")) / Fraction(number(right, "reference slope"))
        error = max(ratio, 1 / ratio)
        try:
            if not math.isfinite(float(error)):
                raise OverflowError
        except OverflowError as cause:
            raise QualificationError("reference pair ratio overflows") from cause
        errors.append(error)
    factor = sorted(errors, reverse=True)[REFERENCE_RANK_FROM_LARGEST - 1]
    try:
        rounded = float(factor)
    except OverflowError as error:
        raise QualificationError("reference noise factor overflows") from error
    if Fraction(rounded) < factor:
        rounded = math.nextafter(rounded, math.inf)
    if not math.isfinite(rounded):
        raise QualificationError("reference noise factor overflows")
    adverse = COMPARISON_PAIRS // 2
    ranks = adverse + REFERENCE_RANK_FROM_LARGEST - 1
    failures = sum(math.comb(COMPARISON_PAIRS, future) * math.comb(CALIBRATION_PAIRS, ranks - future)
                   for future in range(adverse, min(ranks, COMPARISON_PAIRS) + 1))
    coverage = 1 - Fraction(failures, math.comb(CALIBRATION_PAIRS + COMPARISON_PAIRS, ranks))
    confidence = float(coverage)
    if Fraction(confidence) > coverage:
        confidence = math.nextafter(confidence, 0.0)
    return {"noise_factor": rounded, "confidence_level": confidence,
            "calibration_pairs": CALIBRATION_PAIRS, "comparison_pairs": COMPARISON_PAIRS,
            "reference_rank_from_largest": REFERENCE_RANK_FROM_LARGEST,
            "coverage_kind": "marginal_prediction_under_exchangeable_multiplicative_errors"}


def controlled_analytical_protocol(manifest_protocol: dict) -> dict:
    """Keep the immutable workload's advisory protocol separate from controlled sampling."""
    return {**manifest_protocol, "pairs": COMPARISON_PAIRS, "benchmark_filter": ANALYTICAL_FILTER}
