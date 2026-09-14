//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered IME alternatives use immutable lexical ranks and one reserved buffer per result.

use super::{error::check_limit, CompletionMapping, KuromojiDictionary, KuromojiLimits};
use crate::morphology::filter::Work;
use crate::AnalysisResult;
use uqa_core::memory::{Budgeted, BudgetedVec, MemoryBudget};

/// Convert raw UTF-16 using the dictionary's completion mappings, keeping ordered alternatives.
///
/// The longest key wins at each position. An unmatched suffix is appended to every candidate; an unmatched initial unit produces no candidates. This is separate from reading-form Hepburn.
///
/// ```
/// use uqa_analysis::kuromoji::{romanize_completion_utf16, KuromojiLimits, KuromojiResources};
/// let dictionary = KuromojiResources::default().load_default()?;
/// let output = romanize_completion_utf16(
///     &"シン".encode_utf16().collect::<Vec<_>>(), dictionary.model(),
///     KuromojiLimits::default(), &mut || Ok(()),
/// )?;
/// let terms: Vec<_> = output.iter().map(|units| String::from_utf16(units).unwrap()).collect();
/// assert_eq!(terms, ["sin", "shin", "sinn", "shinn"]);
/// # Ok::<(), Box<dyn std::error::Error>>(())
/// ```
pub fn romanize_completion_utf16(
    input: &[u16],
    model: &KuromojiDictionary,
    limits: KuromojiLimits,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Vec<Vec<u16>>> {
    Ok(romanize_completion_utf16_budgeted(
        input,
        model,
        limits,
        &MemoryBudget::new(usize::MAX),
        poll,
    )?
    .into_parts()
    .0)
}

/// Count the complete product before emitting, reserving all output and polling bounded work.
pub fn romanize_completion_utf16_budgeted(
    input: &[u16],
    model: &KuromojiDictionary,
    limits: KuromojiLimits,
    budget: &MemoryBudget,
    poll: &mut impl FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Budgeted<Vec<Vec<u16>>>> {
    let mut work = CompletionWork::new(limits.max_completion_work, poll)?;
    check_limit(
        "Kuromoji input UTF-16 units",
        input.len(),
        limits.max_input_utf16,
    )?;
    let plan = Plan::new(input, model, limits, budget, &mut work)?;
    let mut output = BudgetedVec::new(budget);
    output.reserve(plan.count)?;
    let mut memory = budget.empty_reservation();
    for index in 0..plan.count {
        let (term, allocation) = plan.term(index, budget, &mut work)?.into_parts();
        output.push(term)?;
        memory.absorb(allocation);
    }
    let (output, allocation) = output.into_parts();
    memory.absorb(allocation);
    work.finish()?;
    Ok(Budgeted::new(output, memory))
}

pub(super) mod stream;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CompletionMode {
    #[default]
    Index,
    Query,
}

struct Plan<'a> {
    mappings: BudgetedVec<&'a CompletionMapping>,
    suffix: &'a [u16],
    count: usize,
    units: usize,
}

impl<'a> Plan<'a> {
    fn new(
        input: &'a [u16],
        model: &'a KuromojiDictionary,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        work: &mut CompletionWork<'_>,
    ) -> AnalysisResult<Self> {
        let mut mappings = BudgetedVec::new(budget);
        let mut position = 0;
        let mut count = 1usize;
        let mut units = 0usize;
        while position < input.len() {
            let mut cursor = model.analysis.completion_lexicon.cursor();
            let mut longest = None;
            for (offset, &unit) in input[position..].iter().enumerate() {
                work.tick()?;
                if cursor.advance(unit).is_none() {
                    break;
                }
                if let Some(rank) = cursor.rank() {
                    longest = Some((offset + 1, rank));
                }
            }
            let Some((length, rank)) = longest else { break };
            let mapping = &model.completion_mappings()[rank as usize];
            let alternatives = mapping.alternatives();
            let next_count = multiply(count, alternatives.len())?;
            check_limit("Kuromoji output tokens", next_count, limits.max_tokens)?;
            let mut added = 0;
            for alternative in alternatives {
                added = add(added, work.text_length(alternative)?)?;
            }
            units = add(
                multiply(units, alternatives.len())?,
                multiply(count, added)?,
            )?;
            check_limit(
                "Kuromoji output UTF-16 units",
                units,
                limits.max_output_utf16,
            )?;
            mappings.push(mapping)?;
            count = next_count;
            position += length;
        }
        if mappings.is_empty() {
            count = 0;
        }
        let suffix = &input[position..];
        units = add(units, multiply(count, suffix.len())?)?;
        check_limit(
            "Kuromoji output UTF-16 units",
            units,
            limits.max_output_utf16,
        )?;
        Ok(Self {
            mappings,
            suffix,
            count,
            units,
        })
    }

    fn term(
        &self,
        index: usize,
        budget: &MemoryBudget,
        work: &mut CompletionWork<'_>,
    ) -> AnalysisResult<Budgeted<Vec<u16>>> {
        let mut length = self.suffix.len();
        self.visit(index, |text| {
            length = add(length, work.text_length(text)?)?;
            Ok(())
        })?;
        let mut output = BudgetedVec::new(budget);
        output.reserve(length)?;
        self.visit(index, |text| {
            for unit in text.encode_utf16() {
                work.tick()?;
                output.push(unit)?;
            }
            Ok(())
        })?;
        for &unit in self.suffix {
            work.tick()?;
            output.push(unit)?;
        }
        let (output, memory) = output.into_parts();
        Ok(Budgeted::new(output, memory))
    }

    fn visit(
        &self,
        mut index: usize,
        mut visit: impl FnMut(&str) -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        // Later mapping candidates are the outer loop; earlier candidates vary first.
        for mapping in self.mappings.iter() {
            let alternatives = mapping.alternatives();
            visit(&alternatives[index % alternatives.len()])?;
            index /= alternatives.len();
        }
        Ok(())
    }
}

struct CompletionWork<'a> {
    work: Work<'a>,
    used: usize,
    limit: usize,
}
impl<'a> CompletionWork<'a> {
    fn new(limit: usize, poll: &'a mut dyn FnMut() -> AnalysisResult<()>) -> AnalysisResult<Self> {
        Ok(Self {
            work: Work::new(poll)?,
            used: 0,
            limit,
        })
    }
    fn tick(&mut self) -> AnalysisResult<()> {
        self.used = add(self.used, 1)?;
        check_limit("Kuromoji completion work", self.used, self.limit)?;
        self.work.tick()
    }
    fn text_length(&mut self, text: &str) -> AnalysisResult<usize> {
        let mut length = 0;
        for _ in text.encode_utf16() {
            self.tick()?;
            length += 1;
        }
        Ok(length)
    }
    fn finish(&mut self) -> AnalysisResult<()> {
        self.work.finish()
    }
}
fn add(left: usize, right: usize) -> AnalysisResult<usize> {
    left.checked_add(right).ok_or_else(overflow)
}
fn multiply(left: usize, right: usize) -> AnalysisResult<usize> {
    left.checked_mul(right).ok_or_else(overflow)
}
fn overflow() -> crate::AnalysisError {
    super::error::invalid("Kuromoji completion", "candidate size overflow").into()
}

#[cfg(test)]
mod tests;
