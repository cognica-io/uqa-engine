//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Japanese prefix matching, unknown grouping and penalties over the shared ordered traversal.

use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation};

use super::word;
use super::{KuromojiLimits, KuromojiMode, KuromojiOptions, KuromojiOutput, KuromojiToken};
use crate::kuromoji::error::{check_limit, invalid};
use crate::kuromoji::{KuromojiDictionary, UserDictionary};
use crate::morphology::lattice::{Node, WordId};
use crate::morphology::viterbi::{Search, Traversal};
use crate::AnalysisResult;

pub(super) struct State<'a> {
    pub input: &'a [u16],
    pub model: &'a KuromojiDictionary,
    pub user: Option<&'a UserDictionary>,
    pub options: KuromojiOptions,
    pub limits: KuromojiLimits,
    pub traversal: Traversal<'a, KuromojiLimits>,
    pub pending: BudgetedVec<KuromojiToken>,
    pub budget: &'a MemoryBudget,
    pub ngram: Option<u32>,
    total_tokens: usize,
    output_units: usize,
    resegmentation_work: usize,
    output_memory: MemoryReservation,
}

pub(super) fn analyze(
    input: &[u16],
    model: &KuromojiDictionary,
    user: Option<&UserDictionary>,
    options: KuromojiOptions,
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<KuromojiOutput>> {
    let mut ngram = None;
    if options.mode == KuromojiMode::Extended {
        for id in model.unknown_words(0).expect("validated NGRAM class") {
            poll()?;
            if model
                .word(id)
                .expect("validated unknown word")
                .original_id()
                == 0
            {
                ngram = Some(id);
                break;
            }
        }
        if ngram.is_none() {
            return Err(invalid(
                "Kuromoji tokenizer",
                "unknown dictionary has no NGRAM word ID zero",
            )
            .into());
        }
    }
    let mut state = State {
        input,
        model,
        user,
        options,
        limits,
        traversal: Traversal::new(input.len(), limits, budget, poll)?,
        pending: BudgetedVec::new(budget),
        budget,
        ngram,
        total_tokens: 0,
        output_units: 0,
        resegmentation_work: 0,
        output_memory: budget.empty_reservation(),
    };
    let mut tokens = BudgetedVec::new(budget);
    let mut last_position = None;
    loop {
        let ended = crate::morphology::viterbi::forward(&mut state)?;
        tokens.reserve(state.pending.len())?;
        while let Some(mut token) = state.pending.pop() {
            state.tick()?;
            token.position_increment = u32::from(last_position != Some(token.start_utf16));
            if token.position_increment != 0 {
                token.position_length = 1;
            }
            last_position = Some(token.start_utf16);
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
        KuromojiOutput {
            tokens,
            final_offset_utf16: state.traversal.position,
            final_position_increment: 0,
        },
        memory,
    ))
}

impl State<'_> {
    pub fn tick(&mut self) -> AnalysisResult<()> {
        self.traversal.tick()
    }

    pub fn resegment_tick(&mut self) -> AnalysisResult<()> {
        self.tick()?;
        self.resegmentation_work = self
            .resegmentation_work
            .checked_add(1)
            .ok_or_else(|| invalid("Kuromoji resegmentation", "work count overflow"))?;
        check_limit(
            "Kuromoji resegmentation work",
            self.resegmentation_work,
            self.limits.max_resegmentation_work,
        )?;
        Ok(())
    }

    pub fn check_units(&self, additional: usize) -> AnalysisResult<()> {
        let total = self
            .output_units
            .checked_add(additional)
            .ok_or_else(|| invalid("Kuromoji emission", "UTF-16 output size overflow"))?;
        check_limit(
            "Kuromoji output UTF-16 units",
            total,
            self.limits.max_output_utf16,
        )?;
        Ok(())
    }

    pub fn push(&mut self, token: Budgeted<KuromojiToken>) -> AnalysisResult<()> {
        check_limit(
            "Kuromoji output tokens",
            self.total_tokens
                .checked_add(1)
                .ok_or_else(|| invalid("Kuromoji emission", "token count overflow"))?,
            self.limits.max_tokens,
        )?;
        let mut units = token.term_utf16.len();
        for attribute in [
            &token.part_of_speech,
            &token.base_form,
            &token.reading,
            &token.pronunciation,
            &token.inflection_type,
            &token.inflection_form,
        ]
        .into_iter()
        .flatten()
        {
            let count =
                crate::morphology::input::utf16_len(attribute, self.traversal.poll, |_| Ok(()))?;
            units = units
                .checked_add(count)
                .ok_or_else(|| invalid("Kuromoji emission", "attribute size overflow"))?;
        }
        self.check_units(units)?;
        let (token, memory) = token.into_parts();
        self.pending.push(token)?;
        self.output_memory.absorb(memory);
        self.total_tokens += 1;
        self.output_units += units;
        Ok(())
    }

    pub fn add(
        &mut self,
        from: usize,
        end: usize,
        id: WordId,
        penalize: bool,
    ) -> AnalysisResult<()> {
        self.tick()?;
        let word = word::costs(id, self.model, self.user);
        let mut least = i32::MAX;
        let mut best = None;
        for index in 0..self.traversal.lattice.get(from).len() {
            self.tick()?;
            let node = self.traversal.lattice.get(from)[index];
            let cost = node.cost.wrapping_add(i32::from(
                self.model
                    .connection_cost(node.right, word.left)
                    .expect("validated contexts"),
            ));
            if cost < least {
                least = cost;
                best = Some(index);
            }
        }
        let back_index =
            best.ok_or_else(|| invalid("Kuromoji lattice", "no incoming least-cost path"))?;
        let mut cost = least.wrapping_add(word.word);
        if penalize && !matches!(id, WordId::User(_)) {
            cost = cost.wrapping_add(self.penalty(from, end - from)?);
        }
        self.traversal.lattice.push(
            end,
            Node {
                cost,
                right: word.right,
                back_pos: from,
                word_pos: from,
                back_index,
                word: id,
            },
            self.traversal.poll,
        )
    }

    pub fn penalty(&mut self, start: usize, length: usize) -> AnalysisResult<i32> {
        if length > 2 {
            let mut kanji = true;
            for &unit in &self.input[start..start + length] {
                self.tick()?;
                if !self.model.is_kanji(unit) {
                    kanji = false;
                    break;
                }
            }
            if kanji {
                return Ok(((length - 2) as i32).wrapping_mul(3000));
            }
            if length > 7 {
                return Ok(((length - 7) as i32).wrapping_mul(1700));
            }
        }
        Ok(0)
    }

    pub fn punctuation(&self, unit: u16) -> bool {
        matches!(self.model.unicode(u32::from(unit)).expect("complete Unicode table").category, 12..=16 | 20..=30)
    }

    fn unknown(&mut self, matched: bool) -> AnalysisResult<usize> {
        let start = self.traversal.position;
        let first = self.input[start];
        if matched && !self.model.invokes_unknown(first) {
            return Ok(0);
        }
        let class = self.model.character_class(first);
        let mut length = 1;
        if self.model.groups_unknown(first) {
            let punctuation = self.punctuation(first);
            while length < 1024 && start + length < self.input.len() {
                self.tick()?;
                let unit = self.input[start + length];
                if self.model.character_class(unit) != class
                    || self.punctuation(unit) != punctuation
                {
                    break;
                }
                length += 1;
            }
        }
        for id in self
            .model
            .unknown_words(class)
            .expect("validated unknown class")
        {
            self.add(start, start + length, WordId::Unknown(id), false)?;
        }
        Ok(length)
    }
}

impl<'a> Search<'a> for State<'a> {
    type Config = KuromojiLimits;
    type Batch = usize;

    fn traversal(&self) -> &Traversal<'a, KuromojiLimits> {
        &self.traversal
    }
    fn traversal_mut(&mut self) -> &mut Traversal<'a, KuromojiLimits> {
        &mut self.traversal
    }
    fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }
    fn eos_cost(&self, right: u16) -> i32 {
        i32::from(
            self.model
                .connection_cost(right, 0)
                .expect("validated context"),
        )
    }
    fn backtrace(&mut self, position: usize, index: usize) -> AnalysisResult<()> {
        super::emission::backtrace(self, position, index)
    }

    fn extend(&mut self, unknown_end: &mut usize) -> AnalysisResult<()> {
        let start = self.traversal.position;
        let mut matched = false;
        if let Some(user) = self.user {
            let mut cursor = user.cursor();
            for (offset, &unit) in self.input[start..].iter().enumerate() {
                self.tick()?;
                if cursor.advance(unit).is_none() {
                    break;
                }
                if let Some(id) = cursor.rank() {
                    self.add(start, start + offset + 1, WordId::User(id), false)?;
                    matched = true;
                }
            }
        }
        if !matched {
            let mut cursor = self.model.lexicon.cursor();
            for (offset, &unit) in self.input[start..].iter().enumerate() {
                self.tick()?;
                if cursor.advance(unit).is_none() {
                    break;
                }
                if let Some(rank) = cursor.rank() {
                    for id in self.model.surfaces[rank as usize].word_ids.clone() {
                        self.add(start, start + offset + 1, WordId::Known(id), false)?;
                        matched = true;
                    }
                }
            }
        }
        if self.options.mode != KuromojiMode::Normal || *unknown_end <= start {
            *unknown_end = start + self.unknown(matched)?;
        }
        Ok(())
    }
}
