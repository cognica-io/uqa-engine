#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from fractions import Fraction
from itertools import combinations
import math
from pathlib import Path
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from performance_noise import reference_noise_bound
from performance_qualification import QualificationError


class ReferenceNoiseTest(unittest.TestCase):
    def test_rank_bound_covers_every_adverse_future_rank_assignment(self):
        # Enumerate exchangeable ranks independently of the calibration implementation.
        assignments = list(combinations(range(1, 37), 4))
        adverse = 0
        for future in assignments:
            calibration_max = max(rank for rank in range(1, 37) if rank not in future)
            if sum(rank > calibration_max for rank in future) >= 2:
                adverse += 1
        self.assertEqual(Fraction(adverse, len(assignments)), Fraction(1, 105))
        result = reference_noise_bound([(100.0, 100.0)] * 32)
        self.assertLessEqual(Fraction(result["confidence_level"]), Fraction(104, 105))
        self.assertGreaterEqual(result["confidence_level"], 0.99)

    def test_calibration_retains_outliers_and_both_ratio_directions(self):
        ordinary = [(100.0, 100.0)] * 31
        self.assertEqual(reference_noise_bound(ordinary + [(200.0, 100.0)])["noise_factor"], 2)
        self.assertEqual(reference_noise_bound(ordinary + [(100.0, 200.0)])["noise_factor"], 2)
        self.assertEqual(reference_noise_bound([(100.0, 100.0)] * 32)["noise_factor"], 1)

    def test_factor_rounding_is_outward(self):
        result = reference_noise_bound([(10.0, 9.0)] * 32)
        self.assertGreaterEqual(Fraction(result["noise_factor"]), Fraction(10, 9))
        self.assertLess(Fraction(math.nextafter(result["noise_factor"], 0.0)), Fraction(10, 9))

    def test_incomplete_nonfinite_and_unrepresentable_reference_is_rejected(self):
        for count in [0, 31, 33]:
            with self.subTest(count=count), self.assertRaises(QualificationError):
                reference_noise_bound([(100.0, 100.0)] * count)
        for value in [0, -1, True, float("nan"), float("inf")]:
            with self.subTest(value=value), self.assertRaises(QualificationError):
                reference_noise_bound([(value, 100.0)] * 32)
        with self.assertRaises(QualificationError):
            reference_noise_bound([(1e308, 1e-308)] * 32)


if __name__ == "__main__":
    unittest.main()
