//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Reserved ordered UTF-16 stop sets use cancellable comparisons and pinned case mapping.

use super::super::{
    error::{check_limit, invalid},
    KuromojiDictionary, KuromojiLimits,
};
use crate::morphology::filter::Work;
use crate::AnalysisResult;
use std::cmp::Ordering;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget},
    ordering::sort_by_with_control,
};

#[derive(Debug)]
pub(in crate::kuromoji) struct PreparedWords {
    words: Vec<Vec<u16>>,
}
impl PreparedWords {
    pub fn new(
        words: &[String],
        ignore_case: bool,
        model: Option<&KuromojiDictionary>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<Budgeted<Self>> {
        check_limit(
            "Kuromoji filter entries",
            words.len(),
            limits.max_filter_entries,
        )?;
        let mut output = BudgetedVec::new(budget);
        output.reserve(words.len())?;
        let mut memory = budget.empty_reservation();
        let mut total = 0_usize;
        for word in words {
            poll()?;
            let count = crate::allocation::input::utf16_len(word, poll, |count| {
                let needed = total
                    .checked_add(count)
                    .ok_or_else(|| invalid("Kuromoji filter", "prepared UTF-16 count overflow"))?;
                check_limit(
                    "Kuromoji filter UTF-16 units",
                    needed,
                    limits.max_filter_utf16,
                )
                .map_err(Into::into)
            })?;
            total += count;
            let (mut value, allocation) =
                crate::allocation::input::encode(word, budget, poll, |_| Ok(()))?.into_parts();
            if ignore_case {
                lowercase(&mut value, super::dictionary(model)?, &mut Work::new(poll)?)?;
            }
            output.push(value)?;
            memory.absorb(allocation);
        }
        sort_by_with_control(&mut output, poll, |a, b, poll| compare(a, b, poll))?;
        let mut retained = 0;
        for index in 0..output.len() {
            poll()?;
            if retained > 0
                && compare(&output[retained - 1], &output[index], poll)? == Ordering::Equal
            {
                let duplicate = std::mem::take(&mut output[index]);
                let bytes = duplicate.capacity() * size_of::<u16>();
                drop(duplicate);
                drop(memory.split(bytes));
            } else {
                output.swap(retained, index);
                retained += 1;
            }
        }
        output.truncate(retained);
        let (words, allocation) = output.into_parts();
        memory.absorb(allocation);
        Ok(Budgeted::new(Self { words }, memory))
    }

    pub fn contains(
        &self,
        term: &[u16],
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<bool> {
        let (mut left, mut right) = (0, self.words.len());
        while left < right {
            poll()?;
            let middle = left + (right - left) / 2;
            match compare(&self.words[middle], term, poll)? {
                Ordering::Less => left = middle + 1,
                Ordering::Greater => right = middle,
                Ordering::Equal => return Ok(true),
            }
        }
        Ok(false)
    }
}
fn compare(
    left: &[u16],
    right: &[u16],
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<Ordering> {
    for (left, right) in left.chunks(1024).zip(right.chunks(1024)) {
        poll()?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}
pub(in crate::kuromoji) fn lowercase(
    input: &mut [u16],
    model: &KuromojiDictionary,
    work: &mut Work<'_>,
) -> AnalysisResult<()> {
    crate::morphology::filter::lowercase::apply(input, &model.unicode, work, || {
        invalid("Kuromoji lowercase", "simple mapping changes UTF-16 width").into()
    })
}
