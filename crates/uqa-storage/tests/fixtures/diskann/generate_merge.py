#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent rational expectations fixed before the Rust global merge."""

import argparse
from fractions import Fraction as F
import json
from pathlib import Path

points = [(F(1), F(0)), (F(-1), F(0)), (F(3, 5), F(4, 5)),
          (F(0), F(-1)), (F(0), F(1)), (F(0), F(-1))]
edges = [(4, 2), (0, 3), (1, 4), (0, 2), (4, 2), (2, 0), (4, 0),
         (2, 4), (1, 3), (0, 2), (4, 1), (0, 1), (0, 5), (5, 0)]


def distance(a, b):
    return sum((x - y) ** 2 for x, y in zip(points[a], points[b]))


neighbors = []
for source in range(len(points)):
    successor = (source + 1) % len(points)
    candidates = {b for a, b in edges if a == source and b not in (source, successor)}
    selected = []
    for candidate in sorted(candidates, key=lambda n: (distance(source, n), n)):
        if len(selected) == 2:
            break
        if all(F(6, 5) ** 2 * distance(n, candidate) > distance(source, candidate)
               for n in selected):
            selected.append(candidate)
    neighbors.append(sorted(selected + [successor]))
assert neighbors == [[1, 2, 3], [2, 3, 4], [0, 3, 4], [4], [1, 2, 5], [0]]
expected = {
    'revision': 1,
    'points': [[str(x) for x in point] for point in points],
    'logical_keys': [[11, 0], [11, 1], [20, 0], [30, 0], [99, 0], [100, 0]],
    'candidate_edges': [list(edge) for edge in edges],
    'max_degree': 3,
    'alpha': '6/5',
    'neighbors': neighbors,
}
parser = argparse.ArgumentParser(description=__doc__)
parser.add_argument('--write', action='store_true')
arguments = parser.parse_args()
path = Path(__file__).with_name('merge.json')
if arguments.write:
    path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
else:
    assert json.loads(path.read_text(encoding='utf-8')) == expected
    print('DiskANN independent global merge expectations match')
