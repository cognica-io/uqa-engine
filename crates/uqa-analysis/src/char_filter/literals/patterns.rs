//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Combine literal rules whose leftmost matches have the same priority as ordered replacement.

use std::collections::{HashMap, HashSet};

use crate::{work::Work, AnalysisError, AnalysisResult};

use super::index::PrefixIndex;

#[derive(Debug)]
pub(super) enum Stage {
    Literal(String, String),
    Scalars(HashMap<char, String>),
    Patterns(Box<PrefixIndex>),
}

#[derive(Default)]
pub(super) struct Group {
    rules: Vec<(String, String)>,
    starts: HashSet<char>,
    produced: HashSet<char>,
    deletes: bool,
    compounds: bool,
}

impl Group {
    pub(super) async fn add(
        &mut self,
        key: String,
        value: String,
        stages: &mut Vec<Stage>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<()> {
        let first = key.chars().next().expect("nonempty mapping");
        let compound = key.len() != first.len_utf8();
        let mut barrier = self.deletes && compound;
        // A higher-priority key starting inside this key must be applied first.
        // A match that consumes previous output or crosses a deletion needs the
        // transformed source map. All other matches can be emitted left to right.
        for (index, c) in key.chars().enumerate() {
            work.records(index).await?;
            if self.produced.contains(&c) || index != 0 && self.starts.contains(&c) {
                barrier = true;
                break;
            }
        }
        if barrier {
            self.finish(stages, work).await?;
        }
        for (index, c) in value.chars().enumerate() {
            work.records(index).await?;
            self.produced.insert(c);
        }
        self.starts.insert(first);
        self.compounds |= compound;
        self.deletes |= value.is_empty();
        self.rules.push((key, value));
        Ok(())
    }

    pub(super) async fn finish(
        &mut self,
        stages: &mut Vec<Stage>,
        work: &mut impl Work<AnalysisError>,
    ) -> AnalysisResult<()> {
        if self.rules.is_empty() {
            return Ok(());
        }
        work.checkpoint().await?;
        let group = std::mem::take(self);
        if !group.compounds {
            let mut values = HashMap::with_capacity(group.rules.len());
            for (index, (key, value)) in group.rules.into_iter().enumerate() {
                work.records(index).await?;
                values.insert(key.chars().next().expect("single scalar"), value);
            }
            stages.push(Stage::Scalars(values));
        } else if group.rules.len() == 1 {
            let (key, value) = group.rules.into_iter().next().expect("one rule");
            stages.push(Stage::Literal(key, value));
        } else {
            stages.push(Stage::Patterns(Box::new(
                PrefixIndex::prepare(group.rules, work).await?,
            )));
        }
        Ok(())
    }
}
