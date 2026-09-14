#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify Japanese iteration marks and complete corrected offsets using the pinned Docker JVM."""

import subprocess

from run_tokenizer_reference import ROOT, docker_command, encoded, main, prepare_jars


def fields(case):
    text = ('a' * case['span_size'] + '々' * (case['span_size'] + 3) if 'span_size' in case
            else case.get('input', '') * case.get('repeat', 1))
    return [case.get('kind', 'text'), encoded(text),
            str(case.get('normalize_kanji', True)).lower(), str(case.get('normalize_kana', True)).lower(),
            str(case.get('chunk', 1024))]


if __name__ == '__main__':
    try:
        main(ROOT, prepare_jars, docker_command, 'iteration', 'KuromojiIterationReference.java', fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
