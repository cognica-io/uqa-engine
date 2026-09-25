#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent exact-rational expectations for declared Vamana initialization."""

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import struct

from generate import POINTS, distance, splitmix64

MASK = (1 << 64) - 1
ORDER_STREAM = 0x9E3779B97F4A7C15
ENTRY_STREAM = 0x94D049BB133111EB


def below(stream, bound):
    threshold = ((-bound) & MASK) % bound
    while True:
        value = next(stream)
        if value >= threshold:
            return value % bound


def sample(population, count, stream):
    selected = set()
    for upper in range(population - count, population):
        candidate = below(stream, upper + 1)
        selected.add(upper if candidate in selected else candidate)
    assert len(selected) == count
    return sorted(selected)


def graph(count, degree, seed):
    stream = splitmix64(seed)
    return [[offset + (offset >= node)
             for offset in sample(count - 1, min(degree, count - 1), stream)]
            for node in range(count)]


def order(count, seed):
    stream = splitmix64(seed ^ ORDER_STREAM)
    remaining = list(range(count))
    for index in range(count):
        selected = index + below(stream, count - index)
        remaining[index], remaining[selected] = remaining[selected], remaining[index]
    return remaining


def entry(points, seed):
    selected = sample(len(points), min(len(points), 256), splitmix64(seed ^ ENTRY_STREAM))
    center = [sum((points[i][j] for i in selected), Fraction(0)) / len(selected)
              for j in range(len(points[0]))]
    best = min(range(len(points)), key=lambda i: (distance(points[i], center), i))
    digest = hashlib.sha256(b''.join(struct.pack('<Q', i) for i in selected)).hexdigest()
    return {'entry': best, 'sample_count': len(selected), 'sample_u64le_sha256': digest,
            'centroid': [str(x) for x in center]}


def fixture():
    axes = [(Fraction(1), Fraction(0)), (Fraction(0), Fraction(1)),
            (Fraction(-1), Fraction(0)), (Fraction(0), Fraction(-1))]
    return {'revision': 1, 'seed': 42,
            'small': {'count': 5, 'degree': 3, 'initial_neighbors': graph(5, 3, 42),
                      'visit_order': order(5, 42), **entry(POINTS, 42)},
            'capped': {'count': 260, 'vectors': 'four signed coordinate axes repeated in order',
                       **entry([axes[i % 4] for i in range(260)], 42)}}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = Path(__file__).with_name('initialization.json')
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('Vamana independent initialization fixture matches')


if __name__ == '__main__':
    main()
