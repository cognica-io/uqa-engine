#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent rational partition assignment and bounded window expectations."""

import argparse
from fractions import Fraction as F
import hashlib
import json
import struct
from pathlib import Path

centers = [(F(-1), F(0)), (F(0), F(1)), (F(1), F(0)), (F(0), F(-1))]
points = [(F(1), F(0)), (F(3, 5), F(4, 5)), (F(-1), F(0)), (F(0), F(-1))]


def nearest_two(point, centroids):
    return sorted(
        range(len(centroids)),
        key=lambda i: (sum((x - y) ** 2 for x, y in zip(point, centroids[i])), i),
    )[:2]


def windows(ids, capacity):
    assert capacity >= 2
    result = []
    for start in range(0, len(ids), capacity - 1):
        if start and len(ids) - start == 1:
            break
        result.append(ids[start:start + capacity])
    return result


assignments = [nearest_two(point, centers) for point in points]
assert assignments == [[2, 1], [1, 2], [0, 1], [3, 0]]
assert nearest_two((F(1), F(0)), [(F(1), F(0))] * 4) == [0, 1]
assert windows(list(range(10)), 4) == [[0, 1, 2, 3], [3, 4, 5, 6], [6, 7, 8, 9]]
assert windows(list(range(5)), 2) == [[0, 1], [1, 2], [2, 3], [3, 4]]
assert windows([7], 2) == [[7]]
assert windows([], 2) == []
expected = {
    'revision': 1,
    'centers': [[str(x) for x in p] for p in centers],
    'points': [[str(x) for x in p] for p in points],
    'nearest_two': assignments,
    'duplicate_centers_nearest_two': [0, 1],
    'capacity_four': windows(list(range(10)), 4),
    'capacity_two': windows(list(range(5)), 2),
}
expected['child_seeds'] = [
    [label, int.from_bytes(hashlib.sha256(
        b'UQA DiskANN partition child\0\x01' + struct.pack('<Q', 42) + bytes([label])
    ).digest()[:8], 'little')]
    for label in [0, 1, 2, 255]
]
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--write', action='store_true')
arguments = parser.parse_args()
path = Path(__file__).with_name('partitions.json')
if arguments.write:
    path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
else:
    assert json.loads(path.read_text(encoding='utf-8')) == expected
    print('DiskANN independent partition expectations match')
