#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent binary expectations from the rational PQ fixture and declared layouts."""

import argparse
from fractions import Fraction
import hashlib
import json
from pathlib import Path
import struct

from generate_pages import page


DIRECTORY = Path(__file__).parent
GENERATION = struct.pack('<16sQQQ', bytes([0x22] * 16), 11, 12, 13)
ORIGIN = struct.pack('<16sQQ', bytes([0x11] * 16), 7, 9)


def sha(data):
    return hashlib.sha256(data).digest()


def record(magic, body):
    header = struct.pack('<8sII', magic, 1, 96) + GENERATION + struct.pack('<Q', len(body))
    assert len(header) == 64
    return header + sha(header + body) + body


def fixture():
    reference = json.loads((DIRECTORY / 'training.json').read_text(encoding='utf-8'))
    centroids = [float(Fraction(value)) for chunk in reference['codebooks'] for row in chunk for value in row]
    pq_header = struct.pack('<IIHHIIIIIHHIQQQ', 5, 2, 2, 0, 1, 1, 1, 5, 20, 2, 0, 5, 42, 8, 10)
    assert len(pq_header) == 64
    book = record(b'UQADNPQ\0', pq_header + struct.pack('<III', 0, 3, 5) + struct.pack('<10d', *centroids))
    codes = bytes(label for row in reference['codes'] for label in row)
    coded = record(b'UQADNCD\0', struct.pack('<IIIIQQQ', 5, 2, 2, 1, 8, 0, 8) + sha(book) + codes)
    side_entries = b''.join(struct.pack('<QII', 200, ordinal, reason) + ORIGIN for ordinal, reason in [(0, 1), (1, 2)])
    side = record(b'UQADNSD\0', struct.pack('<IIQQQ', 5, 1, 2, 0, 2) + side_entries)
    rows = [struct.pack('<5f', *row) for row in reference['vectors']]
    coverage = b'UQA DiskANN canonical coverage\0\x01' + GENERATION + struct.pack('<I', 5)
    nodes = []
    for node_id, raw in enumerate(rows):
        coverage += struct.pack('<QI', 100 + node_id, 0) + ORIGIN + raw
        nodes.append(struct.pack('<QQIIQ', node_id, 100 + node_id, 0, 0x3F800000, 1) + ORIGIN + raw + struct.pack('<QQ', (node_id + 1) % 8, 0))
    for ordinal, raw in [(0, struct.pack('<5I', 0, 0, 0, 0, 0x80000000)), (1, struct.pack('<5I', 0x7F7FFFFF, 0, 0, 0, 0))]:
        coverage += struct.pack('<QI', 200, ordinal) + ORIGIN + raw
    coverage_hash = sha(coverage + struct.pack('<Q', 10))
    graph_page = page(b''.join(nodes), 5, 8, 0, 0, 8, 0, 1)
    graph_hash = sha(graph_page[112:144])
    manifest_body = struct.pack('<8I', 5, 1, 1, 1, 1, 4096, 64, 144)
    alpha_bits = struct.unpack('<Q', struct.pack('<d', 1.2))[0]
    manifest_body += struct.pack('<10Q', 2, 4, 8, alpha_bits, 2, 2, 42, 8, 2, 0)
    manifest_body += coverage_hash + struct.pack('<QQ', 10, 1)
    manifest_body += sha(book) + sha(codes) + sha(side_entries) + graph_hash
    assert len(manifest_body) == 288
    manifest = record(b'UQADNMF\0', manifest_body)
    return {
        'fixture_revision': 1,
        'coverage_sha256': coverage_hash.hex(),
        'manifest_sha256': sha(manifest).hex(),
        'codebook_sha256': sha(book).hex(),
        'codebook_header_hex': book[:160].hex(),
        'code_batch_sha256': sha(coded).hex(),
        'side_batch_sha256': sha(side).hex(),
        'side_entry_hex': side_entries.hex(),
        'graph_sha256': graph_hash.hex(),
        'codes_sha256': sha(codes).hex(),
        'side_sha256': sha(side_entries).hex(),
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = DIRECTORY / 'metadata.json'
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('DiskANN independent manifest, PQ, code and side fixtures match')


if __name__ == '__main__':
    main()
