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
use uqa_sql::{ast::RelationPersistence, SQLError};

mod image;

#[derive(Clone)]
pub struct CommentRegistry {
    durable: RetainedMap<CommentAddress, Budgeted<String>>,
    temporary: RetainedMap<CommentAddress, Budgeted<String>>,
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
            durable: RetainedMap::new(memory),
            temporary: RetainedMap::new(memory),
        }
    }

    pub fn budget(&self) -> &MemoryBudget {
        self.durable.budget()
    }

    pub fn len(&self) -> usize {
        self.durable.len() + self.temporary.len()
    }

    pub fn is_empty(&self) -> bool {
        self.durable.is_empty() && self.temporary.is_empty()
    }

    pub fn get(&self, address: &CommentAddress) -> Option<&String> {
        self.durable
            .get(address)
            .or_else(|| self.temporary.get(address))
            .map(|text| &**text)
    }

    pub fn iter(&self) -> CommentIter<'_> {
        CommentIter::new(self.durable.iter(), self.temporary.iter())
    }

    pub fn keys(&self) -> impl Iterator<Item = &CommentAddress> {
        self.iter().map(|(address, _)| address)
    }

    pub(super) fn durable_iter(&self) -> impl Iterator<Item = (&CommentAddress, &String)> {
        self.durable.iter().map(|(key, text)| (key, &**text))
    }

    pub(super) fn durable_get(&self, address: &CommentAddress) -> Option<&String> {
        self.durable.get(address).map(|text| &**text)
    }

    pub fn range(
        &self,
        range: RangeInclusive<CommentAddress>,
    ) -> impl Iterator<Item = (&CommentAddress, &String)> {
        let end = *range.end();
        CommentIter::new(
            self.durable.range_from(Bound::Included(range.start())),
            self.temporary.range_from(Bound::Included(range.start())),
        )
        .take_while(move |(address, _)| **address <= end)
    }

    /// Admit the selected text and changed tree path before replacing this root.
    pub fn insert(
        &mut self,
        address: CommentAddress,
        text: &str,
        persistence: RelationPersistence,
    ) -> Result<(), SQLError> {
        let cancellation = CancellationToken::new();
        let memory = if persistence == RelationPersistence::Temporary {
            self.temporary.budget()
        } else {
            self.durable.budget()
        };
        let control = ProductionControl::new(memory, &cancellation, &cancellation);
        let text = control
            .copy_text(text)?
            .into_budgeted()
            .unwrap_or_else(|_| unreachable!("native comments use admitted construction"));
        let bytes = text.reserved_bytes();
        let mut candidate = self.clone();
        if persistence == RelationPersistence::Temporary {
            candidate.durable.remove(&address)?;
            candidate.temporary.insert(address, text, bytes)?;
        } else {
            candidate.temporary.remove(&address)?;
            candidate.durable.insert(address, text, bytes)?;
        }
        *self = candidate;
        Ok(())
    }

    pub fn remove(&mut self, address: &CommentAddress) -> Result<bool, SQLError> {
        if self.durable.get(address).is_some() {
            Ok(self.durable.remove(address)?)
        } else {
            Ok(self.temporary.remove(address)?)
        }
    }

    pub fn is_temporary(&self, address: &CommentAddress) -> bool {
        self.temporary.get(address).is_some()
    }

    /// Select the original durable root without retaining temporary descriptions or consulting object catalogs.
    pub(super) fn durable_generation(&self) -> Self {
        Self {
            durable: self.durable.clone(),
            temporary: RetainedMap::new(self.budget()),
        }
    }

    /// A physical catalog refresh changes durable descriptions while preserving the same session's temporary generation.
    pub(super) fn with_temporary_from(&self, session: &Self) -> Self {
        Self {
            durable: self.durable.clone(),
            temporary: session.temporary.clone(),
        }
    }

    pub fn try_from_iter(
        values: impl IntoIterator<Item = (CommentAddress, String, RelationPersistence)>,
    ) -> Result<Self, SQLError> {
        let mut result = Self::new();
        for (address, text, persistence) in values {
            result.insert(address, &text, persistence)?;
        }
        Ok(result)
    }

    /// Independently admit the cached durable footprint without copying or visiting entries. Temporary descriptions belong to their session and are not retained.
    pub fn capture(
        &self,
        control: &ProductionControl<'_>,
    ) -> Result<Produced<CapturedComments>, SQLError> {
        control.check()?;
        let memory = control.budget().ok_or_else(|| {
            SQLError::Internal("comment capture requires an admitted control".into())
        })?;
        let (values, memory) = self.durable.capture(memory)?.into_parts();
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
        self.durable.len() == other.durable.len()
            && self.temporary.len() == other.temporary.len()
            && self
                .durable
                .iter()
                .map(|(key, text)| (key, &**text))
                .eq(other.durable.iter().map(|(key, text)| (key, &**text)))
            && self
                .temporary
                .iter()
                .map(|(key, text)| (key, &**text))
                .eq(other.temporary.iter().map(|(key, text)| (key, &**text)))
    }
}

impl Eq for CommentRegistry {}

impl std::ops::Index<&CommentAddress> for CommentRegistry {
    type Output = String;

    fn index(&self, address: &CommentAddress) -> &String {
        self.get(address).expect("catalog comment address exists")
    }
}

pub struct CommentIter<'a> {
    durable: std::iter::Peekable<RetainedMapIter<'a, CommentAddress, Budgeted<String>>>,
    temporary: std::iter::Peekable<RetainedMapIter<'a, CommentAddress, Budgeted<String>>>,
}

impl<'a> CommentIter<'a> {
    fn new(
        durable: RetainedMapIter<'a, CommentAddress, Budgeted<String>>,
        temporary: RetainedMapIter<'a, CommentAddress, Budgeted<String>>,
    ) -> Self {
        Self {
            durable: durable.peekable(),
            temporary: temporary.peekable(),
        }
    }
}

impl<'a> Iterator for CommentIter<'a> {
    type Item = (&'a CommentAddress, &'a String);

    fn next(&mut self) -> Option<Self::Item> {
        let selected = match (self.durable.peek(), self.temporary.peek()) {
            (Some((a, _)), Some((b, _))) if a > b => self.temporary.next(),
            (Some(_), _) => self.durable.next(),
            (None, _) => self.temporary.next(),
        };
        selected.map(|(address, text)| (address, &**text))
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

    pub fn iter(&self) -> impl Iterator<Item = (&CommentAddress, &String)> {
        self.values.iter().map(|(key, text)| (key, &**text))
    }
}

impl std::fmt::Debug for CapturedComments {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_map().entries(self.iter()).finish()
    }
}

#[cfg(test)]
mod tests;
