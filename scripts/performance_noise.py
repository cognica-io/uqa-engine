#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reference-only finite-sample noise bounds for the four-pair median protocol."""

from __future__ import annotations

from fractions import Fraction
import math

from performance_qualification import QualificationError, number


CALIBRATION_PAIRS = 32
COMPARISON_PAIRS = 4
ANALYTICAL_FILTER = r"^(analytical_external_(q1|q6)/uqa|analytical_result_scan/uqa_(cursor|materialized))$"


def reference_noise_bound(pairs: list[tuple[float, float]]) -> dict:
    """Return marginal prediction coverage under the documented exchangeability assumption."""
    if len(pairs) != CALIBRATION_PAIRS:
        raise QualificationError(f"noise calibration requires exactly {CALIBRATION_PAIRS} pairs")
    factor = Fraction(1)
    for left, right in pairs:
        ratio = Fraction(number(left, "reference slope")) / Fraction(number(right, "reference slope"))
        factor = max(factor, ratio, 1 / ratio)
    try:
        rounded = float(factor)
    except OverflowError as error:
        raise QualificationError("reference noise factor overflows") from error
    if Fraction(rounded) < factor:
        rounded = math.nextafter(rounded, math.inf)
    if not math.isfinite(rounded):
        raise QualificationError("reference noise factor overflows")
    coverage = 1 - Fraction(COMPARISON_PAIRS * (COMPARISON_PAIRS - 1),
                            (CALIBRATION_PAIRS + COMPARISON_PAIRS)
                            * (CALIBRATION_PAIRS + COMPARISON_PAIRS - 1))
    confidence = float(coverage)
    if Fraction(confidence) > coverage:
        confidence = math.nextafter(confidence, 0.0)
    return {"noise_factor": rounded, "confidence_level": confidence,
            "calibration_pairs": CALIBRATION_PAIRS, "comparison_pairs": COMPARISON_PAIRS,
            "coverage_kind": "marginal_prediction_under_exchangeable_multiplicative_errors"}
