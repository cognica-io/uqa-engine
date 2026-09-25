#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent, compact little-endian node/page format expectations."""

import argparse
import hashlib
import json
from pathlib import Path
import struct


PAGE_BYTES, HEADER_BYTES = 4096, 144
PAYLOAD_BYTES = PAGE_BYTES - HEADER_BYTES


def node(node_id, vector_bits, neighbors, norm_bits):
    prefix = struct.pack('<QQIIQ16sQQ', node_id, 41 + node_id,
                         3 if node_id == 1 else 0, norm_bits, len(neighbors),
                         bytes([0x11] * 16), 7, 9)
    assert len(prefix) == 64
    vectors = struct.pack('<' + 'I' * len(vector_bits), *vector_bits)
    adjacency = struct.pack('<QQ', *(neighbors + [0] * (2 - len(neighbors))))
    return prefix + vectors + adjacency


def page(payload, dimensions, node_count, page_id, first_node, slots, fragment_index, fragments):
    header = struct.pack('<8sII16sQQQQQQIIQIIII', b'UQADNPG\0', 1, 0,
                         bytes([0x22] * 16), 11, 12, 13, page_id, node_count,
                         2, dimensions, 0, first_node, slots, fragment_index,
                         fragments, len(payload))
    assert len(header) == 112
    body = payload + bytes(PAYLOAD_BYTES - len(payload))
    checksum = hashlib.sha256(header + body).digest()
    result = header + checksum + body
    assert len(result) == PAGE_BYTES
    return result


def fixture():
    small = [node(i, [0x80000000, 0x40400000, 0x40800000],
                  [j for j in range(3) if j != i], 0x40A00000) for i in range(3)]
    packed = page(b''.join(small), 3, 3, 0, 0, 3, 0, 1)
    large = node(0, [0x3F800000] + [0] * 1022 + [0x80000000], [], 0x3F800000)
    fragments = [large[start:start + PAYLOAD_BYTES] for start in range(0, len(large), PAYLOAD_BYTES)]
    frames = [page(body, 1024, 1, i, 0, 1, i, len(fragments)) for i, body in enumerate(fragments)]
    return {
        'fixture_revision': 1, 'page_bytes': PAGE_BYTES, 'header_bytes': HEADER_BYTES,
        'packed_node_hex': small[1].hex(), 'packed_page_header_hex': packed[:HEADER_BYTES].hex(),
        'packed_page_sha256': hashlib.sha256(packed).hexdigest(),
        'large_node_bytes': len(large), 'fragment_payload_bytes': [len(part) for part in fragments],
        'fragment_page_sha256': [hashlib.sha256(frame).hexdigest() for frame in frames],
    }


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = Path(__file__).with_name('pages.json')
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('DiskANN independent node and page byte fixtures match')


if __name__ == '__main__':
    main()
