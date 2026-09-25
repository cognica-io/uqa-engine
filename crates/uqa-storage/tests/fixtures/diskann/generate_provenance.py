#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Independent revision-2 manifest bytes, fixed before the Rust codec."""

import argparse
import hashlib
import json
from pathlib import Path
import struct

DIRECTORY = Path(__file__).parent


def fixture():
    reference = json.loads((DIRECTORY / 'metadata.json').read_text(encoding='utf-8'))
    body = struct.pack('<8I', 5, 1, 1, 1, 1, 4096, 64, 144)
    alpha = struct.unpack('<Q', struct.pack('<d', 1.2))[0]
    body += struct.pack('<10Q', 2, 4, 8, alpha, 2, 2, 42, 8, 2, 0)
    body += bytes.fromhex(reference['coverage_sha256']) + struct.pack('<QQ', 10, 1)
    for name in ['codebook', 'codes', 'side', 'graph']:
        body += bytes.fromhex(reference[name + '_sha256'])
    assert len(body) == 288
    # Work revisions, coarse options/counts, merge options/counts, PQ/batch options, entry sample.
    words = [1, 1, 4, 4, 64, 3, 4, 42, 3, 10, 16, 0, 4, 1, 4, 2,
             8, 5, 20, 2, 42, 3, 2, 8]
    provenance = struct.pack('<24Q', *words) + bytes([0x55] * 32) + bytes([0x66] * 32)
    assert len(provenance) == 256
    body += provenance
    header = struct.pack('<8sII16sQQQQ', b'UQADNMF\0', 2, 96, bytes([0x22] * 16), 11, 12, 13, len(body))
    checksum = hashlib.sha256(header + body).digest()
    encoded = header + checksum + body
    assert len(encoded) == 640
    return {'fixture_revision': 1, 'manifest_revision': 2, 'provenance_words': words,
            'provenance_hex': provenance.hex(), 'manifest_checksum': checksum.hex(),
            'manifest_sha256': hashlib.sha256(encoded).hexdigest()}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--write', action='store_true')
    arguments = parser.parse_args()
    path = DIRECTORY / 'provenance.json'
    expected = fixture()
    if arguments.write:
        path.write_text(json.dumps(expected, indent=2) + '\n', encoding='utf-8')
    else:
        assert json.loads(path.read_text(encoding='utf-8')) == expected
        print('DiskANN independent build provenance manifest matches')
