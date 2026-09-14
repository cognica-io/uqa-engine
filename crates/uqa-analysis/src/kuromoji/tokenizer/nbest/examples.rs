//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Example-derived cost probes preserve the reference grammar and first substring occurrence.

use uqa_core::memory::{BudgetedVec, MemoryBudget};

use super::super::{viterbi, JapaneseTokenizer, KuromojiLimits};
use crate::kuromoji::error::{check_limit, invalid};
use crate::{AnalysisError, AnalysisResult};

impl JapaneseTokenizer {
    /// Effective signed N-best allowance. Non-positive values retain single-path behavior.
    pub fn n_best_cost(&self) -> i32 {
        self.options.n_best_cost
    }

    /// Estimate the maximum extra cost for slash-separated `input-requiredToken` examples.
    pub fn calc_n_best_cost(&self, examples: &str) -> AnalysisResult<i32> {
        self.calc_n_best_cost_budgeted(
            examples,
            KuromojiLimits::default(),
            &MemoryBudget::new(usize::MAX),
            &mut || Ok(()),
        )
    }

    /// Return an immutable tokenizer with the greater of its explicit and example-derived costs.
    ///
    /// ```
    /// use uqa_analysis::kuromoji::{JapaneseTokenizer, KuromojiOptions, KuromojiResources};
    /// let dictionary = KuromojiResources::default().load_default()?;
    /// let tokenizer = JapaneseTokenizer::new(dictionary.model().clone(), None,
    ///     KuromojiOptions { n_best_cost: 2000, ..KuromojiOptions::default() })?;
    /// let configured = tokenizer.with_n_best_examples("関西国際空港-関西")?;
    /// assert_eq!(configured.n_best_cost(), 9325);
    /// assert_eq!(tokenizer.n_best_cost(), 2000);
    /// # Ok::<(), Box<dyn std::error::Error>>(())
    /// ```
    pub fn with_n_best_examples(&self, examples: &str) -> AnalysisResult<Self> {
        self.with_n_best_examples_budgeted(
            examples,
            KuromojiLimits::default(),
            &MemoryBudget::new(usize::MAX),
            &mut || Ok(()),
        )
    }

    /// Bound preparation while preserving this tokenizer and its selected immutable models on failure.
    pub fn with_n_best_examples_budgeted(
        &self,
        examples: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Self> {
        let cost = self.calc_n_best_cost_budgeted(examples, limits, budget, poll)?;
        let mut tokenizer = self.clone();
        tokenizer.options.n_best_cost = tokenizer.options.n_best_cost.max(cost);
        Ok(tokenizer)
    }

    /// Reserve example encodings, substring search and probe lattices through one preparation allowance.
    ///
    /// Empty slash components are ignored. Java's removal of trailing empty hyphen fields is retained; every nonempty example must leave exactly two fields. Only the first occurrence of a required token is probed. Missing substrings contribute zero; present spans follow the reference's signed lattice-cost arithmetic. Successful preparation retains no scratch allocation.
    pub fn calc_n_best_cost_budgeted(
        &self,
        examples: &str,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut impl FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<i32> {
        crate::allocation::input::utf16_len(examples, poll, |length| input_limit(length, limits))?;
        let mut work = 0;
        let mut count = 0;
        let mut start = 0;
        let mut maximum = 0;
        for end in 0..=examples.len() {
            tick(&mut work, limits, poll)?;
            if end == examples.len() || examples.as_bytes()[end] == b'/' {
                if start != end {
                    count += 1;
                    check_limit(
                        "Kuromoji N-best examples",
                        count,
                        limits.max_n_best_examples,
                    )?;
                    let (input, required) = pair(&examples[start..end], &mut work, limits, poll)?;
                    let input = crate::allocation::input::encode(input, budget, poll, |length| {
                        input_limit(length, limits)
                    })?;
                    let required =
                        crate::allocation::input::encode(required, budget, poll, |length| {
                            input_limit(length, limits)
                        })?;
                    if let Some(start) = find(&input, &required, &mut work, limits, budget, poll)? {
                        let (delta, used) = viterbi::probe(
                            &input,
                            self,
                            start..start + required.len(),
                            KuromojiLimits {
                                max_n_best_work: limits.max_n_best_work - work,
                                ..limits
                            },
                            budget,
                            poll,
                        )?;
                        work = work.checked_add(used).ok_or_else(|| {
                            invalid("Kuromoji N-best examples", "work count overflow")
                        })?;
                        maximum = maximum.max(delta);
                    }
                }
                start = end + 1;
            }
        }
        poll()?;
        Ok(maximum)
    }
}

fn input_limit(length: usize, limits: KuromojiLimits) -> AnalysisResult<()> {
    check_limit(
        "Kuromoji N-best example UTF-16 units",
        length,
        limits.max_input_utf16,
    )?;
    Ok(())
}

fn tick(
    work: &mut usize,
    limits: KuromojiLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    super::count_work(work, limits.max_n_best_work)?;
    if (*work).is_multiple_of(1024) {
        poll()?;
    }
    Ok(())
}

fn pair<'a>(
    example: &'a str,
    work: &mut usize,
    limits: KuromojiLimits,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<(&'a str, &'a str)> {
    let malformed = || {
        AnalysisError::from(invalid(
            "Kuromoji N-best examples",
            "expected exactly two hyphen-separated fields",
        ))
    };
    let mut end = example.len();
    while end > 0 && example.as_bytes()[end - 1] == b'-' {
        tick(work, limits, poll)?;
        end -= 1;
    }
    let mut separator = None;
    for (offset, &byte) in example.as_bytes()[..end].iter().enumerate() {
        tick(work, limits, poll)?;
        if byte == b'-' {
            if separator.is_some() {
                return Err(malformed());
            }
            separator = Some(offset);
        }
    }
    let separator = separator.ok_or_else(malformed)?;
    Ok((&example[..separator], &example[separator + 1..end]))
}

fn find(
    input: &[u16],
    pattern: &[u16],
    work: &mut usize,
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Option<usize>> {
    if pattern.len() > input.len() {
        return Ok(None);
    }
    let mut prefix = BudgetedVec::new(budget);
    prefix.reserve(pattern.len())?;
    prefix.push(0_usize)?;
    let mut matched = 0;
    for index in 1..pattern.len() {
        tick(work, limits, poll)?;
        while matched > 0 && pattern[index] != pattern[matched] {
            tick(work, limits, poll)?;
            matched = prefix[matched - 1];
        }
        if pattern[index] == pattern[matched] {
            matched += 1;
        }
        prefix.push(matched)?;
    }
    matched = 0;
    for (index, &unit) in input.iter().enumerate() {
        tick(work, limits, poll)?;
        while matched > 0 && unit != pattern[matched] {
            tick(work, limits, poll)?;
            matched = prefix[matched - 1];
        }
        if unit == pattern[matched] {
            matched += 1;
        }
        if matched == pattern.len() {
            return Ok(Some(index + 1 - matched));
        }
    }
    Ok(None)
}
