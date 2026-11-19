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
use crate::{
    work::progress::{Callback, Counter, Progress},
    AnalysisError, AnalysisResult,
};
use std::cmp::Ordering;
use uqa_core::{
    memory::{Budgeted, BudgetedVec, MemoryBudget, MemoryReservation},
    ordering::{sort_by_with_progress, SortProgress},
};

#[derive(Debug)]
pub(in crate::kuromoji) struct PreparedWords {
    words: Vec<Vec<u16>>,
}
struct Preparation {
    words: BudgetedVec<Vec<u16>>,
    memory: MemoryReservation,
}

impl PreparedWords {
    pub(crate) async fn new_with(
        words: &[String],
        ignore_case: bool,
        model: Option<&KuromojiDictionary>,
        limits: KuromojiLimits,
        budget: &MemoryBudget,
        progress: &mut impl Progress<AnalysisError>,
    ) -> AnalysisResult<Budgeted<Self>> {
        check_limit(
            "Kuromoji filter entries",
            words.len(),
            limits.max_filter_entries,
        )?;
        let mut prepared = Preparation {
            words: BudgetedVec::new(budget),
            memory: budget.empty_reservation(),
        };
        prepared.words.reserve(words.len())?;
        let mut total = 0_usize;
        for word in words {
            progress.checkpoint().await?;
            let count = crate::allocation::input::utf16_len_with(word, progress, |count| {
                let needed = total
                    .checked_add(count)
                    .ok_or_else(|| invalid("Kuromoji filter", "prepared UTF-16 count overflow"))?;
                check_limit(
                    "Kuromoji filter UTF-16 units",
                    needed,
                    limits.max_filter_utf16,
                )
                .map_err(Into::into)
            })
            .await?;
            total += count;
            let value =
                crate::allocation::input::encode_with(word, budget, progress, |_| Ok(())).await?;
            let next = &mut *progress;
            let value = value
                .try_map_async(|mut value| async move {
                    if ignore_case {
                        let model = super::dictionary(model)?;
                        let mut counter = Counter::new(next).await?;
                        crate::morphology::filter::lowercase::apply_with(
                            &mut value,
                            &model.unicode,
                            &mut counter,
                            || {
                                invalid("Kuromoji lowercase", "simple mapping changes UTF-16 width")
                                    .into()
                            },
                        )
                        .await?;
                    }
                    Ok::<_, AnalysisError>(value)
                })
                .await?;
            let (value, allocation) = value.into_parts();
            prepared.words.push(value)?;
            prepared.memory.absorb(allocation);
        }
        sort_by_with_progress(&mut prepared.words, &mut OrderingProgress(progress)).await?;
        let mut retained = 0;
        for index in 0..prepared.words.len() {
            progress.checkpoint().await?;
            if retained > 0
                && compare_with(
                    &prepared.words[retained - 1],
                    &prepared.words[index],
                    progress,
                )
                .await?
                    == Ordering::Equal
            {
                let duplicate = std::mem::take(&mut prepared.words[index]);
                let bytes = duplicate.capacity() * size_of::<u16>();
                drop(duplicate);
                drop(prepared.memory.split(bytes));
            } else {
                prepared.words.swap(retained, index);
                retained += 1;
            }
        }
        prepared.words.truncate(retained);
        let (words, allocation) = prepared.words.into_parts();
        prepared.memory.absorb(allocation);
        Ok(Budgeted::new(Self { words }, prepared.memory))
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
    crate::work::inline(compare_with(left, right, &mut Callback(poll)))
}

async fn compare_with(
    left: &[u16],
    right: &[u16],
    progress: &mut impl Progress<AnalysisError>,
) -> AnalysisResult<Ordering> {
    for (left, right) in left.chunks(1024).zip(right.chunks(1024)) {
        progress.checkpoint().await?;
        let order = left.cmp(right);
        if order != Ordering::Equal {
            return Ok(order);
        }
    }
    Ok(left.len().cmp(&right.len()))
}

struct OrderingProgress<'a, P>(&'a mut P);

impl<P: Progress<AnalysisError>> SortProgress<Vec<u16>, AnalysisError> for OrderingProgress<'_, P> {
    async fn checkpoint(&mut self) -> AnalysisResult<()> {
        self.0.checkpoint().await
    }
    async fn compare(&mut self, left: &Vec<u16>, right: &Vec<u16>) -> AnalysisResult<Ordering> {
        compare_with(left, right, self.0).await
    }
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
