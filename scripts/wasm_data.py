#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Read initialized data from UQA's single-memory wasm32 release artifacts.

The optimizer may split byte arrays around zero-filled memory. Compare embedded resources against their initialized memory, not adjacent bytes in the file.
Encoding: https://webassembly.github.io/spec/core/binary/modules.html#data-section
"""


class Reader:
    def __init__(self, data):
        self.data = memoryview(data)
        self.position = 0

    def take(self, count):
        end = self.position + count
        if count < 0 or end > len(self.data):
            raise RuntimeError("truncated WASM section or data")
        result = self.data[self.position:end]
        self.position = end
        return result

    def integer(self, signed=False):
        value = 0
        for shift in range(0, 35, 7):
            byte = self.take(1)[0]
            value |= (byte & 127) << shift
            if byte < 128:
                if signed and byte & 64:
                    value -= 1 << (shift + 7)
                lower, upper = (-(1 << 31), 1 << 31) if signed else (0, 1 << 32)
                if lower <= value < upper:
                    return value
                break
        raise RuntimeError("invalid WASM 32-bit LEB128 integer")

    def done(self):
        return self.position == len(self.data)


def initialized_data(blob, limit=256 * 1024 * 1024):
    reader = Reader(blob)
    if reader.take(8) != b"\0asm\x01\0\0\0":
        raise RuntimeError("invalid WASM binary header")
    memory_size = None
    segments = []
    seen = set()
    expected_count = None
    while not reader.done():
        section_id = reader.take(1)[0]
        section = Reader(reader.take(reader.integer()))
        if section_id not in (5, 11, 12):
            continue
        if section_id in seen:
            raise RuntimeError("duplicate WASM memory/data section")
        seen.add(section_id)
        if section_id == 5:
            if section.integer() != 1:
                raise RuntimeError("release inventory requires one WASM memory")
            flags = section.integer()
            if flags not in (0, 1):
                raise RuntimeError("release inventory requires unshared wasm32 memory")
            minimum = section.integer()
            maximum = section.integer() if flags else 65536
            if not minimum <= maximum <= 65536:
                raise RuntimeError("invalid WASM memory limits")
            memory_size = minimum * 65536
        elif section_id == 12:
            expected_count = section.integer()
        else:
            count = section.integer()
            for _ in range(count):
                flags = section.integer()
                if flags not in (0, 2) or (flags == 2 and section.integer() != 0):
                    raise RuntimeError("release inventory requires active WASM data in memory zero")
                if section.take(1)[0] != 0x41:
                    raise RuntimeError("release inventory requires constant WASM data offsets")
                offset = section.integer(signed=True) & 0xffffffff
                if section.take(1)[0] != 0x0b:
                    raise RuntimeError("invalid WASM offset expression")
                data = section.take(section.integer())
                if memory_size is None or offset + len(data) > min(memory_size, limit):
                    raise RuntimeError("WASM initialized data exceeds memory or inventory allowance")
                segments.append((offset, data))
        if not section.done():
            raise RuntimeError("trailing WASM section bytes")
    if expected_count is not None and expected_count != len(segments):
        raise RuntimeError("WASM data count differs from data section")
    memory = bytearray(max((offset + len(data) for offset, data in segments), default=0))
    for offset, data in segments:
        memory[offset:offset + len(data)] = data
    return memory
