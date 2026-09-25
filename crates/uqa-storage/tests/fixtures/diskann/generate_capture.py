#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Reproduce the canonical capture expectation fixed before the Rust consumer."""

import hashlib
import struct


def main():
    records = [
        (10, 0, [0x40400000, 0x40800000]),
        (10, 1, [0x80000000, 0]),
        (20, 0, [0x7F7FFFFF, 0x7F7FFFFF]),
        (21, 0, [0xBF800000, 0]),
        (21, 1, [0x00800000, 0]),
    ]
    digest = hashlib.sha256(
        b'UQA DiskANN canonical coverage\0\x01'
        + bytes([1]) * 16 + struct.pack('<QQQI', 2, 3, 4, 2))
    for doc, ordinal, bits in records:
        digest.update(struct.pack('<QI', doc, ordinal) + bytes([9]) * 16
                      + struct.pack('<QQ', 7, 3) + struct.pack('<II', *bits))
    digest.update(struct.pack('<Q', len(records)))
    assert digest.hexdigest() == '7e20a0b4972bf9be57997a63789ddd7b79c73f6e5f248748eb9f256a211dd2f1'
    print('DiskANN independent capture coverage expectation matches')


if __name__ == '__main__':
    main()
