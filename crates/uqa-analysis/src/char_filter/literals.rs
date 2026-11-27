//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Ordered literal edits. Independent rules share one source-map pass.

use super::{mapping_longest_first, stream::edit_literal, HTML_ENTITIES};
use crate::{source::EditBuilder, work::Work, AnalysisError, AnalysisResult, FilteredText};
use std::collections::BTreeMap;

mod index;
mod patterns;
use patterns::{Group, Stage};
use uqa_core::memory::MemoryBudget;

pub(super) fn replace_entities(
    text: &mut FilteredText<'_>,
    budget: &MemoryBudget,
    poll: &mut dyn FnMut() -> AnalysisResult<()>,
) -> AnalysisResult<()> {
    // Ampersands decode first. An ampersand produced by this step can start
    // one of the later entities, including in "&amp;lt;".
    let (&(amp, amp_value), other) = HTML_ENTITIES.split_first().expect("ampersand entity");
    let edited = {
        let input = text.as_str();
        let mut builder = EditBuilder::new(input, budget, poll);
        let mut end = 0;
        for (index, byte) in input.bytes().enumerate() {
            if index.is_multiple_of(1024) {
                builder.check()?;
            }
            if byte != b'&' || index < end {
                continue;
            }
            let tail = &input[index..];
            let expanded = tail.strip_prefix(amp);
            let after_amp = expanded.unwrap_or(&tail[1..]);
            let matched = other
                .iter()
                .find(|(entity, _)| after_amp.starts_with(&entity[1..]));
            if let Some(&(entity, replacement)) = matched {
                end = index + tail.len() - after_amp.len() + entity.len() - 1;
                builder.edit(index..end, std::iter::once(replacement))?;
            } else if expanded.is_some() {
                end = index + amp.len();
                builder.edit(index..end, std::iter::once(amp_value))?;
            }
        }
        builder.finish()?
    };
    text.apply_edited(edited, budget, poll)
}

#[derive(Debug)]
pub(crate) struct LiteralMappings(Vec<Stage>);

impl LiteralMappings {
    pub(crate) async fn prepare(
        mapping: &BTreeMap<String, String>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<Self> {
        let mut stages = Vec::new();
        let mut group = Group::default();
        for (index, (key, value)) in mapping_longest_first(mapping, work)
            .await?
            .into_iter()
            .enumerate()
        {
            work.records(index).await?;
            if same_literal(&key, &value, work).await? {
                continue;
            }
            if key.is_empty() {
                group.finish(&mut stages, work).await?;
                stages.push(Stage::Literal(key, value));
            } else {
                group.add(key, value, &mut stages, work).await?;
            }
        }
        group.finish(&mut stages, work).await?;
        Ok(Self(stages))
    }

    pub(crate) fn replace(
        &self,
        text: &mut FilteredText<'_>,
        budget: &MemoryBudget,
        poll: &mut dyn FnMut() -> AnalysisResult<()>,
    ) -> AnalysisResult<()> {
        for stage in &self.0 {
            let edited = {
                let input = text.as_str();
                let mut builder = EditBuilder::new(input, budget, poll);
                match stage {
                    Stage::Scalars(values) => {
                        for (step, (index, c)) in input.char_indices().enumerate() {
                            if step.is_multiple_of(1024) {
                                builder.check()?;
                            }
                            if let Some(value) = values.get(&c) {
                                builder.edit(
                                    index..index + c.len_utf8(),
                                    std::iter::once(value.as_str()),
                                )?;
                            }
                        }
                    }
                    Stage::Patterns(patterns) => patterns.apply(input, &mut builder)?,
                    Stage::Literal(old, new) => edit_literal(input, old, new, &mut builder)?,
                }
                builder.finish()?
            };
            text.apply_edited(edited, budget, poll)?;
        }
        Ok(())
    }
}

async fn same_literal(
    left: &str,
    right: &str,
    work: &mut impl Work<AnalysisError>,
) -> AnalysisResult<bool> {
    if left.len() != right.len() {
        return Ok(false);
    }
    for (left, right) in left
        .as_bytes()
        .chunks(1024)
        .zip(right.as_bytes().chunks(1024))
    {
        work.checkpoint().await?;
        if left != right {
            return Ok(false);
        }
    }
    Ok(true)
}

#[cfg(test)]
mod tests;
