//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered character edits write directly into reserved text and source-map buffers.

use regex::Regex;
use uqa_core::memory::{MemoryBudget, MemoryError};

use super::replacement::Replacement;
use crate::cooperative_regex::{CooperativeRegex, SearchError};
use crate::source::EditBuilder;
use crate::{AnalysisError, AnalysisResult, FilteredText};

pub(super) fn replace_html(
    text: &mut FilteredText<'_>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut index = 0;
        while index < input.len() {
            builder.check()?;
            let character = input[index..].chars().next().expect("valid UTF-8 boundary");
            if character != '<' {
                index += character.len_utf8();
                continue;
            }
            let mut cursor = index + character.len_utf8();
            let mut has_content = false;
            let mut end = None;
            let mut closed = false;
            while cursor < input.len() {
                builder.check()?;
                let character = input[cursor..]
                    .chars()
                    .next()
                    .expect("valid UTF-8 boundary");
                if character == '>' {
                    closed = true;
                    if has_content {
                        end = cursor.checked_add(character.len_utf8());
                    }
                    break;
                }
                has_content = true;
                cursor += character.len_utf8();
            }
            if let Some(end) = end {
                builder.edit(index..end, std::iter::once(" "))?;
                index = end;
            } else if !closed {
                break;
            } else {
                index += character.len_utf8();
            }
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

pub(super) fn replace_literal(
    text: &mut FilteredText<'_>,
    old: &str,
    new: &str,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        if old.is_empty() {
            for (index, _) in input.char_indices() {
                builder.check()?;
                builder.edit(index..index, std::iter::once(new))?;
            }
            builder.edit(input.len()..input.len(), std::iter::once(new))?;
        } else {
            let input_bytes = input.as_bytes();
            let old_bytes = old.as_bytes();
            if let Some(last_start) = input_bytes.len().checked_sub(old_bytes.len()) {
                let mut start = 0;
                let mut scanned = 0;
                while start <= last_start {
                    if scanned % 1024 == 0 {
                        builder.check()?;
                    }
                    if input.is_char_boundary(start)
                        && bytes_equal(input_bytes, old_bytes, start, &mut builder)?
                    {
                        let end = start + old_bytes.len();
                        builder.edit(start..end, std::iter::once(new))?;
                        start = end;
                    } else {
                        start += 1;
                    }
                    scanned += 1;
                }
            }
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

fn bytes_equal(
    input: &[u8],
    pattern: &[u8],
    start: usize,
    builder: &mut EditBuilder<'_, '_>,
) -> AnalysisResult<bool> {
    for (left, right) in input[start..start + pattern.len()]
        .chunks(1024)
        .zip(pattern.chunks(1024))
    {
        builder.check()?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

pub(super) fn replace_pattern(
    text: &mut FilteredText<'_>,
    pattern: &Regex,
    replacement: &Replacement<'_>,
    cooperative: Option<&CooperativeRegex>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    poll()?;
    // CaptureLocations owns two pointer-sized slots per group in the pinned regex implementation.
    let mut capture_memory = budget.empty_reservation();
    let mut locations = if replacement.uses_captures() {
        let bytes = pattern
            .captures_len()
            .checked_mul(2 * size_of::<usize>())
            .ok_or(MemoryError::SizeOverflow)?;
        capture_memory.grow(bytes)?;
        Some(pattern.capture_locations())
    } else {
        None
    };
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut start = 0;
        let mut last_end = None;
        loop {
            builder.check()?;
            let mut captures_resolved = false;
            let matched = if let Some(cooperative) = cooperative {
                match cooperative.find_at(input, start, &mut || builder.check()) {
                    Ok(matched) => matched,
                    Err(SearchError::Poll(error)) => return Err(error),
                    Err(SearchError::Automaton) => {
                        if let Some(locations) = &mut locations {
                            captures_resolved = true;
                            pattern
                                .captures_read_at(locations, input, start)
                                .map(|matched| matched.range())
                        } else {
                            pattern.find_at(input, start).map(|matched| matched.range())
                        }
                    }
                }
            } else if let Some(locations) = &mut locations {
                captures_resolved = true;
                pattern
                    .captures_read_at(locations, input, start)
                    .map(|matched| matched.range())
            } else {
                pattern.find_at(input, start).map(|matched| matched.range())
            };
            let Some(mut matched) = matched else {
                break;
            };
            if !captures_resolved {
                if let Some(locations) = &mut locations {
                    builder.check()?;
                    let Some(captured) = pattern
                        .captures_read_at(locations, input, matched.start)
                        .map(|capture| capture.range())
                    else {
                        return Err(AnalysisError::Descriptor(
                            "capture resolution returned no overall match",
                        ));
                    };
                    matched = captured;
                }
            }
            if matched.is_empty() && Some(matched.end) == last_end {
                if start == input.len() {
                    break;
                }
                start += 1;
                continue;
            }
            builder.edit(
                matched.clone(),
                replacement.fragments(locations.as_ref(), input),
            )?;
            start = matched.end;
            last_end = Some(start);
        }
        builder.finish()?
    };
    drop(locations);
    drop(capture_memory);
    text.apply_edited(edited, budget, poll)
}

#[cfg(test)]
mod tests {
    use super::{replace_html, replace_literal, replace_pattern};
    use crate::{AnalysisError, FilteredText};
    use regex::Regex;
    use uqa_core::memory::MemoryBudget;

    use super::super::replacement::Replacement;
    use crate::cooperative_regex::CooperativeRegex;

    #[test]
    fn literal_search_polls_inside_a_long_unmatched_input() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let budget = MemoryBudget::new(0);
        let mut polls = 0;
        replace_literal(&mut text, "needle", "replacement", &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert!(polls > input.len() / 2048);
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn literal_search_cancellation_releases_an_unpublished_edit() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let budget = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let result = replace_literal(&mut text, "needle", "replacement", &budget, &mut || {
            polls += 1;
            if polls == 8 {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn configured_pattern_scan_polls_through_a_long_unmatched_input() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let pattern = Regex::new("needle").unwrap();
        let replacement = Replacement::literal("#");
        let cooperative = CooperativeRegex::compile("needle");
        let budget = MemoryBudget::new(0);
        let mut polls = 0;
        replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &budget,
            &mut || {
                polls += 1;
                Ok(())
            },
        )
        .unwrap();
        assert!(polls > input.len() / 2048);
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn configured_pattern_scan_cancellation_releases_unpublished_edit() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let pattern = Regex::new("needle").unwrap();
        let replacement = Replacement::literal("#");
        let cooperative = CooperativeRegex::compile("needle");
        let budget = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let result = replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &budget,
            &mut || {
                polls += 1;
                if polls == 8 {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn capture_pattern_scan_polls_before_resolving_replacement_captures() {
        let prefix = "x".repeat(128 * 1024);
        let input = format!("{prefix}needle");
        let mut text = FilteredText::new(&input);
        let pattern = Regex::new("(needle)").unwrap();
        let replacement = Replacement::prepare("<$1>", &pattern);
        let cooperative = CooperativeRegex::compile("(needle)");
        let budget = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &budget,
            &mut || {
                polls += 1;
                Ok(())
            },
        )
        .unwrap();
        assert!(polls > input.len() / 2048);
        assert_eq!(text.as_str(), format!("{prefix}<needle>"));
    }

    #[test]
    fn capture_pattern_scan_cancellation_releases_unpublished_edit() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let pattern = Regex::new("(needle)").unwrap();
        let replacement = Replacement::prepare("<$1>", &pattern);
        let cooperative = CooperativeRegex::compile("(needle)");
        let budget = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let result = replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &budget,
            &mut || {
                polls += 1;
                if polls == 8 {
                    Err(AnalysisError::Cancelled)
                } else {
                    Ok(())
                }
            },
        );
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn capture_pattern_replacement_resolves_captures_after_range_search() {
        let input = "prefix foobar suffix";
        let mut text = FilteredText::new(input);
        let pattern = Regex::new("(foo)(bar)").unwrap();
        let replacement = Replacement::prepare("<$2-$1>", &pattern);
        let cooperative = CooperativeRegex::compile("(foo)(bar)");
        replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &MemoryBudget::new(1 << 20),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(text.as_str(), "prefix <bar-foo> suffix");
    }

    #[test]
    fn capture_pattern_preserves_anchors_after_a_nonzero_restart() {
        let input = "xab";
        let mut text = FilteredText::new(input);
        let pattern = Regex::new(r"(^ab|b|x)").unwrap();
        let replacement = Replacement::prepare("<$1>", &pattern);
        let cooperative = CooperativeRegex::compile(r"(^ab|b|x)");
        replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &MemoryBudget::new(1 << 20),
            &mut || Ok(()),
        )
        .unwrap();
        assert_eq!(text.as_str(), "<x>a<b>");
    }

    #[test]
    fn capture_pattern_reserves_all_capture_slots_before_search() {
        let input = "x".repeat(128 * 1024);
        let mut text = FilteredText::new(&input);
        let pattern = Regex::new("(needle)").unwrap();
        let replacement = Replacement::prepare("<$1>", &pattern);
        let cooperative = CooperativeRegex::compile("(needle)");
        let budget = MemoryBudget::new(0);
        let result = replace_pattern(
            &mut text,
            &pattern,
            &replacement,
            cooperative.as_ref(),
            &budget,
            &mut || Ok(()),
        );
        assert!(matches!(
            result,
            Err(crate::AnalysisError::Memory(
                uqa_core::memory::MemoryError::Limit { .. }
            ))
        ));
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn html_scan_matches_the_pinned_regex_on_small_inputs() {
        let regex = Regex::new(r"<[^>]+>").unwrap();
        let mut input = String::new();
        visit_html_inputs(&mut input, &regex, 5);
    }

    fn visit_html_inputs(input: &mut String, regex: &Regex, remaining: usize) {
        if remaining == 0 {
            let expected = regex.replace_all(input, " ");
            let mut text = FilteredText::new(input);
            replace_html(&mut text, &MemoryBudget::new(1 << 20), &mut || Ok(())).unwrap();
            assert_eq!(text.as_str(), expected.as_ref());
            return;
        }
        for character in ['<', '>', 'a', '🙂'] {
            input.push(character);
            visit_html_inputs(input, regex, remaining - 1);
            input.pop();
        }
    }

    #[test]
    fn html_scan_polls_through_a_long_unmatched_input() {
        let input = format!("{}x", "<".repeat(128 * 1024));
        let mut text = FilteredText::new(&input);
        let budget = MemoryBudget::new(0);
        let mut polls = 0;
        replace_html(&mut text, &budget, &mut || {
            polls += 1;
            Ok(())
        })
        .unwrap();
        assert!(polls > input.len() / 2048);
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }

    #[test]
    fn html_scan_cancellation_releases_an_unpublished_edit() {
        let input = format!("{}x", "<".repeat(128 * 1024));
        let mut text = FilteredText::new(&input);
        let budget = MemoryBudget::new(1 << 20);
        let mut polls = 0;
        let result = replace_html(&mut text, &budget, &mut || {
            polls += 1;
            if polls == 8 {
                Err(AnalysisError::Cancelled)
            } else {
                Ok(())
            }
        });
        assert!(matches!(result, Err(AnalysisError::Cancelled)));
        assert_eq!(text.as_str(), input);
        assert_eq!(budget.used(), 0);
    }
}
