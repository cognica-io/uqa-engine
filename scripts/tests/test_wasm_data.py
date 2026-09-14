#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import unittest

from scripts.wasm_data import Reader, initialized_data


def leb(value):
    result = bytearray()
    while value >= 128:
        result.append((value & 127) | 128)
        value >>= 7
    return bytes(result) + bytes([value])


def section(kind, data):
    return bytes([kind]) + leb(len(data)) + data


def module(segments, pages=1):
    data = leb(len(segments))
    for offset, content in segments:
        data += b"\x00\x41" + leb(offset) + b"\x0b" + leb(len(content)) + content
    return b"\0asm\x01\0\0\0" + section(5, b"\x01\x00" + leb(pages)) + section(11, data)


class WASMDataTest(unittest.TestCase):
    def test_segment_gaps_and_later_writes_reconstruct_exact_memory(self):
        blob = module([(2, b"UQA"), (9, b"end"), (3, b"qa")])
        self.assertEqual(initialized_data(blob), b"\0\0Uqa\0\0\0\0end")
        # Bytes in an ignored custom section are not initialized runtime data.
        self.assertEqual(initialized_data(blob + section(0, b"\0dictionary decoy")), initialized_data(blob))

    def test_truncated_overflowing_and_unsupported_data_is_rejected(self):
        valid = module([(2, b"UQA")])
        invalid = [valid[:7], valid[:-1], valid + b"\x00\x80", valid + section(11, b"\0"),
                   valid + section(12, b"\x02"), module([(65535, b"overflow")]),
                   module([(0x20000000, b"far")], pages=65536)]
        for data in (b"\x01\x01\x00", b"\x01\x02\x01", b"\x01\x00\x23\x00\x0b\x00",
                     b"\x01\x00\x41\x7f\x0b\x00", b"\x01\x00\x41\x00\x00\x00"):
            invalid.append(b"\0asm\x01\0\0\0" + section(5, b"\x01\x00\x01") + section(11, data))
        for blob in invalid:
            with self.subTest(blob=blob[:20]), self.assertRaises(RuntimeError):
                initialized_data(blob)
        with self.assertRaisesRegex(RuntimeError, "allowance"):
            initialized_data(valid, limit=4)

    def test_integer_width_sign_and_permitted_padding_are_checked(self):
        self.assertEqual(Reader(b"\x83\x00").integer(), 3)
        self.assertEqual(Reader(b"\xff\xff\xff\xff\x7f").integer(signed=True), -1)
        self.assertEqual(Reader(b"\xff\xff\xff\xff\x0f").integer(), 0xffffffff)
        for value, signed in [(b"\x80" * 6, False), (b"\xff\xff\xff\xff\x10", False),
                              (b"\xff\xff\xff\xff\x0f", True), (b"\x80", True)]:
            with self.subTest(value=value, signed=signed), self.assertRaises(RuntimeError):
                Reader(value).integer(signed=signed)


if __name__ == "__main__":
    unittest.main()
