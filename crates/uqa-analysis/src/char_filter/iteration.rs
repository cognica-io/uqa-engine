//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese iteration spans read original characters and stop at full stops and surrogate pairs.

use crate::source::EditBuilder;
use crate::{AnalysisResult, FilteredText};
use uqa_core::memory::MemoryBudget;

const HIRAGANA_VOICED: [char; 50] = [
    'が', 'が', 'ぎ', 'ぎ', 'ぐ', 'ぐ', 'げ', 'げ', 'ご', 'ご', 'ざ', 'ざ', 'じ', 'じ', 'ず', 'ず',
    'ぜ', 'ぜ', 'ぞ', 'ぞ', 'だ', 'だ', 'ぢ', 'ぢ', 'っ', 'づ', 'づ', 'で', 'で', 'ど', 'ど', 'な',
    'に', 'ぬ', 'ね', 'の', 'ば', 'ば', 'ぱ', 'び', 'び', 'ぴ', 'ぶ', 'ぶ', 'ぷ', 'べ', 'べ', 'ぺ',
    'ぼ', 'ぼ',
];

fn normalize(source: char, mark: char) -> char {
    let (start, voiced) = match mark {
        'ゝ' => ('か', false),
        'ゞ' => ('か', true),
        'ヽ' => ('カ', false),
        'ヾ' => ('カ', true),
        _ => return source,
    };
    let Some(index) = (source as u32).checked_sub(start as u32) else {
        return source;
    };
    let Some(&mapped) = HIRAGANA_VOICED.get(index as usize) else {
        return source;
    };
    let mapped =
        char::from_u32(mapped as u32 + start as u32 - 'か' as u32).expect("voiced kana scalar");
    if voiced {
        mapped
    } else if mapped == source {
        // The reference predicate includes unchanged table entries, including small and unvoiced kana.
        char::from_u32(source as u32 - 1).expect("preceding kana scalar")
    } else {
        source
    }
}

pub(super) fn replace(
    text: &mut FilteredText<'_>,
    normalize_kanji: bool,
    normalize_kana: bool,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    if !normalize_kanji && !normalize_kana {
        return Ok(());
    }
    let mark = |character| {
        (normalize_kanji && character == '々')
            || (normalize_kana && matches!(character, 'ゝ' | 'ゞ' | 'ヽ' | 'ヾ'))
    };
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut position = 0;
        let mut span_end = 0;
        let mut source = input.chars();
        for (start, original) in input.char_indices() {
            builder.check()?;
            let units = original.len_utf16();
            if units == 2 || original == '。' {
                span_end = position + units;
            }
            let mut replacement = original;
            if mark(original) {
                if position == span_end {
                    span_end += 1;
                } else {
                    if position > span_end {
                        let mut span = 0;
                        for next in input[start..].chars() {
                            builder.check()?;
                            if !mark(next) {
                                break;
                            }
                            span += 1;
                        }
                        span = span.min(position - span_end);
                        span_end = position + span;
                        let mut source_start = start;
                        for previous in input[..start].chars().rev().take(span) {
                            builder.check()?;
                            // Surrogate pairs delimit spans, so every referenced character has one UTF-16 unit.
                            debug_assert_eq!(previous.len_utf16(), 1);
                            source_start -= previous.len_utf8();
                        }
                        source = input[source_start..].chars();
                    }
                    replacement =
                        normalize(source.next().expect("original iteration source"), original);
                }
            }
            if replacement != original {
                let mut buffer = [0; 4];
                builder.edit(
                    start..start + original.len_utf8(),
                    std::iter::once(&*replacement.encode_utf8(&mut buffer)),
                )?;
            }
            position += units;
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

#[cfg(test)]
mod tests;
