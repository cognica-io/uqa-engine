//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese filter decisions are identical over raw and source-mapped tokens.

use super::super::{
    error::{check_limit, invalid},
    KuromojiDictionary, KuromojiLimits,
};
use super::{stream::JapaneseToken, words::lowercase, CompiledFilter};
use crate::morphology::filter::{text_units, AllocatedStream, Work};
use crate::AnalysisResult;
use uqa_core::memory::Budgeted;

impl CompiledFilter {
    pub(super) fn apply_owned<T: JapaneseToken>(
        &self,
        mut input: AllocatedStream<T>,
        model: &KuromojiDictionary,
        limits: KuromojiLimits,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<AllocatedStream<T>> {
        poll()?;
        check_limit(
            "Kuromoji output tokens",
            input.batch.tokens().len(),
            limits.max_tokens,
        )?;
        check_limit(
            "Kuromoji input UTF-16 units",
            input.final_offset_utf16,
            limits.max_input_utf16,
        )?;
        let budget = input.batch.budget().clone();
        let mut output_units = 0;
        input.batch = if matches!(self, Self::PartOfSpeech(_) | Self::Stop(_, _)) {
            input.batch.retain(
                |token, poll| {
                    let mut work = Work::new(poll)?;
                    let keep = self.keep(token, model, &budget, &mut work)?;
                    if keep {
                        let length = token.term_len(&mut work)?;
                        output_units = units(token, length, output_units, limits, &mut work)?;
                    }
                    work.finish()?;
                    Ok(keep)
                },
                poll,
            )?
        } else {
            let mut work = Work::new(poll)?;
            input.batch.map_tokens(|token, memory| {
                work.tick()?;
                let mut length = token.term_len(&mut work)?;
                match self {
                    Self::BaseForm if !token.keyword() => {
                        if let Some(base) = token.base_form() {
                            length = text_units(base, &mut work)?;
                            units(token, length, output_units, limits, &mut work)?;
                            let term = crate::morphology::input::encode(
                                base,
                                memory.budget(),
                                work.poll,
                                |_| Ok(()),
                            )?;
                            token.replace_term(term, memory, &input.context, &mut work)?;
                        } else {
                            token.refresh_context(&input.context, &mut work)?;
                        }
                    }
                    Self::KatakanaStem(minimum) if !token.keyword() => {
                        if katakana_stem(token, length, *minimum, &mut work)? {
                            length -= 1;
                            units(token, length, output_units, limits, &mut work)?;
                            let mut term = token.copy_term(memory.budget(), &mut work)?;
                            term.truncate(length);
                            let (term, allocation) = term.into_parts();
                            token.replace_term(
                                Budgeted::new(term, allocation),
                                memory,
                                &input.context,
                                &mut work,
                            )?;
                        } else {
                            token.refresh_context(&input.context, &mut work)?;
                        }
                    }
                    Self::SimpleLowercase => {
                        units(token, length, output_units, limits, &mut work)?;
                        let mut term = token.copy_term(memory.budget(), &mut work)?;
                        lowercase(&mut term, model, &mut work)?;
                        let (term, allocation) = term.into_parts();
                        token.replace_term(
                            Budgeted::new(term, allocation),
                            memory,
                            &input.context,
                            &mut work,
                        )?;
                    }
                    Self::BaseForm | Self::KatakanaStem(_) => {
                        token.refresh_context(&input.context, &mut work)?;
                    }
                    Self::PartOfSpeech(_) | Self::Stop(_, _) => unreachable!("non-removing filter"),
                }
                output_units = units(token, length, output_units, limits, &mut work)?;
                Ok(())
            })?
        };
        if let Some(terminal) = input.batch.terminal() {
            let mut work = Work::new(poll)?;
            units(
                terminal,
                terminal.term_len(&mut work)?,
                output_units,
                limits,
                &mut work,
            )?;
        }
        poll()?;
        Ok(input)
    }
    fn keep<T: JapaneseToken>(
        &self,
        token: &T,
        model: &KuromojiDictionary,
        budget: &uqa_core::memory::MemoryBudget,
        work: &mut Work<'_>,
    ) -> AnalysisResult<bool> {
        Ok(match self {
            Self::PartOfSpeech(words) => {
                if let Some(pos) = token.part_of_speech()? {
                    let term =
                        crate::morphology::input::encode(pos, budget, work.poll, |_| Ok(()))?;
                    !words.contains(&term, work.poll)?
                } else {
                    true
                }
            }
            Self::Stop(words, ignore_case) => {
                let mut term = token.copy_term(budget, work)?;
                if *ignore_case {
                    lowercase(&mut term, model, work)?;
                }
                !words.contains(&term, work.poll)?
            }
            _ => unreachable!("removal filter"),
        })
    }
}
fn units<T: JapaneseToken>(
    token: &T,
    term: usize,
    previous: usize,
    limits: KuromojiLimits,
    work: &mut Work<'_>,
) -> AnalysisResult<usize> {
    let overflow = || invalid("Kuromoji filter", "UTF-16 output size overflow");
    let mut total = previous.checked_add(term).ok_or_else(overflow)?;
    for attribute in token.attributes().into_iter().flatten() {
        total = total
            .checked_add(text_units(attribute, work)?)
            .ok_or_else(overflow)?;
    }
    check_limit(
        "Kuromoji output UTF-16 units",
        total,
        limits.max_output_utf16,
    )?;
    Ok(total)
}
fn katakana_stem<T: JapaneseToken>(
    token: &T,
    length: usize,
    minimum: usize,
    work: &mut Work<'_>,
) -> AnalysisResult<bool> {
    if length < minimum {
        return Ok(false);
    }
    let mut last = None;
    for unit in token.term() {
        work.tick()?;
        if !(0x30a0..=0x30ff).contains(&unit) {
            return Ok(false);
        }
        last = Some(unit);
    }
    Ok(last == Some(0x30fc))
}
