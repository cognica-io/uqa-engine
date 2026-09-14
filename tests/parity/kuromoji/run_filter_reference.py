#!/usr/bin/env python3
#
# Unified Query Algebra
#
# Copyright (c) 2023-2026 Cognica, Inc.
#

"""Verify complete Japanese filter chains, analyzer output and normalization using Docker."""

import base64
import struct
import subprocess

from run_tokenizer_reference import ROOT, docker_command, encoded, main, prepare_jars


def text_bytes(text):
    if text is None:
        return struct.pack('>i', -1)
    units = text.encode('utf-16-be', errors='surrogatepass')
    return struct.pack('>i', len(units) // 2) + units


def strings(values):
    if values is None:
        return '-'
    return base64.b64encode(struct.pack('>i', len(values)) + b''.join(map(text_bytes, values))).decode('ascii')


def term_units(token):
    if 'term_utf16_pairs' in token:
        suffix = token.get('term_suffix_utf16', [])
        return [unit for first in range(*token['term_utf16_pairs'])
                for second in range(*token['term_utf16_pairs']) for unit in [first, second, *suffix, 0]]
    if 'term_utf16_range' in token:
        return [unit for first in range(*token['term_utf16_range'])
                for unit in [first, *token.get('term_separator_utf16', [])]]
    if 'term_utf16' in token:
        return token['term_utf16']
    raw = token['term'].encode('utf-16-be', errors='surrogatepass')
    return [value[0] for value in struct.iter_unpack('>H', raw)]


def fields(case):
    if 'input_utf16_range' in case:
        raw = b''.join(struct.pack('>H', unit) for unit in range(*case['input_utf16_range']))
    elif 'input_utf16' in case:
        raw = b''.join(struct.pack('>H', unit) for unit in case['input_utf16'])
    else:
        parts = case.get('input_parts', [{'text': case.get('input', ''), 'repeat': case.get('repeat', 1)}])
        raw = ''.join(part['text'] * part.get('repeat', 1) for part in parts).encode('utf-16-be')
    chain = []
    for stage in case.get('filters', []):
        kind = stage['type']
        if kind == 'kuromoji_part_of_speech':
            chain.append('pos:' + strings(stage.get('stop_tags')))
        elif kind == 'kuromoji_stop':
            chain.append('stop:' + str(stage.get('ignore_case', True)).lower() + ':' + strings(stage.get('words')))
        elif kind == 'kuromoji_stemmer':
            chain.append('stem:' + str(stage.get('minimum_length', 4)))
        elif kind == 'kuromoji_completion':
            chain.append('completion:' + stage.get('mode', 'index').upper())
        elif kind == 'kuromoji_readingform':
            chain.append('reading:' + str(stage.get('use_romaji', False)).lower())
        else:
            chain.append({'kuromoji_baseform': 'base', 'unicode_simple_lowercase': 'lower',
                          'kuromoji_number': 'number',
                          'kuromoji_hiragana_uppercase': 'hiragana_uppercase',
                          'kuromoji_katakana_uppercase': 'katakana_uppercase'}[kind])
    tokens = case.get('tokens', [])
    data = struct.pack('>i', len(tokens))
    for token in tokens:
        units = term_units(token)
        term = struct.pack('>i', len(units)) + b''.join(struct.pack('>H', unit) for unit in units)
        data += term + struct.pack('>iiii?', token['start_utf16'], token['end_utf16'], token.get('position_increment', 1), token.get('position_length', 1), token.get('keyword', False))
        data += b''.join(text_bytes(token.get(field)) for field in ['part_of_speech', 'base_form', 'reading', 'pronunciation', 'inflection_type', 'inflection_form'])
    return [case['kind'], base64.b64encode(raw).decode('ascii'), case.get('completion_mode', case.get('mode', 'search')).upper(),
            str(case.get('discard_punctuation', True)).lower(), str(case.get('discard_compound_token', True)).lower(),
            str(case.get('n_best_cost', 0)), '-' if case.get('user_dictionary') is None else encoded(case['user_dictionary']),
            ','.join(chain), base64.b64encode(data).decode('ascii'),
            str(case.get('final_offset_utf16', len(raw) // 2)), str(case.get('final_position_increment', 0))]


if __name__ == '__main__':
    try:
        main(ROOT, prepare_jars, docker_command, 'filter', 'KuromojiFilterReference.java', fields, __doc__)
    except subprocess.CalledProcessError as error:
        raise SystemExit(error.stderr or str(error)) from error
