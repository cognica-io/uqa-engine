//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered rolling Viterbi with frontier commits and the reference's forced backtrace rule.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::lattice::{Node, WordId};
use super::word::Word;
use super::{NoriLimits, NoriOptions, NoriOutput, NoriToken};
use crate::morphology::viterbi::{Search, Traversal};
use crate::nori::error::{check_limit, invalid};
use crate::nori::{NoriDictionary, POSTag, UserDictionary};
use crate::AnalysisResult;

pub(super) struct State<'a> {
    pub input: &'a [u16],
    pub model: &'a NoriDictionary,
    pub user: Option<&'a UserDictionary>,
    pub options: NoriOptions,
    pub traversal: Traversal<'a, NoriLimits>,
    pub pending: BudgetedVec<NoriToken>,
    pub ngram: Option<u32>,
    pub budget: &'a MemoryBudget,
    limits: NoriLimits,
    total_tokens: usize,
    output_units: usize,
    output_memory: MemoryReservation,
}

pub(super) fn analyze(
    input: &[u16],
    model: &NoriDictionary,
    user: Option<&UserDictionary>,
    options: NoriOptions,
    limits: NoriLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<NoriOutput>> {
    let ngram =
        if options.output_unknown_unigrams {
            let mut ngram = None;
            for (work, id) in (model.known_word_count()..model.word_count()).enumerate() {
                if work % 1024 == 0 {
                    poll()?;
                }
                if model
                    .word(id as u32)
                    .is_some_and(|word| word.original_id() == 0)
                {
                    ngram = Some(id as u32);
                    break;
                }
            }
            Some(ngram.ok_or_else(|| {
                invalid("Nori tokenizer", "unknown dictionary has no word ID zero")
            })?)
        } else {
            None
        };
    let mut state = State {
        input,
        model,
        user,
        options,
        traversal: Traversal::new(input.len(), limits, budget, poll)?,
        pending: BudgetedVec::new(budget),
        ngram,
        budget,
        limits,
        total_tokens: 0,
        output_units: 0,
        output_memory: budget.empty_reservation(),
    };
    let mut tokens = BudgetedVec::new(budget);
    loop {
        let ended = state.forward()?;
        tokens.reserve(state.pending.len())?;
        while let Some(token) = state.pending.pop() {
            state.tick()?;
            tokens.push(token)?;
        }
        if ended {
            break;
        }
    }
    (state.traversal.poll)()?;
    let (tokens, mut memory) = tokens.into_parts();
    memory.absorb(state.output_memory);
    Ok(Budgeted::new(
        NoriOutput::from_tokens(tokens, state.traversal.position, 0),
        memory,
    ))
}

impl State<'_> {
    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.traversal.tick()
    }

    pub fn check_units(&self, additional: usize) -> AnalysisResult<()> {
        let total = self
            .output_units
            .checked_add(additional)
            .ok_or_else(|| invalid("Nori emission", "UTF-16 output size overflow"))?;
        check_limit(
            "Nori output UTF-16 units",
            total,
            self.limits.max_output_utf16,
        )?;
        Ok(())
    }

    pub fn push(&mut self, token: Budgeted<NoriToken>) -> AnalysisResult<()> {
        check_limit(
            "Nori output tokens",
            self.total_tokens
                .checked_add(1)
                .ok_or_else(|| invalid("Nori emission", "token count overflow"))?,
            self.limits.max_tokens,
        )?;
        let mut units = token.term_utf16.len();
        if let Some(reading) = &token.reading {
            units = units
                .checked_add(super::allocation::utf16_len(
                    reading,
                    usize::MAX,
                    self.traversal.poll,
                )?)
                .ok_or_else(|| invalid("Nori emission", "reading size overflow"))?;
        }
        for part in token.morphemes.iter().flatten() {
            self.tick()?;
            units = units
                .checked_add(part.surface_utf16.len())
                .ok_or_else(|| invalid("Nori emission", "morpheme size overflow"))?;
        }
        self.check_units(units)?;
        let (token, memory) = token.into_parts();
        self.pending.push(token)?;
        self.output_memory.absorb(memory);
        self.total_tokens += 1;
        self.output_units += units;
        Ok(())
    }

    fn forward(&mut self) -> AnalysisResult<bool> {
        crate::morphology::viterbi::forward(self)
    }

    fn extend_at(&mut self, user_maximum: &mut Option<usize>) -> AnalysisResult<()> {
        let from = self.traversal.position;
        if self
            .model
            .unicode(u32::from(self.input[self.traversal.position]))
            .expect("complete Unicode table")
            .category
            == 12
            && self.traversal.position + 1 < self.input.len()
        {
            self.traversal.position += 1;
        }
        let mut matched = false;
        if let Some(user) = self.user {
            let mut longest = None;
            let mut cursor = user.cursor();
            for (offset, &unit) in self.input[self.traversal.position..].iter().enumerate() {
                self.tick()?;
                if cursor.advance(unit).is_none() {
                    break;
                }
                if let Some(id) = cursor.rank() {
                    longest = Some((offset + 1, id));
                }
            }
            if let Some((length, id)) = longest {
                matched = true;
                let end = self.traversal.position + length;
                if user_maximum.is_none_or(|previous| end > previous) {
                    self.add(from, self.traversal.position, end, WordId::User(id))?;
                    *user_maximum = Some(end);
                }
            }
        }
        if !matched {
            let model = self.model;
            let mut cursor = model.lexicon.cursor();
            for (offset, &unit) in self.input[self.traversal.position..].iter().enumerate() {
                self.tick()?;
                if cursor.advance(unit).is_none() {
                    break;
                }
                if let Some(rank) = cursor.rank() {
                    for id in model.surfaces[rank as usize].word_ids.clone() {
                        self.add(
                            from,
                            self.traversal.position,
                            self.traversal.position + offset + 1,
                            WordId::Known(id),
                        )?;
                        matched = true;
                    }
                }
            }
        }
        self.unknown(from, matched)?;
        Ok(())
    }

    fn add(&mut self, from: usize, word_pos: usize, end: usize, id: WordId) -> AnalysisResult<()> {
        self.tick()?;
        let word = Word::resolve(id, self.model, self.user);
        let penalty = if word_pos > from && penalized(word.left_pos()) {
            3000
        } else {
            0
        };
        let mut least = i32::MAX;
        let mut best = None;
        for index in 0..self.traversal.lattice.get(from).len() {
            self.tick()?;
            let node = self.traversal.lattice.get(from)[index];
            let cost = node
                .cost
                .wrapping_add(i32::from(
                    self.model
                        .connection_cost(node.right, word.left())
                        .expect("validated contexts"),
                ))
                .wrapping_add(penalty);
            if cost < least {
                least = cost;
                best = Some(index);
            }
        }
        let back_index =
            best.ok_or_else(|| invalid("Nori lattice", "no incoming least-cost path"))?;
        self.traversal.lattice.push(
            end,
            Node {
                cost: least.wrapping_add(word.cost()),
                right: word.right(),
                back_pos: from,
                word_pos,
                back_index,
                word: id,
            },
            self.traversal.poll,
        )?;
        Ok(())
    }

    fn unknown(&mut self, from: usize, matched: bool) -> AnalysisResult<()> {
        let first = self.input[self.traversal.position];
        if matched && !self.model.invokes_unknown(first) {
            return Ok(());
        }
        let mut class = self.model.character_class(first);
        let mut length = 1;
        if self.model.groups_unknown(first) {
            let first_properties = self
                .model
                .unicode(u32::from(first))
                .expect("complete Unicode table");
            let mut script = first_properties.script;
            let punct = punctuation(first, first_properties.category);
            while length < 1024 && self.traversal.position + length < self.input.len() {
                self.tick()?;
                let unit = self.input[self.traversal.position + length];
                let properties = self
                    .model
                    .unicode(u32::from(unit))
                    .expect("complete Unicode table");
                let same_script = script == properties.script
                    || self.common(script)
                    || self.common(properties.script)
                    || properties.category == 6;
                if !same_script
                    || punctuation(unit, properties.category) != punct
                    || properties.is_digit != first_properties.is_digit
                    || !self.model.groups_unknown(unit)
                {
                    break;
                }
                length += 1;
                if self.common(script) && !self.common(properties.script) {
                    script = properties.script;
                    class = self.model.character_class(unit);
                }
            }
        }
        for id in self
            .model
            .unknown_words(class)
            .expect("validated unknown class")
        {
            self.add(
                from,
                self.traversal.position,
                self.traversal.position + length,
                WordId::Unknown(id),
            )?;
        }
        Ok(())
    }

    fn common(&self, script: u16) -> bool {
        matches!(
            self.model.unicode_script_name(script),
            Some("COMMON" | "INHERITED")
        )
    }
}

impl<'a> Search<'a> for State<'a> {
    type Config = NoriLimits;
    type Batch = Option<usize>;

    fn traversal(&self) -> &Traversal<'a, NoriLimits> {
        &self.traversal
    }

    fn traversal_mut(&mut self) -> &mut Traversal<'a, NoriLimits> {
        &mut self.traversal
    }

    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    fn extend(&mut self, batch: &mut Self::Batch) -> AnalysisResult<()> {
        self.extend_at(batch)
    }

    fn backtrace(&mut self, position: usize, index: usize) -> AnalysisResult<()> {
        super::emission::backtrace(self, position, index)
    }

    fn eos_cost(&self, right: u16) -> i32 {
        i32::from(
            self.model
                .connection_cost(right, 0)
                .expect("validated context"),
        )
    }
}

pub(super) fn punctuation(unit: u16, category: u8) -> bool {
    unit == 0x318d || matches!(category, 12..=16 | 20..=30)
}

fn penalized(tag: POSTag) -> bool {
    matches!(
        tag,
        POSTag::EP
            | POSTag::EF
            | POSTag::EC
            | POSTag::ETN
            | POSTag::ETM
            | POSTag::JKS
            | POSTag::JKC
            | POSTag::JKG
            | POSTag::JKO
            | POSTag::JKB
            | POSTag::JKV
            | POSTag::JKQ
            | POSTag::JX
            | POSTag::JC
            | POSTag::VCP
            | POSTag::XSA
            | POSTag::XSN
            | POSTag::XSV
    )
}

#[cfg(test)]
mod tests;
