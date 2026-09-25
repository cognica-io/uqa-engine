#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent rational PQ beam order, specified before the Rust traversal."""

import argparse
from fractions import Fraction
import json
from pathlib import Path

# Declared centroids isolate traversal from the separate training oracle.
CENTROIDS = [(1, 0), (0, 1), (-1, 0), (0, -1)]
QUERY = (1, 0)
LABELS = [0, 1, 2, 3, 0, 2, 2, 1, 0]
NEIGHBORS = [[1, 2], [2, 4], [3], [4, 6], [5], [6, 7], [0, 3, 7], [8], [0, 2]]
DISTANCES = [sum((Fraction(x) - Fraction(y)) ** 2
                 for x, y in zip(QUERY, CENTROIDS[label])) for label in LABELS]


def traverse(capacity, width):
    frontier = [6]
    expanded = set()
    rounds = []
    order = []
    while True:
        selected = [node for node in frontier if node not in expanded][:width]
        if not selected:
            break
        before = list(frontier)
        for node in selected:
            assert node not in expanded
            expanded.add(node)
            order.append(node)
            frontier.extend(candidate for candidate in NEIGHBORS[node]
                            if candidate not in expanded and candidate not in frontier)
            frontier.sort(key=lambda candidate: (DISTANCES[candidate], candidate))
            del frontier[capacity:]
        rounds.append({'before': before, 'selected': selected, 'after': list(frontier)})
    completion = [node for node in range(len(NEIGHBORS)) if node not in expanded]
    assert sorted(order + completion) == list(range(len(NEIGHBORS)))
    return {'list': capacity, 'beam': width, 'rounds': rounds,
            'expanded': order, 'completion': completion}


def fixture():
    return {'revision': 1, 'query': QUERY, 'centroids': CENTROIDS, 'labels': LABELS,
            'distances': [int(d) for d in DISTANCES], 'neighbors': NEIGHBORS, 'entry': 6,
            'cases': [traverse(3, 1), traverse(3, 2), traverse(4, 3)]}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = Path(__file__).with_name('beams.json')
    encoded = json.dumps(fixture(), indent=2) + '\n'
    if arguments.write:
        path.write_text(encoded, encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == json.loads(encoded)
        print('DiskANN independent beam order fixture matches')


if __name__ == '__main__':
    main()
