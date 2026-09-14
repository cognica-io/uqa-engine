//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Completion joins pending surfaces, clears dictionary attributes and recreates graph positions.

use super::{
    add, check_limit, CompletionMode, CompletionWork, KuromojiDictionary, KuromojiLimits, Plan,
};
use crate::kuromoji::filters::stream::{token_units, JapaneseToken};
use crate::morphology::filter::AllocatedStream;
use crate::token::allocation::{TokenBatchAllocation, TokenBuffer};
use crate::AnalysisResult;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

struct Surface {
    units: BudgetedVec<u16>,
    lowercase: bool,
    kana: bool,
}
struct Pending<S> {
    surface: Surface,
    reading: BudgetedVec<u16>,
    valid_reading: bool,
    first: S,
    last: Option<S>,
}
struct Generator<'a, T: JapaneseToken> {
    model: &'a KuromojiDictionary,
    mode: CompletionMode,
    limits: KuromojiLimits,
    budget: MemoryBudget,
    work: CompletionWork<'a>,
    context: T::Context,
    pending: Option<Pending<T::Span>>,
    output: TokenBuffer<T>,
    output_units: usize,
}

pub(in crate::kuromoji) fn filter<T: JapaneseToken>(
    input: AllocatedStream<T>,
    mode: CompletionMode,
    model: &KuromojiDictionary,
    limits: KuromojiLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<AllocatedStream<T>> {
    let work = CompletionWork::new(limits.max_completion_work, poll)?;
    check_limit(
        "Kuromoji input UTF-16 units",
        input.final_offset_utf16,
        limits.max_input_utf16,
    )?;
    check_limit(
        "Kuromoji output tokens",
        input.batch.tokens().len(),
        limits.max_tokens,
    )?;
    let budget = input.batch.budget().clone();
    let mut state: Generator<'_, T> = Generator {
        model,
        mode,
        limits,
        budget: budget.clone(),
        work,
        context: input.context,
        pending: None,
        output: TokenBuffer::new(&budget),
        output_units: 0,
    };
    let mut source = input.batch.into_input();
    loop {
        let (token, remaining) = source.next(state.work.work.poll)?;
        source = remaining;
        let Some(token) = token else { break };
        state.accept(&*token)?;
    }
    let (terminal, final_increment) = source.finish();
    if state.pending.is_some() {
        // Emitting the pending surface clears attributes changed by the exhausted input.
        drop(terminal);
        state.emit()?;
    } else if let Some(terminal) = terminal {
        let units = token_units(
            &**terminal,
            terminal.term_len(&mut state.work.work)?,
            state.output_units,
            &mut state.work.work,
        )?;
        check_limit(
            "Kuromoji output UTF-16 units",
            units,
            limits.max_output_utf16,
        )?;
        state.output.set_terminal_box(terminal);
    }
    state.work.finish()?;
    Ok(AllocatedStream {
        batch: TokenBatchAllocation::from_budgeted(state.output.into_batch(final_increment)),
        final_offset_utf16: input.final_offset_utf16,
        context: state.context,
    })
}

impl<T: JapaneseToken> Generator<'_, T> {
    fn accept(&mut self, token: &T) -> AnalysisResult<()> {
        self.work.tick()?;
        // Lucene accesses each new reading before emitting the previous pending token.
        let reading = token.reading()?;
        let mut surface = Surface {
            units: BudgetedVec::new(&self.budget),
            lowercase: true,
            kana: true,
        };
        let length = token.term_len(&mut self.work.work)?;
        check_limit(
            "Kuromoji completion scratch UTF-16 units",
            length,
            self.limits.max_output_utf16,
        )?;
        surface.units.reserve(length)?;
        for unit in token.term() {
            self.work.tick()?;
            surface.lowercase &= matches!(unit, 0x61..=0x7a | 0xff41..=0xff5a);
            surface.kana &= matches!(unit, 0x3040..=0x30ff);
            surface.units.push(unit)?;
        }
        let mut pending = self.pending.take();
        if let Some(value) = pending.as_mut() {
            if self.mode == CompletionMode::Query && !value.surface.lowercase && surface.lowercase {
                self.append(&mut value.surface.units, surface.units.iter().copied())?;
                value.valid_reading &=
                    self.append_reading(&mut value.reading, surface.units.iter().copied())?;
                value.last = Some(token.span());
                self.pending = pending;
                return self.emit();
            }
            if self.mode == CompletionMode::Query && value.surface.kana && surface.kana {
                self.append(&mut value.surface.units, surface.units.iter().copied())?;
                value.surface.lowercase &= surface.lowercase;
                value.valid_reading &= self.reading(&mut value.reading, reading, &surface)?;
                value.last = Some(token.span());
                self.pending = pending;
                return Ok(());
            }
            self.pending = pending;
            self.emit()?;
        }
        let mut units = BudgetedVec::new(&self.budget);
        let valid_reading = self.reading(&mut units, reading, &surface)?;
        self.pending = Some(Pending {
            surface,
            reading: units,
            valid_reading,
            first: token.span(),
            last: None,
        });
        Ok(())
    }

    fn reading(
        &mut self,
        target: &mut BudgetedVec<u16>,
        reading: Option<&str>,
        surface: &Surface,
    ) -> AnalysisResult<bool> {
        if let Some(reading) = reading {
            self.append_reading(target, reading.encode_utf16())
        } else if surface.kana {
            self.append_reading(
                target,
                surface.units.iter().map(|&unit| {
                    if matches!(unit, 0x3041..=0x3096 | 0x309d..=0x309e) {
                        unit + 0x60
                    } else {
                        unit
                    }
                }),
            )
        } else {
            // CharsRefBuilder.append(null) appends these four literal characters.
            self.append_reading(target, "null".encode_utf16())
        }
    }

    fn append_reading(
        &mut self,
        target: &mut BudgetedVec<u16>,
        units: impl Iterator<Item = u16>,
    ) -> AnalysisResult<bool> {
        let mut valid = true;
        self.append(
            target,
            units.inspect(|unit| valid &= matches!(*unit, 0x30a0..=0x30ff | 0x61..=0x7a)),
        )?;
        Ok(valid)
    }

    fn append(
        &mut self,
        target: &mut BudgetedVec<u16>,
        units: impl Iterator<Item = u16>,
    ) -> AnalysisResult<()> {
        for unit in units {
            self.work.tick()?;
            target.push(unit)?;
        }
        Ok(())
    }

    fn emit(&mut self) -> AnalysisResult<()> {
        self.work.tick()?;
        let Pending {
            surface,
            reading,
            valid_reading,
            first,
            last,
        } = self.pending.take().expect("pending completion surface");
        let tokens = add(self.output.len(), 1)?;
        let units = add(self.output_units, surface.units.len())?;
        check_limit("Kuromoji output tokens", tokens, self.limits.max_tokens)?;
        check_limit(
            "Kuromoji output UTF-16 units",
            units,
            self.limits.max_output_utf16,
        )?;
        let limits = KuromojiLimits {
            max_tokens: self.limits.max_tokens - tokens,
            max_output_utf16: self.limits.max_output_utf16 - units,
            ..self.limits
        };
        let input = if valid_reading { &*reading } else { &[] };
        let plan = Plan::new(input, self.model, limits, &self.budget, &mut self.work)?;
        self.output_units = add(units, plan.units)?;
        self.output.reserve_tokens(add(plan.count, 1)?)?;
        let last = last.as_ref().unwrap_or(&first);
        let (term, memory) = surface.units.into_parts();
        let token = T::generated(
            Budgeted::new(term, memory),
            &first,
            last,
            1,
            &self.context,
            &mut self.work.work,
        )?;
        self.output.push(token)?;
        for index in 0..plan.count {
            let term = plan.term(index, &self.budget, &mut self.work)?;
            let token = T::generated(term, &first, last, 0, &self.context, &mut self.work.work)?;
            self.output.push(token)?;
        }
        Ok(())
    }
}
