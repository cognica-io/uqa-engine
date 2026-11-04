//
// Unified Query Algebra
//
// Copyright (c) 2023-2026 Cognica, Inc.
//

//! Native object descriptions retain immutable payloads and cached complete footprints.

use super::CommentAddress;
use std::ops::{Bound, RangeInclusive};
use uqa_core::{
    memory::{
        Budgeted, MemoryBudget, Produced, ProductionControl, RetainedMap, RetainedMapIter,
        RetainedMapSnapshot,
    },
    CancellationToken,
};
use uqa_sql::SQLError;

#[derive(Clone)]
pub struct CommentRegistry {
    values: RetainedMap<CommentAddress, Budgeted<String>>,
}

impl Default for CommentRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CommentRegistry {
    pub fn new() -> Self {
        Self::with_budget(&MemoryBudget::new(usize::MAX))
    }

    pub fn with_budget(memory: &MemoryBudget) -> Self {
        Self {
            values: RetainedMap::new(memory),
        }
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.values.budget()
    }

    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, address: &CommentAddress) -> Option<&String> {
        self.values.get(address).map(|text| &**text)
    }

    pub fn iter(&self) -> CommentIter<'_> {
        CommentIter(self.values.iter())
    }

    pub fn keys(&self) -> impl Iterator<Item = &CommentAddress> {
        self.iter().map(|(address, _)| address)
    }

    pub fn range(
        &self,
        range: RangeInclusive<CommentAddress>,
    ) -> impl Iterator<Item = (&CommentAddress, &String)> {
        let end = *range.end();
        CommentIter(self.values.range_from(Bound::Included(range.start())))
            .take_while(move |(address, _)| **address <= end)
    }

    /// Admit the selected text and changed tree path before replacing this root.
    pub fn insert(&mut self, address: CommentAddress, text: &str) -> Result<(), SQLError> {
        let cancellation = CancellationToken::new();
        let control = ProductionControl::new(self.budget(), &cancellation, &cancellation);
        let text = control
            .copy_text(text)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native comments use admitted construction"));
        let bytes = text.reserved_bytes();
        self.values.insert(address, text, bytes)?;
        Ok(())
    }

    pub fn remove(&mut self, address: &CommentAddress) -> Result<bool, SQLError> {
        Ok(self.values.remove(address)?)
    }

    pub fn try_from_iter(
        values: impl IntoIterator<Item = (CommentAddress, String)>,
    ) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (address, text) in values {
            result.insert(address, &text)?;
        }
        Ok(result)
    }

    /// Independently admit the cached complete footprint without copying or visiting entries.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedComments>, SQLError> {
        control.check()?;
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("comment capture requires an admitted control".into())
        })?;
        let (values, memory) = self.values.capture(memory)?.into_parts();
        Ok(control.finish(CapturedComments { values }, Some(memory))?)
    }
}

impl std::fmt::Debug for CommentRegistry {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

impl PartialEq for CommentRegistry {
    fn eq(&self, other: &Self) -> bool {
        self.len() == other.len() && self.iter().eq(other.iter())
    }
}

impl Eq for CommentRegistry {}

impl std::ops::Index<&CommentAddress> for CommentRegistry {
    type Output = String;

    fn index(&self, address: &CommentAddress) -> &String {
        self.get(address).expect("catalog comment address exists")
    }
}

pub struct CommentIter<'a>(RetainedMapIter<'a, CommentAddress, Budgeted<String>>);

impl<'a> Iterator for CommentIter<'a> {
    type Item = (&'a CommentAddress, &'a String);

    fn next(&mut self) -> Option<Self::Item> {
        self.0.next().map(|(address, text)| (address, &**text))
    }
}

impl<'a> IntoIterator for &'a CommentRegistry {
    type Item = (&'a CommentAddress, &'a String);
    type IntoIter = CommentIter<'a>;

    fn into_iter(self) -> Self::IntoIter {
        self.iter()
    }
}

/// An immutable admitted root; its enclosing production lease funds every retained entry.
pub struct CapturedComments {
    values: RetainedMapSnapshot<CommentAddress, Budgeted<String>>,
}

impl CapturedComments {
    pub fn len(&self) -> usize {
        self.values.len()
    }

    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, address: &CommentAddress) -> Option<&String> {
        self.values.get(address).map(|text| &**text)
    }

    pub fn iter(&self) -> CommentIter<'_> {
        CommentIter(self.values.iter())
    }
}

impl std::fmt::Debug for CapturedComments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests;
