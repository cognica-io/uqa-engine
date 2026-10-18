#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

import importlib.util
from pathlib import Path
import unittest


SPEC = importlib.util.spec_from_file_location(
    "emscripten_flags", Path(__file__).resolve().parents[1] / "emscripten-flags.py"
)
FLAGS = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(FLAGS)


class EmscriptenFlagsTests(unittest.TestCase):
    def test_supported_compilers_use_their_shipped_unwind_abi(self):
        for version, expected in [
            ("rustc 1.90.0 (1159e78c4 2025-09-14)", ""),
            ("rustc 1.92.0 (ded5c06cf 2025-12-08)", ""),
            ("rustc 1.93.0 (254b59607 2026-01-19)", "-fwasm-exceptions"),
            ("rustc 1.95.0 (59807616e 2026-04-14)\n", "-fwasm-exceptions"),
            ("rustc 1.95.0-beta.1 (abcdef012 2026-03-01)", "-fwasm-exceptions"),
            ("rustc 1.95.0-nightly (abcdef012 2026-03-01)", "-fwasm-exceptions"),
        ]:
            with self.subTest(version=version):
                self.assertEqual(FLAGS.c_flags(version), expected)

    def test_unknown_compiler_output_cannot_silently_select_an_abi(self):
        for version in ["", "clang version 20", "rustc unknown", "rustc 1.95"]:
            with self.subTest(version=version), self.assertRaises(ValueError):
                FLAGS.c_flags(version)
