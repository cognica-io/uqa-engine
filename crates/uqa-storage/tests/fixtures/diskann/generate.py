#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent rational-arithmetic fixtures; never import the Rust implementation."""

import argparse
from fractions import Fraction as F
import hashlib
import json
from pathlib import Path
import struct


REVISION = 1
POINTS = [(F(1), F(0)), (F(-1), F(0)), (F(3, 5), F(4, 5)),
          (F(0), F(-1)), (F(0), F(1))]
INITIAL = [[1, 2, 3], [0, 2, 4], [0, 1, 4], [0, 2, 4], [1, 2, 3]]
ORDER = [2, 4, 1, 3, 0]


def distance(left, right):
    return sum((a - b) ** 2 for a, b in zip(left, right, strict=True))


def prune(points, source, candidates, alpha, degree):
    remaining = set(candidates) - {source}
    selected = []
    while remaining and len(selected) < degree:
        nearest = min(remaining, key=lambda i: (distance(points[source], points[i]), i))
        selected.append(nearest)
        remaining = {i for i in remaining
                     if alpha ** 2 * distance(points[nearest], points[i])
                     > distance(points[source], points[i])}
    return selected


def visited(graph, source, query, capacity):
    active, expanded = {source}, set()
    priority = lambda i: (distance(POINTS[i], query), i)
    while active - expanded:
        current = min(active - expanded, key=priority)
        expanded.add(current)
        active.update(graph[current])
        active = set(sorted(active, key=priority)[:capacity])
    return sorted(expanded)


def graph_fixture():
    graph = [neighbors.copy() for neighbors in INITIAL]
    centroid = [sum(p[j] for p in POINTS) / len(POINTS) for j in range(2)]
    entry = min(range(len(POINTS)), key=lambda i: (distance(POINTS[i], centroid), i))
    passes = []
    for alpha in (F(1), F(2)):
        for source in ORDER:
            candidates = set(visited(graph, entry, POINTS[source], 4)) | set(graph[source])
            graph[source] = prune(POINTS, source, candidates, alpha, 3)
            for neighbor in graph[source]:
                reverse = set(graph[neighbor]) | {source}
                graph[neighbor] = (prune(POINTS, neighbor, reverse, alpha, 3)
                                   if len(reverse) > 3 else sorted(reverse))
        passes.append([sorted(neighbors) for neighbors in graph])
    connected = []
    for source, neighbors in enumerate(graph):
        successor = (source + 1) % len(POINTS)
        selected = prune(POINTS, source, set(neighbors) - {successor}, F(2), 2)
        connected.append(sorted([successor, *selected]))
    return {
        "points": [[str(x) for x in p] for p in POINTS],
        "logical_keys": [[11, 0], [11, 1], [20, 0], [30, 0], [99, 0]],
        "initial_neighbors": INITIAL, "visit_order": ORDER, "entry": entry,
        "build_list_size": 4, "max_degree": 3, "alpha": "2",
        "unaugmented_passes": passes, "with_reserved_cycle": connected,
    }


def splitmix64(seed):
    mask = (1 << 64) - 1
    while True:
        seed = (seed + 0x9E3779B97F4A7C15) & mask
        value = seed
        value = ((value ^ (value >> 30)) * 0xBF58476D1CE4E5B9) & mask
        value = ((value ^ (value >> 27)) * 0x94D049BB133111EB) & mask
        yield value ^ (value >> 31)


def recall_inputs():
    centers = splitmix64(42)
    centers = [[(next(centers) >> 32) % 2049 - 1024 for _ in range(32)]
               for _ in range(32)]

    def vectors(count, seed):
        noise = splitmix64(seed)
        return [[16 * center + (next(noise) >> 32) % 65 - 32
                 for center in centers[index % 32]] for index in range(count)]

    return vectors(4096, 43), vectors(128, 24301)


def fingerprint(vectors):
    digest = hashlib.sha256()
    for vector in vectors:
        digest.update(struct.pack('<' + 'f' * len(vector), *vector))
    return digest.hexdigest()


def fixture():
    geometry = [POINTS[0], POINTS[2], POINTS[1]]
    assert distance(geometry[0], geometry[1]) == F(4, 5)
    assert distance(geometry[1], geometry[2]) == F(16, 5)
    assert prune(geometry, 0, [0, 1, 2, 1], F(6, 5), 2) == [1, 2]
    assert prune(geometry, 0, [1, 2], F(1), 2) == [1]
    corpus, queries = recall_inputs()
    assert not (set(map(tuple, corpus)) & set(map(tuple, queries)))
    result = {
        "fixture_revision": REVISION,
        "pruning": {
            "points": [[str(x) for x in p] for p in geometry],
            "source": 0, "candidates": [0, 1, 2, 1], "max_degree": 2,
            "alpha": "6/5", "expected_neighbors": [1, 2],
            "alpha_one_neighbors": [1], "incorrect_unsquared_factor_neighbors": [1],
        },
        "pq": {
            "dimensions": 5, "pq_bytes": 2, "chunk_offsets": [0, 3, 5],
            "codebooks": [[[0, 0, 0], [1, 0, 1]], [[0, 0], [0, 2]]],
            "query": [1, 0, 1, 0, 0], "lookup": [[2, 0], [0, 4]],
            "codes": [[0, 0], [1, 0], [0, 1], [1, 1]],
            "distances": [2, 0, 6, 4],
            "tie_query": [1, 0, 0, 0, 1], "tie_code": [0, 0],
        },
        "scores": {
            "query": [1, 0],
            "documents": [
                {"id": 2, "vectors": [[0, 1], [1, 0]], "score_bits": "3f800000"},
                {"id": 3, "vectors": [[0, 0]], "score_bits": "00000000"},
                {"id": 4, "vectors": [[-1, 0]], "score_bits": "bf800000"},
                {"id": 5, "vectors": [], "score_bits": None},
                {"id": 7, "vectors": [[1, 0]], "score_bits": "3f800000"},
                {"id": 9, "vectors": [[3, 4]], "score_bits": "3f19999a"},
            ],
            "ranked_doc_ids": [2, 7, 9, 3, 4], "posting_doc_ids": [2, 3, 4, 7, 9],
        },
        "graph": graph_fixture(),
        "recall_workload": {
            "generator": "SplitMix64 integer clustered vectors, generate.py revision 1",
            "dimensions": 32, "corpus_count": 4096, "query_count": 128,
            "center_seed": 42, "corpus_seed": 43, "query_seed": 24301,
            "corpus_f32le_sha256": fingerprint(corpus),
            "query_f32le_sha256": fingerprint(queries),
            "k": 10, "minimum_mean_recall": 0.95, "score_ties": "ascending DocId",
            "ground_truth": "canonical raw f32 cosine, exhaustive document ranking",
        },
    }
    pq = result['pq']
    for chunk, (start, end) in enumerate(zip(pq['chunk_offsets'], pq['chunk_offsets'][1:])):
        assert [distance(pq['query'][start:end], c) for c in pq['codebooks'][chunk]] == pq['lookup'][chunk]
        assert min(range(2), key=lambda i: (distance(pq['tie_query'][start:end], pq['codebooks'][chunk][i]), i)) == pq['tie_code'][chunk]
    assert [sum(pq['lookup'][i][label] for i, label in enumerate(code)) for code in pq['codes']] == pq['distances']
    assert result['graph']['unaugmented_passes'][0] != result['graph']['unaugmented_passes'][1]
    assert result['graph']['unaugmented_passes'][1] != result['graph']['with_reserved_cycle']
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = Path(__file__).with_name('reference.json')
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('DiskANN independent fixtures and held-out workload fingerprints match')


if __name__ == '__main__':
    main()
