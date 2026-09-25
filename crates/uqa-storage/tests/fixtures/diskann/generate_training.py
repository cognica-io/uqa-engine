#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Exact-rational PQ training oracle, independent of the Rust implementation."""

import argparse
from fractions import Fraction as F
import json
from pathlib import Path

from generate import distance, splitmix64


def bounded(random, bound):
    threshold = (1 << 64) % bound
    while True:
        value = next(random)
        if value >= threshold:
            return value % bound


def fixture():
    dimensions, chunks, centers, sample_limit, iterations, seed = 5, 2, 2, 5, 20, 42
    vectors = [[int(axis == j) * sign for j in range(dimensions)]
               for axis in (0, 1, 2, 4) for sign in (1, -1)]
    sample_ids = []
    random = splitmix64(seed)
    for index in range(len(vectors)):
        slot = index if index < sample_limit else bounded(random, index + 1)
        if index < sample_limit:
            sample_ids.append(index)
        elif slot < sample_limit:
            sample_ids[slot] = index
    sample = [vectors[index] for index in sample_ids]
    offsets = [i * (dimensions // chunks) + min(i, dimensions % chunks)
               for i in range(chunks + 1)]
    random = splitmix64(seed ^ 0xD1B54A32D192ED03)
    codebooks, initial_ids = [], []
    for start, end in zip(offsets, offsets[1:]):
        order = list(range(len(sample)))
        for label in range(centers):
            slot = label + bounded(random, len(sample) - label)
            order[label], order[slot] = order[slot], order[label]
        initial_ids.append([sample_ids[i] for i in order[:centers]])
        centroids = [[F(v) for v in sample[i][start:end]] for i in order[:centers]]
        previous = None
        for _ in range(iterations):
            labels = [min(range(centers), key=lambda i: (distance(row[start:end], centroids[i]), i))
                      for row in sample]
            if labels == previous:
                break
            previous = labels
            for label in range(centers):
                group = [row[start:end] for row, assigned in zip(sample, labels) if assigned == label]
                if group:
                    centroids[label] = [sum(F(row[j]) for row in group) / len(group)
                                        for j in range(end - start)]
        codebooks.append(centroids)
    codes = [[min(range(centers), key=lambda label: (distance(row[start:end], codebooks[chunk][label]), label))
              for chunk, (start, end) in enumerate(zip(offsets, offsets[1:]))] for row in vectors]
    query = [1, 0, 0, 0, 0]
    lookup = [[distance(query[start:end], centroid) for centroid in codebooks[chunk]]
              for chunk, (start, end) in enumerate(zip(offsets, offsets[1:]))]
    return {
        'fixture_revision': 1,
        'dimensions': dimensions, 'pq_bytes': chunks, 'max_centroids': centers,
        'max_samples': sample_limit, 'max_iterations': iterations, 'seed': seed,
        'vectors': vectors, 'sample_ids': sample_ids, 'initial_ids': initial_ids,
        'chunk_offsets': offsets,
        'codebooks': [[[str(x) for x in centroid] for centroid in chunk] for chunk in codebooks],
        'codes': codes, 'query': query,
        'lookup': [[str(x) for x in chunk] for chunk in lookup],
        'distances': [str(sum(lookup[chunk][label] for chunk, label in enumerate(code))) for code in codes],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = Path(__file__).with_name('training.json')
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('DiskANN rational PQ training fixture matches')


if __name__ == '__main__':
    main()
