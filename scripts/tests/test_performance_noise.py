#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

from fractions import Fraction
from itertools import product
import math
from pathlib import Path
import statistics
import sys
import unittest

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from performance_noise import reference_noise_bound
from performance_qualification import QualificationError, ratio_decision


class ReferenceNoiseTest(unittest.TestCase):
    def test_rank_bound_covers_every_adverse_future_rank_assignment(self):
        # Enumerate which of the six highest ranks are future observations, then
        # count completions among the remaining 34 ranks independently of the implementation.
        total = adverse = 0
        for ranks in product([False, True], repeat=6):
            future = sum(ranks)
            completions = math.comb(34, 8 - future)
            total += completions
            if future >= 4:
                adverse += completions
        self.assertEqual(total, math.comb(40, 8))
        self.assertEqual(Fraction(adverse, total), Fraction(87, 9139))
        result = reference_noise_bound([(100.0, 100.0)] * 32)
        self.assertLessEqual(Fraction(result['confidence_level']), Fraction(9052, 9139))
        self.assertGreaterEqual(Fraction(result['confidence_level']), Fraction(104, 105))
        self.assertEqual(result['reference_rank_from_largest'], 3)
        self.assertEqual(result['comparison_pairs'], 8)

    def test_every_observation_contributes_to_the_reviewed_order_statistic(self):
        for pairs in [[(100.0, 100.0)] * 29 + [(1000.0, 100.0), (100.0, 300.0), (200.0, 100.0)],
                      [(100.0, 100.0)] * 29 + [(100.0, 1000.0), (300.0, 100.0), (100.0, 200.0)]]:
            self.assertEqual(reference_noise_bound(pairs)['noise_factor'], 2)
        self.assertEqual(reference_noise_bound([(100, 100)] * 30 + [(10000, 100)] * 2)['noise_factor'], 1)
        self.assertEqual(reference_noise_bound([(100, 100)] * 29 + [(10000, 100)] * 3)['noise_factor'], 100)

    def test_median_resists_three_extreme_pairs_but_four_can_escape(self):
        bound = reference_noise_bound([(100, 100)] * 30 + [(10000, 100)] * 2)['noise_factor']
        for extreme in [0.001, 1000]:
            self.assertEqual(statistics.median([extreme] * 3 + [1.0] * 5), 1)
            self.assertNotEqual(statistics.median([extreme] * 4 + [1.0] * 4), 1)
        # A real 20% regression still fails the unchanged 10% limit even when
        # two calibration pairs contain extreme, fully retained reference noise.
        self.assertEqual(ratio_decision(1.2, 1.1, bound)['decision'], 'regression')

    def test_factor_rounding_is_outward(self):
        result = reference_noise_bound([(10.0, 9.0)] * 32)
        self.assertGreaterEqual(Fraction(result['noise_factor']), Fraction(10, 9))
        self.assertLess(Fraction(math.nextafter(result['noise_factor'], 0.0)), Fraction(10, 9))

    def test_incomplete_nonfinite_and_unrepresentable_reference_is_rejected(self):
        for count in [0, 31, 33]:
            with self.subTest(count=count), self.assertRaises(QualificationError):
                reference_noise_bound([(100.0, 100.0)] * count)
        for value in [0, -1, True, float('nan'), float('inf')]:
            with self.subTest(value=value), self.assertRaises(QualificationError):
                reference_noise_bound([(100.0, 100.0)] * 31 + [(value, 100.0)])
        # Invalid arithmetic cannot be hidden among values above the selected rank.
        with self.assertRaises(QualificationError):
            reference_noise_bound([(100.0, 100.0)] * 31 + [(1e308, 1e-308)])


if __name__ == '__main__':
    unittest.main()
