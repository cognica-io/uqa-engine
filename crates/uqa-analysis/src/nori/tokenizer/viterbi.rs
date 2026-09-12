//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered rolling Viterbi with frontier commits and the reference's forced backtrace rule.

use super::lattice::{Lattice, Node, WordId};
use super::word::Word;
use super::{NoriLimits, NoriOptions, NoriOutput, NoriToken};
use crate::nori::error::{check_limit, invalid};
use crate::nori::{DictionaryError, NoriDictionary, POSTag, UserDictionary};
use crate::AnalysisResult;

pub(super) struct State<'a> {
    pub input: &'a [u16],
    pub model: &'a NoriDictionary,
    pub user: Option<&'a UserDictionary>,
    pub options: NoriOptions,
    pub lattice: Lattice,
    pub position: usize,
    pub last_backtrace: usize,
    pub pending: Vec<NoriToken>,
    pub ngram: Option<u32>,
    limits: NoriLimits,
    total_tokens: usize,
    output_units: usize,
    work: usize,
    poll: &'a mut dyn FnMut() -> AnalysisResult<()>,
}

pub(super) fn analyze(
    input: &[u16],
    model: &NoriDictionary,
    user: Option<&UserDictionary>,
    options: NoriOptions,
    limits: NoriLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<NoriOutput> {
    let ngram = if options.output_unknown_unigrams {
        Some(
            (model.known_word_count()..model.word_count())
                .find(|id| {
                    model
                        .word(*id as u32)
                        .is_some_and(|word| word.original_id() == 0)
                })
                .ok_or_else(|| {
                    invalid("Nori tokenizer", "unknown dictionary has no word ID zero")
                })? as u32,
        )
    } else {
        None
    };
    let mut state = State {
        input,
        model,
        user,
        options,
        lattice: Lattice::new(limits)?,
        position: 0,
        last_backtrace: 0,
        pending: Vec::new(),
        ngram,
        limits,
        total_tokens: 0,
        output_units: 0,
        work: 0,
        poll,
    };
    let mut tokens = Vec::new();
    loop {
        let ended = state.forward()?;
        tokens
            .try_reserve(state.pending.len())
            .map_err(DictionaryError::from)?;
        tokens.extend(state.pending.drain(..).rev());
        if ended {
            break;
        }
    }
    (state.poll)()?;
    Ok(NoriOutput {
        tokens,
        final_offset_utf16: state.position,
        final_position_increment: 0,
    })
}

impl State<'_> {
    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.work = (self.work + 1) % 1024;
        if self.work == 0 {
            (self.poll)()?;
        }
        Ok(())
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

    pub fn push(&mut self, token: NoriToken) -> AnalysisResult<()> {
        check_limit(
            "Nori output tokens",
            self.total_tokens + 1,
            self.limits.max_tokens,
        )?;
        let mut units = token.term_utf16.len()
            + token
                .reading
                .as_ref()
                .map_or(0, |text| text.encode_utf16().count());
        for part in token.morphemes.iter().flatten() {
            units = units
                .checked_add(part.surface_utf16.len())
                .ok_or_else(|| invalid("Nori emission", "morpheme size overflow"))?;
        }
        self.check_units(units)?;
        self.pending.try_reserve(1).map_err(DictionaryError::from)?;
        self.pending.push(token);
        self.total_tokens += 1;
        self.output_units += units;
        Ok(())
    }

    fn forward(&mut self) -> AnalysisResult<bool> {
        // The reference resets this bound whenever a nonempty pending batch is consumed.
        let mut user_maximum = None;
        while self.position < self.input.len() {
            self.tick()?;
            self.lattice.ensure(self.position)?;
            if self.lattice.get(self.position).is_empty() {
                self.position += 1;
                continue;
            }
            let frontier = self.lattice.next_pos() == self.position + 1;
            if self.position > self.last_backtrace
                && frontier
                && self.lattice.get(self.position).len() == 1
            {
                super::emission::backtrace(self, self.position, 0)?;
                self.lattice.rebase(self.position);
                if !self.pending.is_empty() {
                    return Ok(false);
                }
            }
            if self.position - self.last_backtrace >= 1024 {
                self.force_backtrace()?;
                if !self.pending.is_empty() {
                    return Ok(false);
                }
                continue;
            }
            let from = self.position;
            if self
                .model
                .unicode(u32::from(self.input[self.position]))
                .expect("complete Unicode table")
                .category
                == 12
                && self.position + 1 < self.input.len()
            {
                self.position += 1;
            }
            let mut matched = false;
            if let Some(user) = self.user {
                let mut longest = None;
                let mut cursor = user.cursor();
                for (offset, &unit) in self.input[self.position..].iter().enumerate() {
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
                    let end = self.position + length;
                    if user_maximum.is_none_or(|previous| end > previous) {
                        self.add(from, self.position, end, WordId::User(id))?;
                        user_maximum = Some(end);
                    }
                }
            }
            if !matched {
                let model = self.model;
                let mut cursor = model.lexicon.cursor();
                for (offset, &unit) in self.input[self.position..].iter().enumerate() {
                    self.tick()?;
                    if cursor.advance(unit).is_none() {
                        break;
                    }
                    if let Some(rank) = cursor.rank() {
                        for id in model.surfaces[rank as usize].word_ids.clone() {
                            self.add(
                                from,
                                self.position,
                                self.position + offset + 1,
                                WordId::Known(id),
                            )?;
                            matched = true;
                        }
                    }
                }
            }
            self.unknown(from, matched)?;
            self.position += 1;
        }
        self.finish()?;
        Ok(true)
    }

    fn finish(&mut self) -> AnalysisResult<()> {
        if self.position > 0 {
            self.lattice.ensure(self.position)?;
            let mut best = None;
            let mut least_cost = i32::MAX;
            for index in 0..self.lattice.get(self.position).len() {
                self.tick()?;
                let node = self.lattice.get(self.position)[index];
                let cost = node.cost.wrapping_add(i32::from(
                    self.model
                        .connection_cost(node.right, 0)
                        .expect("validated context"),
                ));
                if cost < least_cost {
                    least_cost = cost;
                    best = Some(index);
                }
            }
            let best = best.ok_or_else(|| invalid("Nori lattice", "no complete path"))?;
            super::emission::backtrace(self, self.position, best)?;
        }
        Ok(())
    }

    fn force_backtrace(&mut self) -> AnalysisResult<()> {
        let mut best = None;
        let mut least = i32::MAX;
        for position in self.position..self.lattice.next_pos() {
            self.tick()?;
            for index in 0..self.lattice.get(position).len() {
                self.tick()?;
                let node = self.lattice.get(position)[index];
                if node.cost < least {
                    least = node.cost;
                    best = Some((position, index));
                }
            }
        }
        let (position, index) =
            best.ok_or_else(|| invalid("Nori lattice", "no live path at forced backtrace"))?;
        self.lattice.prune(self.position, position, index);
        super::emission::backtrace(self, position, 0)?;
        self.lattice.rebase(position);
        self.position = position;
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
        for index in 0..self.lattice.get(from).len() {
            self.tick()?;
            let node = self.lattice.get(from)[index];
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
        self.lattice.push(
            end,
            Node {
                cost: least.wrapping_add(word.cost()),
                right: word.right(),
                back_pos: from,
                word_pos,
                back_index,
                word: id,
            },
        )?;
        Ok(())
    }

    fn unknown(&mut self, from: usize, matched: bool) -> AnalysisResult<()> {
        let first = self.input[self.position];
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
            while length < 1024 && self.position + length < self.input.len() {
                self.tick()?;
                let unit = self.input[self.position + length];
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
                self.position,
                self.position + length,
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
